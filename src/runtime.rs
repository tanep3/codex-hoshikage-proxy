mod wire;
use crate::{
    config::ValidatedConfig,
    domain::{RuntimeEvent, RuntimeState, reduce_runtime},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc, Mutex as SyncMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, RwLock, broadcast, oneshot},
};

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("failed to spawn Codex App Server: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("Codex App Server request failed: {0}")]
    Protocol(String),
    #[error("Codex App Server initialization failed: {0}")]
    Initialization(String),
    #[error("Codex App Server is not ready")]
    NotReady,
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<'a> {
    id: Option<Value>,
    #[serde(borrow, default, deserialize_with = "present_result")]
    result: Option<&'a serde_json::value::RawValue>,
    error: Option<JsonRpcError>,
    method: Option<String>,
    #[serde(borrow)]
    params: Option<&'a serde_json::value::RawValue>,
}

// Preserve an explicit JSON null result as a successful response.
fn present_result<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<&'de serde_json::value::RawValue>, D::Error> {
    <&serde_json::value::RawValue>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: Value,
}

struct PendingRequest {
    sender: oneshot::Sender<Result<Value, RuntimeError>>,
    catalog: bool,
}
type Pending = Arc<SyncMutex<HashMap<u64, PendingRequest>>>;
struct PendingGuard {
    pending: Pending,
    id: u64,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}
struct WriteGuard<'a> {
    runtime: &'a CodexRuntime,
    complete: bool,
}
impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.runtime.transport_closed.store(true, Ordering::Release);
            let _ = self
                .runtime
                .notifications
                .send(json!({"kind":"transport_closed"}));
            for (_, request) in self.runtime.pending.lock().unwrap().drain() {
                let _ = request.sender.send(Err(RuntimeError::Protocol(
                    "Codex write interrupted".into(),
                )));
            }
        }
    }
}

#[derive(Default)]
struct PolicyThread {
    active: bool,
    binding: Option<String>,
    in_flight: usize,
}
struct PolicyCall<'a> {
    threads: &'a SyncMutex<HashMap<String, PolicyThread>>,
    thread: String,
}
impl Drop for PolicyCall<'_> {
    fn drop(&mut self) {
        let mut threads = self.threads.lock().unwrap();
        if let Some(entry) = threads.get_mut(&self.thread) {
            entry.in_flight = entry.in_flight.saturating_sub(1);
            if entry.in_flight == 0 && entry.binding.is_none() {
                threads.remove(&self.thread);
            }
        }
    }
}
pub struct CodexRuntime {
    id: String,
    policy_threads: SyncMutex<HashMap<String, PolicyThread>>,
    state: Arc<RwLock<RuntimeState>>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicU64,
    mcp_refresh: Mutex<Option<crate::user_config::McpRefresh>>,
    transport_closed: Arc<AtomicBool>,
    notifications: broadcast::Sender<Value>,
    child: Arc<Mutex<Option<Child>>>,
}

impl CodexRuntime {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub async fn process_id(&self) -> Option<u32> {
        self.child.lock().await.as_ref().and_then(Child::id)
    }
    pub async fn refresh_configuration(&self) -> Result<(), RuntimeError> {
        let mut refresh = self.mcp_refresh.lock().await;
        if let Some(refresh) = refresh.as_mut() {
            if self
                .policy_threads
                .lock()
                .unwrap()
                .values()
                .any(|t| t.binding.is_some() && t.active)
            {
                return if refresh
                    .is_current()
                    .map_err(|_| RuntimeError::Protocol("MCP configuration unavailable".into()))?
                {
                    Ok(())
                } else {
                    Err(RuntimeError::Protocol(
                        "MCP configuration is bound to an active execution".into(),
                    ))
                };
            }
            if let Some(desired) = refresh
                .prepare()
                .map_err(|_| RuntimeError::Protocol("MCP configuration unavailable".into()))?
            {
                self.request_raw("config/mcpServer/reload", Value::Null)
                    .await?;
                refresh.acknowledge(desired);
            }
        }
        Ok(())
    }
    /// Restore a bound thread before allowing global reload or another owner.
    /// It cannot send model input or execute arbitrary methods.
    pub async fn restore_policy_thread(
        &self,
        binding: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, RuntimeError> {
        if !matches!(method, "thread/unsubscribe" | "thread/resume")
            || !params["threadId"].is_string()
            || (method == "thread/resume" && params["config"] != json!({}))
        {
            return Err(RuntimeError::Protocol("invalid policy restoration".into()));
        }
        let _call = self.check_policy_thread(Some(binding), method, &params)?;
        self.request_raw(method, params).await
    }

    pub async fn launch(config: &ValidatedConfig) -> Result<Arc<Self>, RuntimeError> {
        if config.v2_enabled {
            let version = tokio::time::timeout(
                Duration::from_secs(5),
                Command::new(&config.codex_command)
                    .arg("--version")
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| RuntimeError::Initialization("Codex version probe timed out".into()))??;
            if !version.status.success()
                || String::from_utf8_lossy(&version.stdout)
                    .split_whitespace()
                    .nth(1)
                    != Some("0.153.4")
            {
                return Err(RuntimeError::Initialization(
                    "v2 dynamic-tool adapter requires verified Codex CLI 0.153.4".into(),
                ));
            }
        }
        let mcp_refresh = crate::user_config::McpRefresh::new(
            &config.codex_home,
            config.codex_user_home.as_deref(),
        )
        .map_err(|e| RuntimeError::Initialization(e.to_string()))?;
        let mut command = Command::new(&config.codex_command);
        if config.v2_limits.mcp_turn_approval_enabled {
            command
                .arg("-c")
                .arg("features.tool_call_mcp_elicitation=false");
        }
        command
            .kill_on_drop(true)
            .args(&config.codex_args)
            .env("CODEX_HOME", &config.codex_home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RuntimeError::Protocol("Codex stdin was not piped".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RuntimeError::Protocol("Codex stdout was not piped".into()))?;
        let (notifications, _) = broadcast::channel(256);
        let runtime = Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            policy_threads: SyncMutex::new(HashMap::new()),
            state: Arc::new(RwLock::new(RuntimeState::Starting { attempt: 1 })),
            stdin: Arc::new(Mutex::new(stdin)),
            pending: Arc::new(SyncMutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            mcp_refresh: Mutex::new(mcp_refresh),
            transport_closed: Arc::new(AtomicBool::new(false)),
            notifications,
            child: Arc::new(Mutex::new(Some(child))),
        });
        {
            let mut state = runtime.state.write().await;
            *state = reduce_runtime(&state, RuntimeEvent::ProcessSpawned).next;
        }
        runtime.spawn_reader(stdout);
        runtime.spawn_process_monitor();

        if let Err(error) = runtime.initialize(config.v2_enabled).await {
            let mut state = runtime.state.write().await;
            *state = reduce_runtime(
                &state,
                RuntimeEvent::InitializeFailed {
                    message: error.to_string(),
                },
            )
            .next;
            drop(state);
            let _ = runtime.shutdown().await;
            return Err(error);
        }
        Ok(runtime)
    }

    async fn initialize(&self, experimental: bool) -> Result<(), RuntimeError> {
        let params = json!({
            "clientInfo": {
                "name": "codex-hoshikage-proxy",
                "title": "Codex Hoshikage Proxy",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {"experimentalApi":experimental}
        });
        {
            let mut state = self.state.write().await;
            *state = RuntimeState::Initializing;
        }
        self.request("initialize", params)
            .await
            .map_err(|error| RuntimeError::Initialization(error.to_string()))?;
        self.notify("initialized", json!({})).await?;
        let mut state = self.state.write().await;
        *state = reduce_runtime(&state, RuntimeEvent::InitializeSucceeded).next;
        Ok(())
    }

    /// Scoped execution owns this thread until its settings have been cleared.
    /// The v2 store owner must first exclude active/unknown previous executions.
    pub async fn bind_policy_thread(
        &self,
        thread: &str,
        binding: &str,
    ) -> Result<(), RuntimeError> {
        let refresh = self.mcp_refresh.lock().await;
        if let Some(refresh) = refresh.as_ref()
            && !refresh
                .is_current()
                .map_err(|_| RuntimeError::Protocol("MCP configuration unavailable".into()))?
        {
            return Err(RuntimeError::Protocol(
                "MCP configuration must be refreshed before policy binding".into(),
            ));
        }
        let mut threads = self.policy_threads.lock().unwrap();
        let entry = threads.entry(thread.into()).or_default();
        if entry.binding.as_deref() == Some(binding) {
            entry.active = true;
            return Ok(());
        }
        if entry.binding.is_some() || entry.in_flight != 0 {
            return Err(RuntimeError::Protocol(
                "thread policy binding is busy".into(),
            ));
        }
        entry.binding = Some(binding.into());
        entry.active = true;
        Ok(())
    }
    pub fn finish_policy_binding(&self, binding: &str) {
        for entry in self.policy_threads.lock().unwrap().values_mut() {
            if entry.binding.as_deref() == Some(binding) {
                entry.active = false;
            }
        }
    }
    pub fn release_policy_thread(&self, thread: &str, binding: &str) -> Result<(), RuntimeError> {
        let mut threads = self.policy_threads.lock().unwrap();
        if let Some(entry) = threads.get(thread) {
            if entry.binding.as_deref() != Some(binding) || entry.in_flight != 0 {
                return Err(RuntimeError::Protocol(
                    "thread policy binding is busy".into(),
                ));
            }
            threads.remove(thread);
        }
        Ok(())
    }
    pub async fn request_scoped(
        &self,
        binding: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, RuntimeError> {
        let _call = self.check_policy_thread(Some(binding), method, &params)?;
        self.request_checked(method, params).await
    }
    fn check_policy_thread(
        &self,
        binding: Option<&str>,
        method: &str,
        params: &Value,
    ) -> Result<Option<PolicyCall<'_>>, RuntimeError> {
        if matches!(
            method,
            "thread/read"
                | "thread/items/list"
                | "thread/turns/list"
                | "mcpServerStatus/list"
                | "turn/interrupt"
        ) {
            return Ok(None);
        }
        let Some(thread) = params["threadId"].as_str() else {
            return Ok(None);
        };
        let mut threads = self.policy_threads.lock().unwrap();
        let entry = threads.entry(thread.into()).or_default();
        if entry.binding.is_some() && entry.binding.as_deref() != binding {
            return Err(RuntimeError::Protocol(
                "thread is owned by an approval policy binding".into(),
            ));
        }
        if binding.is_some() && entry.binding.as_deref() != binding {
            if entry.in_flight == 0 {
                threads.remove(thread);
            }
            return Err(RuntimeError::Protocol(
                "thread policy binding not registered".into(),
            ));
        }
        entry.in_flight += 1;
        Ok(Some(PolicyCall {
            threads: &self.policy_threads,
            thread: thread.into(),
        }))
    }
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        let _call = self.check_policy_thread(None, method, &params)?;
        self.request_checked(method, params).await
    }
    async fn request_checked(&self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        if matches!(
            method,
            "thread/start" | "thread/resume" | "turn/start" | "config/mcpServer/reload"
        ) {
            // Serialize refresh acknowledgement with execution boundaries. Never
            // restart the process or replace a conversation to refresh tools.
            let mut refresh = self.mcp_refresh.lock().await;
            let bound = self
                .policy_threads
                .lock()
                .unwrap()
                .values()
                .any(|entry| entry.binding.is_some() && entry.active);
            if bound && method == "config/mcpServer/reload" {
                return Err(RuntimeError::Protocol(
                    "MCP reload is blocked by an active approval policy binding".into(),
                ));
            }
            if let Some(refresh) = refresh.as_mut() {
                if bound {
                    if !refresh.is_current().map_err(|_| {
                        RuntimeError::Protocol("MCP configuration unavailable".into())
                    })? {
                        return Err(RuntimeError::Protocol(
                            "MCP configuration changed during an approval policy binding".into(),
                        ));
                    }
                    return self.request_raw(method, params).await;
                }
                let desired = refresh.prepare().map_err(|e| {
                    RuntimeError::Protocol(format!("MCP configuration refresh failed: {e}"))
                })?;
                if let Some(desired) = desired {
                    self.request_raw("config/mcpServer/reload", Value::Null)
                        .await?;
                    refresh.acknowledge(desired);
                    tracing::info!("MCP configuration reloaded for subsequent turns");
                }
            }
            return self.request_raw(method, params).await;
        }
        self.request_raw(method, params).await
    }

    async fn request_raw(&self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if self.transport_closed.load(Ordering::Acquire) {
                return Err(RuntimeError::NotReady);
            }
            pending.insert(
                id,
                PendingRequest {
                    sender,
                    catalog: method == "mcpServerStatus/list",
                },
            );
        }
        let _registration = PendingGuard {
            pending: Arc::clone(&self.pending),
            id,
        };
        let message = serde_json::to_vec(&JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })
        .map_err(|error| RuntimeError::Protocol(error.to_string()))?;
        if let Err(error) = self.write_line(&message).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(error);
        }
        let result = match tokio::time::timeout(Duration::from_secs(30), receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RuntimeError::Protocol("dispatcher closed".into())),
            Err(_) => Err(RuntimeError::Protocol(format!(
                "request timed out: {method}"
            ))),
        };
        if result.is_err() {
            self.pending.lock().unwrap().remove(&id);
        }
        result
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), RuntimeError> {
        let message = serde_json::to_vec(&JsonRpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        })
        .map_err(|error| RuntimeError::Protocol(error.to_string()))?;
        self.write_line(&message).await
    }

    async fn write_line(&self, message: &[u8]) -> Result<(), RuntimeError> {
        let mut stdin = self.stdin.lock().await;
        if self.transport_closed.load(Ordering::Acquire) {
            return Err(RuntimeError::NotReady);
        }
        let mut write_guard = WriteGuard {
            runtime: self,
            complete: false,
        };
        stdin.write_all(message).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        write_guard.complete = true;
        Ok(())
    }

    fn spawn_reader(self: &Arc<Self>, stdout: tokio::process::ChildStdout) {
        let pending = Arc::clone(&self.pending);
        let pending_for_exit = Arc::clone(&pending);
        let transport_closed = Arc::clone(&self.transport_closed);
        let notifications = self.notifications.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(line)) = wire::frame(&mut reader, wire::MAX_FRAME_BYTES).await {
                let parsed = match serde_json::from_slice::<JsonRpcResponse>(&line) {
                    Ok(message) => message,
                    Err(_) => {
                        let _ = notifications.send(json!({"kind":"protocol_error"}));
                        continue;
                    }
                };
                let mut params = parsed
                    .params
                    .and_then(|raw| serde_json::from_str::<Value>(raw.get()).ok())
                    .unwrap_or_else(|| json!({}));
                if parsed.method.as_deref() == Some("item/started")
                    && params["item"]["type"] == "mcpToolCall"
                {
                    match parsed
                        .params
                        .and_then(|raw| wire::exact_json(raw.get()).ok())
                    {
                        Some(exact) => params = exact,
                        None => {
                            params["item"]["arguments"] = Value::Null;
                            params["item"]["_proxy_argument_integrity"] =
                                json!("arguments_invalid");
                        }
                    }
                }
                if let Some(id) = parsed.id {
                    // Server requests have their own ID namespace. Dispatch by
                    // method first, even if a client request has the same ID.
                    if let Some(method) = parsed.method {
                        let _ = notifications.send(json!({
                            "kind": "server_request",
                            "rpc_id": id,
                            "method": method,
                            "params": params,
                        }));
                        continue;
                    }
                    let Some(id) = id.as_u64() else { continue };
                    let request = pending.lock().unwrap().remove(&id);
                    if let Some(request) = request {
                        let result = match (parsed.result, parsed.error) {
                            (Some(value), _) => {
                                if request.catalog {
                                    wire::catalog(value.get())
                                        .map_err(|code| RuntimeError::Protocol(code.into()))
                                } else {
                                    serde_json::from_str(value.get())
                                        .map_err(|error| RuntimeError::Protocol(error.to_string()))
                                }
                            }
                            (_, Some(error)) => Err(RuntimeError::Protocol(format!(
                                "{} ({})",
                                error.message, error.code
                            ))),
                            _ => Err(RuntimeError::Protocol(
                                "response has neither result nor error".into(),
                            )),
                        };
                        let _ = request.sender.send(result);
                    }
                } else {
                    let _ = notifications.send(json!({
                        "method": parsed.method,
                        "params": params,
                    }));
                }
            }
            transport_closed.store(true, Ordering::Release);
            let _ = notifications.send(json!({"kind": "transport_closed"}));
            let mut pending = pending_for_exit.lock().unwrap();
            for (_, request) in pending.drain() {
                let _ = request.sender.send(Err(RuntimeError::Protocol(
                    "Codex App Server transport closed".into(),
                )));
            }
        });
    }

    pub async fn respond_to_server_request(
        &self,
        rpc_id: impl Into<Value>,
        result: Value,
    ) -> Result<(), RuntimeError> {
        let message = json!({
            "jsonrpc": "2.0",
            "id": rpc_id.into(),
            "result": result,
        });
        let bytes = serde_json::to_vec(&message)
            .map_err(|error| RuntimeError::Protocol(error.to_string()))?;
        self.write_line(&bytes).await
    }

    pub async fn reject_server_request(&self, rpc_id: Value) -> Result<(), RuntimeError> {
        let bytes = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": rpc_id,
            "error": {"code": -32601, "message": "Proxy client does not support this server request"}
        })).map_err(|error| RuntimeError::Protocol(error.to_string()))?;
        self.write_line(&bytes).await
    }

    fn spawn_process_monitor(self: &Arc<Self>) {
        let child = Arc::clone(&self.child);
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let status = loop {
                let status = {
                    let mut guard = child.lock().await;
                    let Some(process) = guard.as_mut() else {
                        return;
                    };
                    match process.try_wait() {
                        Ok(Some(status)) => {
                            let _ = guard.take();
                            Some(status)
                        }
                        Ok(None) => None,
                        Err(error) => {
                            tracing::warn!(error = %error, "failed to poll Codex App Server process");
                            return;
                        }
                    }
                };
                if status.is_some() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            let mut current = state.write().await;
            *current = reduce_runtime(
                &current,
                RuntimeEvent::ProcessExited {
                    code: status.and_then(|s| s.code()),
                },
            )
            .next;
        });
    }

    /// A closed transport is fatal even if the process has not exited yet.
    pub async fn wait_for_failure(&self) {
        loop {
            if self.transport_closed.load(Ordering::Acquire)
                || matches!(
                    self.snapshot().await,
                    RuntimeState::Recovering { .. } | RuntimeState::Failed { .. }
                )
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn snapshot(&self) -> RuntimeState {
        self.state.read().await.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.notifications.subscribe()
    }

    pub fn publish(&self, event: Value) {
        let _ = self.notifications.send(event);
    }

    pub async fn wait_for_notification(
        &self,
        method: &str,
        timeout: Duration,
    ) -> Result<Value, RuntimeError> {
        let mut receiver = self.subscribe();
        tokio::time::timeout(timeout, async move {
            loop {
                let value = receiver
                    .recv()
                    .await
                    .map_err(|error| RuntimeError::Protocol(error.to_string()))?;
                if value.get("method").and_then(Value::as_str) == Some(method) {
                    return Ok(value.get("params").cloned().unwrap_or_else(|| json!({})));
                }
            }
        })
        .await
        .map_err(|_| RuntimeError::Protocol(format!("notification timed out: {method}")))?
    }

    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        {
            let mut state = self.state.write().await;
            *state = reduce_runtime(&state, RuntimeEvent::ShutdownRequested).next;
        }
        let process = self.child.lock().await.take();
        if let Some(mut process) = process {
            process.kill().await?;
            process.wait().await?;
        }
        let mut state = self.state.write().await;
        *state = reduce_runtime(&state, RuntimeEvent::ShutdownCompleted).next;
        Ok(())
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    // Echo process with an unread output pipe: small writes finish, large writes
    // block. No external Codex, configuration, or production service is touched.
    pub(super) fn runtime() -> Arc<CodexRuntime> {
        let mut child = Command::new("cat")
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let (notifications, _) = broadcast::channel(16);
        Arc::new(CodexRuntime {
            id: uuid::Uuid::new_v4().to_string(),
            policy_threads: SyncMutex::new(HashMap::new()),
            state: Arc::new(RwLock::new(RuntimeState::Ready)),
            stdin: Arc::new(Mutex::new(stdin)),
            pending: Arc::new(SyncMutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            mcp_refresh: Mutex::new(None),
            transport_closed: Arc::new(AtomicBool::new(false)),
            notifications,
            child: Arc::new(Mutex::new(Some(child))),
        })
    }

    #[tokio::test]
    async fn cancelling_waiting_rpc_reclaims_registration_without_closing_transport() {
        let runtime = runtime();
        for _ in 0..20 {
            assert!(
                tokio::time::timeout(Duration::from_millis(5), runtime.request("test", json!({})))
                    .await
                    .is_err()
            );
            assert!(runtime.pending.lock().unwrap().is_empty());
            assert!(!runtime.transport_closed.load(Ordering::Acquire));
        }
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_incomplete_write_fences_transport_and_drains_other_requests() {
        let runtime = runtime();
        let (sender, receiver) = oneshot::channel();
        runtime.pending.lock().unwrap().insert(
            100,
            PendingRequest {
                sender,
                catalog: false,
            },
        );
        let message = vec![b'x'; 1024 * 1024];
        assert!(
            tokio::time::timeout(Duration::from_millis(50), runtime.write_line(&message))
                .await
                .is_err()
        );
        assert!(runtime.transport_closed.load(Ordering::Acquire));
        assert!(runtime.pending.lock().unwrap().is_empty());
        assert!(receiver.await.unwrap().is_err());
        assert!(matches!(
            runtime.request("test", json!({})).await,
            Err(RuntimeError::NotReady)
        ));
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_before_stdin_lock_does_not_fence_transport() {
        let runtime = runtime();
        let lock = runtime.stdin.lock().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(5), runtime.request("test", json!({})))
                .await
                .is_err()
        );
        assert!(runtime.pending.lock().unwrap().is_empty());
        assert!(!runtime.transport_closed.load(Ordering::Acquire));
        drop(lock);
        runtime.shutdown().await.unwrap();
    }
}

#[cfg(test)]
mod policy_boundary_tests {
    use super::*;
    #[tokio::test]
    async fn active_legacy_rpc_prevents_policy_rebinding_and_binding_prevents_unscoped_writes() {
        let runtime = super::cancellation_tests::runtime();
        let params = json!({"threadId":"thread"});
        let legacy = runtime
            .check_policy_thread(None, "thread/resume", &params)
            .unwrap();
        assert!(
            runtime
                .bind_policy_thread("thread", "binding")
                .await
                .is_err()
        );
        drop(legacy);
        runtime
            .bind_policy_thread("thread", "binding")
            .await
            .unwrap();
        assert!(
            runtime
                .check_policy_thread(None, "turn/start", &params)
                .is_err()
        );
        assert!(
            runtime
                .check_policy_thread(Some("other"), "turn/start", &params)
                .is_err()
        );
        let owned = runtime
            .check_policy_thread(Some("binding"), "thread/resume", &params)
            .unwrap();
        assert!(runtime.release_policy_thread("thread", "binding").is_err());
        assert!(runtime.bind_policy_thread("thread", "new").await.is_err());
        assert!(
            runtime
                .check_policy_thread(None, "turn/interrupt", &params)
                .is_ok()
        );
        assert!(
            runtime
                .request("config/mcpServer/reload", Value::Null)
                .await
                .is_err()
        );
        drop(owned);
        runtime.release_policy_thread("thread", "binding").unwrap();
        assert!(
            runtime
                .check_policy_thread(None, "turn/start", &params)
                .is_ok()
        );
        runtime.shutdown().await.unwrap();
    }
}
