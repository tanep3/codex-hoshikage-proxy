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
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
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
struct JsonRpcResponse {
    id: Option<u64>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
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

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RuntimeError>>>>>;

pub struct CodexRuntime {
    state: Arc<RwLock<RuntimeState>>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicU64,
    notifications: broadcast::Sender<Value>,
    child: Arc<Mutex<Option<Child>>>,
}

impl CodexRuntime {
    pub async fn launch(config: &ValidatedConfig) -> Result<Arc<Self>, RuntimeError> {
        let mut command = Command::new(&config.codex_command);
        command
            .args(&config.codex_args)
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
            state: Arc::new(RwLock::new(RuntimeState::Starting { attempt: 1 })),
            stdin: Arc::new(Mutex::new(stdin)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            notifications,
            child: Arc::new(Mutex::new(Some(child))),
        });
        {
            let mut state = runtime.state.write().await;
            *state = reduce_runtime(&state, RuntimeEvent::ProcessSpawned).next;
        }
        runtime.spawn_reader(stdout);
        runtime.spawn_process_monitor();

        if let Err(error) = runtime.initialize().await {
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

    async fn initialize(&self) -> Result<(), RuntimeError> {
        let params = json!({
            "clientInfo": {
                "name": "codex-hoshikage-proxy",
                "title": "Codex Hoshikage Proxy",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {}
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

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        let message = serde_json::to_vec(&JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })
        .map_err(|error| RuntimeError::Protocol(error.to_string()))?;
        if let Err(error) = self.write_line(&message).await {
            self.pending.lock().await.remove(&id);
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
            self.pending.lock().await.remove(&id);
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
        stdin.write_all(message).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    fn spawn_reader(self: &Arc<Self>, stdout: tokio::process::ChildStdout) {
        let pending = Arc::clone(&self.pending);
        let pending_for_exit = Arc::clone(&pending);
        let notifications = self.notifications.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let parsed = match serde_json::from_str::<JsonRpcResponse>(&line) {
                    Ok(message) => message,
                    Err(_) => {
                        let _ = notifications.send(json!({"kind":"protocol_error", "line": line}));
                        continue;
                    }
                };
                if let Some(id) = parsed.id {
                    if let Some(sender) = pending.lock().await.remove(&id) {
                        let result = match (parsed.result, parsed.error) {
                            (Some(value), _) => Ok(value),
                            (_, Some(error)) => Err(RuntimeError::Protocol(format!(
                                "{} ({})",
                                error.message, error.code
                            ))),
                            _ => Err(RuntimeError::Protocol(
                                "response has neither result nor error".into(),
                            )),
                        };
                        let _ = sender.send(result);
                    }
                } else {
                    let _ = notifications.send(json!({"kind":"notification", "message": line}));
                }
            }
            let mut pending = pending_for_exit.lock().await;
            for (_, sender) in pending.drain() {
                let _ = sender.send(Err(RuntimeError::Protocol(
                    "Codex App Server transport closed".into(),
                )));
            }
        });
    }

    fn spawn_process_monitor(self: &Arc<Self>) {
        let child = Arc::clone(&self.child);
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let status = {
                let mut guard = child.lock().await;
                match guard.as_mut() {
                    Some(process) => process.wait().await.ok(),
                    None => None,
                }
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

    pub async fn snapshot(&self) -> RuntimeState {
        self.state.read().await.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.notifications.subscribe()
    }

    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        {
            let mut state = self.state.write().await;
            *state = reduce_runtime(&state, RuntimeEvent::ShutdownRequested).next;
        }
        let mut child = self.child.lock().await;
        if let Some(process) = child.as_mut() {
            process.kill().await?;
        }
        let mut state = self.state.write().await;
        *state = reduce_runtime(&state, RuntimeEvent::ShutdownCompleted).next;
        Ok(())
    }
}
