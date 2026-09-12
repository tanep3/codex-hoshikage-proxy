use crate::{
    approval::{
        ApprovalDecision, ApprovalEffect, ApprovalEvent, ApprovalRequest, ApprovalState,
        reduce_approval,
    },
    runtime::{CodexRuntime, RuntimeError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use tokio::{sync::Mutex, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalCapability {
    None,
    Interactive,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApprovalView {
    pub id: String,
    pub state: &'static str,
    pub available_decisions: Vec<ApprovalDecision>,
    pub details: Value,
    pub expires_at_ms: Option<u128>,
    pub reply_status: &'static str,
}

#[derive(Debug, Error)]
pub enum ApprovalManagerError {
    #[error("approval not found: {0}")]
    NotFound(String),
    #[error("invalid approval decision: {0}")]
    InvalidDecision(String),
    #[error("approval decision rejected")]
    Rejected,
    #[error("failed to reply to Codex: {0}")]
    Runtime(#[from] RuntimeError),
}

struct ApprovalRecord {
    reply_status: &'static str,
    request: ApprovalRequest,
    state: ApprovalState,
}

#[derive(Clone)]
struct TurnApprovalContext {
    capability: ApprovalCapability,
    cwd: PathBuf,
    suppress_auto_approval: bool,
    turn_id: Option<String>,
}

pub struct ApprovalManager {
    runtime: Arc<CodexRuntime>,
    turn_contexts: Mutex<HashMap<String, TurnApprovalContext>>,
    records: Mutex<HashMap<String, ApprovalRecord>>,
    file_change_paths: Mutex<HashMap<String, Vec<String>>>,
    timeout: Duration,
    auto_approve_workspace: bool,
}

impl ApprovalManager {
    pub fn new(
        runtime: Arc<CodexRuntime>,
        timeout: Duration,
        auto_approve_workspace: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            turn_contexts: Mutex::new(HashMap::new()),
            records: Mutex::new(HashMap::new()),
            file_change_paths: Mutex::new(HashMap::new()),
            timeout,
            auto_approve_workspace,
        })
    }

    pub fn start(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        let mut notifications = manager.runtime.subscribe();
        tokio::spawn(async move {
            loop {
                let event = match notifications.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "approval listener missed runtime events");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if event.get("kind").and_then(Value::as_str) != Some("server_request") {
                    if event["method"] == "serverRequest/resolved" {
                        let params = &event["params"];
                        let mut records = manager.records.lock().await;
                        for record in records.values_mut() {
                            if params["threadId"] == record.request.thread_id
                                && params["requestId"] == record.request.rpc_id
                                && matches!(record.state, ApprovalState::Pending { .. })
                            {
                                record.state = ApprovalState::Cancelled;
                            }
                        }
                    }
                    if event.get("method").and_then(Value::as_str) == Some("turn/completed") {
                        let params = &event["params"];
                        if let Some(thread) = params.get("threadId").and_then(Value::as_str)
                            && let Some(turn) = params
                                .get("turnId")
                                .or_else(|| params.pointer("/turn/id"))
                                .and_then(Value::as_str)
                        {
                            manager.invalidate_turn(thread, turn).await;
                        }
                    }
                    if event.get("kind").and_then(Value::as_str) == Some("transport_closed") {
                        let mut records = manager.records.lock().await;
                        for record in records.values_mut() {
                            if matches!(record.state, ApprovalState::Pending { .. }) {
                                record.state = ApprovalState::Cancelled;
                            }
                        }
                    }
                    manager.observe_file_change_notification(&event).await;
                    continue;
                }
                let Some(method) = event.get("method").and_then(Value::as_str) else {
                    continue;
                };
                let Some(rpc_id) = event
                    .get("rpc_id")
                    .filter(|id| id.is_string() || id.is_i64() || id.is_u64())
                    .cloned()
                else {
                    continue;
                };
                let mut params = event.get("params").cloned().unwrap_or_else(|| json!({}));
                if !matches!(
                    method,
                    "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
                ) {
                    // The managed execution worker owns the one registered dynamic tool.
                    if method == "item/tool/call" && params["tool"] == "hoshikage_publish_artifact"
                    {
                        continue;
                    }
                    if params["turnId"].as_str().is_none()
                        && let Some(thread) = params["threadId"].as_str()
                        && let Some(turn) = manager
                            .turn_contexts
                            .lock()
                            .await
                            .get(thread)
                            .and_then(|c| c.turn_id.clone())
                    {
                        params["turnId"] = json!(turn);
                    }
                    // Publish before replying: upstream can finish immediately after an error.
                    // Never fabricate answers or use command-approval decisions for other schemas.
                    manager.runtime.publish(json!({
                        "kind":"interaction_unavailable", "threadId":params["threadId"],
                        "turnId":params["turnId"], "method_name":method,
                        "code":"unsupported_interaction"
                    }));
                    if let Err(error) = manager.runtime.reject_server_request(rpc_id).await {
                        tracing::warn!(%error, method, "server request rejection delivery unknown");
                    }
                    continue;
                }
                manager.attach_known_file_change_paths(&mut params).await;
                let _ = manager.handle_request(rpc_id, method, params).await;
            }
        });
    }

    async fn observe_file_change_notification(&self, event: &Value) {
        let Some(params) = event.get("params") else {
            return;
        };
        let (item_id, changes) = match event.get("method").and_then(Value::as_str) {
            Some("item/started" | "item/completed")
                if params.pointer("/item/type").and_then(Value::as_str) == Some("fileChange") =>
            {
                (params.pointer("/item/id"), params.pointer("/item/changes"))
            }
            Some("item/fileChange/patchUpdated") => (params.get("itemId"), params.get("changes")),
            _ => return,
        };
        let Some(item_id) = item_id.and_then(Value::as_str) else {
            return;
        };
        let paths = changes
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|change| {
                [change.get("path"), change.pointer("/kind/move_path")]
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !paths.is_empty() {
            self.file_change_paths
                .lock()
                .await
                .insert(item_id.to_owned(), paths);
        }
    }

    async fn attach_known_file_change_paths(&self, params: &mut Value) {
        let Some(item_id) = params.get("itemId").and_then(Value::as_str) else {
            return;
        };
        let Some(paths) = self.file_change_paths.lock().await.get(item_id).cloned() else {
            return;
        };
        if let Some(object) = params.as_object_mut() {
            object.insert("paths".into(), json!(paths));
        }
    }

    pub async fn register_turn(
        &self,
        thread_id: &str,
        capability: ApprovalCapability,
        cwd: &Path,
        suppress_auto_approval: bool,
    ) {
        self.turn_contexts.lock().await.insert(
            thread_id.into(),
            TurnApprovalContext {
                capability,
                cwd: cwd.to_path_buf(),
                suppress_auto_approval,
                turn_id: None,
            },
        );
    }

    pub async fn bind_turn(&self, thread_id: &str, turn_id: &str) {
        let mut contexts = self.turn_contexts.lock().await;
        if let Some(context) = contexts.get_mut(thread_id) {
            context.turn_id = Some(turn_id.into());
        }
        for record in self.records.lock().await.values_mut() {
            if record.request.thread_id == thread_id && record.request.turn_id.is_none() {
                record.request.turn_id = Some(turn_id.into());
                record.request.details["turnId"] = json!(turn_id);
            }
        }
    }

    pub async fn invalidate_turn(&self, thread_id: &str, turn_id: &str) {
        let mut contexts = self.turn_contexts.lock().await;
        if contexts.get(thread_id).and_then(|c| c.turn_id.as_deref()) == Some(turn_id) {
            contexts.remove(thread_id);
        }
        drop(contexts);
        for record in self.records.lock().await.values_mut() {
            if record.request.thread_id == thread_id
                && record.request.turn_id.as_deref() == Some(turn_id)
                && matches!(record.state, ApprovalState::Pending { .. })
            {
                record.state = ApprovalState::Cancelled;
            }
        }
    }

    pub async fn pending_events_for_turn(&self, turn_id: &str) -> Vec<Value> {
        let records = self.records.lock().await;
        records
            .values()
            .filter_map(|record| {
                let request = &record.request;
                if request.turn_id.as_deref() != Some(turn_id)
                    || !matches!(record.state, ApprovalState::Pending { expires_at_ms, .. } if expires_at_ms > crate::journal::now_ms())
                {
                    return None;
                }
                Some(json!({
                    "kind": "approval_requested",
                    "approval_id": request.approval_id,
                    "threadId": request.thread_id,
                    "turnId": request.turn_id,
                    "availableDecisions": request.available_decisions,
                }))
            })
            .collect()
    }

    pub async fn get(&self, approval_id: &str) -> Result<ApprovalView, ApprovalManagerError> {
        let records = self.records.lock().await;
        let record = records
            .get(approval_id)
            .ok_or_else(|| ApprovalManagerError::NotFound(approval_id.into()))?;
        Ok(view_of(approval_id, record))
    }

    pub async fn decide(
        &self,
        approval_id: &str,
        decision: &str,
    ) -> Result<ApprovalView, ApprovalManagerError> {
        let decision = parse_wire_decision(decision)
            .ok_or_else(|| ApprovalManagerError::InvalidDecision(decision.into()))?;
        let (rpc_id, transition, view) = {
            let mut records = self.records.lock().await;
            let record = records
                .get_mut(approval_id)
                .ok_or_else(|| ApprovalManagerError::NotFound(approval_id.into()))?;
            if matches!(&record.state, ApprovalState::Pending { expires_at_ms, .. } if *expires_at_ms <= crate::journal::now_ms())
            {
                drop(records);
                self.expire(approval_id).await?;
                return Err(ApprovalManagerError::Rejected);
            }
            let transition = reduce_approval(
                &record.state,
                ApprovalEvent::UserDecisionReceived(decision.clone()),
            );
            if transition.effects.contains(&ApprovalEffect::RejectDecision) {
                return Err(ApprovalManagerError::Rejected);
            }
            let effects = transition.effects.clone();
            record.state = transition.next;
            record.reply_status = "unknown";
            let view = view_of(approval_id, record);
            (record.request.rpc_id.clone(), effects, view)
        };
        for effect in transition {
            if let ApprovalEffect::ReplyToCodex(decision) = effect {
                self.runtime
                    .respond_to_server_request(
                        rpc_id.clone(),
                        json!({"decision": codex_decision(&decision)}),
                    )
                    .await?;
            }
        }
        if let Some(record) = self.records.lock().await.get_mut(approval_id) {
            record.reply_status = "written";
        }
        self.runtime.publish(json!({
            "kind": "approval_resolved",
            "approval_id": approval_id,
            "threadId": view.details.get("threadId"),
            "turnId": view.details.get("turnId"),
            "state": view.state,
        }));
        tracing::info!(approval_id, decision = ?decision, state = view.state, "approval resolved");
        self.get(approval_id).await
    }

    async fn handle_request(
        self: &Arc<Self>,
        rpc_id: Value,
        method: &str,
        mut params: Value,
    ) -> Result<(), ApprovalManagerError> {
        let thread_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // Keep context selection and record insertion atomic with bind_turn.
        let contexts = self.turn_contexts.lock().await;
        if params.get("turnId").and_then(Value::as_str).is_none()
            && let Some(turn_id) = contexts.get(&thread_id).and_then(|c| c.turn_id.clone())
        {
            params["turnId"] = json!(turn_id);
        }
        let turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let available_decisions = params
            .get("availableDecisions")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().and_then(parse_codex_decision))
                    .collect::<Vec<_>>()
            })
            .filter(|decisions| !decisions.is_empty())
            .unwrap_or_else(|| default_decisions_for(method));
        let approval_id = format!("approval_{}", uuid::Uuid::new_v4());
        let request = ApprovalRequest {
            approval_id: approval_id.clone(),
            rpc_id: rpc_id.clone(),
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            available_decisions,
            details: params.clone(),
        };
        let available_decisions = request.available_decisions.clone();
        let context = contexts
            .get(&thread_id)
            .filter(|c| c.turn_id.is_none() || turn_id.is_none() || c.turn_id == turn_id)
            .cloned()
            .unwrap_or(TurnApprovalContext {
                capability: ApprovalCapability::None,
                cwd: PathBuf::new(),
                suppress_auto_approval: true,
                turn_id: None,
            });
        let capability = context.capability;
        let auto_approved = self.auto_approve_workspace
            && !context.suppress_auto_approval
            && request_is_in_workspace(method, &params, &context.cwd);
        let automatic_decision = auto_approved.then(|| preferred_accept(&available_decisions));
        let state = if let Some(decision) = automatic_decision.clone() {
            ApprovalState::Approved { decision }
        } else if capability == ApprovalCapability::Interactive {
            ApprovalState::Pending {
                request: request.clone(),
                expires_at_ms: crate::journal::now_ms() + timeout_ms(self.timeout),
            }
        } else {
            ApprovalState::Cancelled
        };
        self.records.lock().await.insert(
            approval_id.clone(),
            ApprovalRecord {
                reply_status: if matches!(state, ApprovalState::Pending { .. }) {
                    "not_sent"
                } else {
                    "unknown"
                },
                request,
                state,
            },
        );
        drop(contexts);
        if auto_approved {
            let decision = automatic_decision.expect("automatic approval decision is present");
            self.runtime
                .respond_to_server_request(rpc_id, json!({"decision": codex_decision(&decision)}))
                .await?;
            self.runtime.publish(json!({
                "kind": "approval_resolved",
                "approval_id": approval_id,
                "threadId": thread_id,
                "turnId": turn_id,
                "state": "approved",
                "automatic": true,
            }));
            tracing::info!(approval_id, "workspace approval automatically accepted");
        } else if capability == ApprovalCapability::Interactive {
            tracing::warn!(
                approval_id,
                method,
                "approval request is waiting for interactive decision"
            );
            let manager = Arc::clone(self);
            let timeout_id = approval_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(manager.timeout).await;
                let _ = manager.expire(&timeout_id).await;
            });
            self.runtime.publish(json!({
                "kind": "approval_requested",
                "approval_id": approval_id,
                "threadId": thread_id,
                "turnId": turn_id,
                "availableDecisions": available_decisions,
            }));
        } else {
            self.runtime
                .respond_to_server_request(rpc_id, json!({"decision": "cancel"}))
                .await?;
            self.runtime.publish(json!({
                "kind": "approval_required",
                "approval_id": approval_id,
                "threadId": thread_id,
                "turnId": turn_id,
            }));
        }
        if (auto_approved || capability != ApprovalCapability::Interactive)
            && let Some(record) = self.records.lock().await.get_mut(&approval_id)
        {
            record.reply_status = "written";
        }
        Ok(())
    }

    async fn expire(&self, approval_id: &str) -> Result<(), ApprovalManagerError> {
        let (rpc_id, effects, view) = {
            let mut records = self.records.lock().await;
            let record = records
                .get_mut(approval_id)
                .ok_or_else(|| ApprovalManagerError::NotFound(approval_id.into()))?;
            let transition = reduce_approval(&record.state, ApprovalEvent::TimeoutElapsed);
            if transition.effects.contains(&ApprovalEffect::RejectDecision) {
                return Ok(());
            }
            let effects = transition.effects.clone();
            record.state = transition.next;
            record.reply_status = "unknown";
            (
                record.request.rpc_id.clone(),
                effects,
                view_of(approval_id, record),
            )
        };
        for effect in effects {
            if let ApprovalEffect::ReplyToCodex(decision) = effect {
                self.runtime
                    .respond_to_server_request(
                        rpc_id.clone(),
                        json!({"decision": codex_decision(&decision)}),
                    )
                    .await?;
            }
        }
        if let Some(record) = self.records.lock().await.get_mut(approval_id) {
            record.reply_status = "written";
        }
        self.runtime.publish(json!({
            "kind": "approval_resolved",
            "approval_id": approval_id,
            "threadId": view.details.get("threadId"),
            "turnId": view.details.get("turnId"),
            "state": view.state,
        }));
        tracing::warn!(approval_id, state = view.state, "approval expired");
        Ok(())
    }
}

fn preferred_accept(decisions: &[ApprovalDecision]) -> ApprovalDecision {
    if decisions.contains(&ApprovalDecision::Accept) {
        ApprovalDecision::Accept
    } else if decisions.contains(&ApprovalDecision::AcceptForSession) {
        ApprovalDecision::AcceptForSession
    } else {
        ApprovalDecision::Cancel
    }
}

fn request_is_in_workspace(method: &str, params: &Value, cwd: &Path) -> bool {
    // A cwd inside the workspace does not authorize a network grant or an
    // additional sandbox permission. These need an explicit interactive decision.
    if [
        "networkApprovalContext",
        "additionalPermissions",
        "proposedExecpolicyAmendment",
    ]
    .iter()
    .any(|key| params.get(*key).is_some_and(|v| !v.is_null()))
    {
        return false;
    }
    if cwd.as_os_str().is_empty()
        || !matches!(
            method,
            "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "execCommandApproval"
                | "applyPatchApproval"
        )
    {
        return false;
    }
    let mut paths = Vec::new();
    for key in [
        "cwd",
        "path",
        "filePath",
        "file_path",
        "targetPath",
        "target_path",
        "grantRoot",
        "paths",
    ] {
        paths.extend(structured_path_values(params.get(key)));
    }
    paths.extend(
        params
            .get("fileChanges")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|changes| changes.keys().map(String::as_str)),
    );
    paths.extend(
        params
            .get("commandActions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|action| action.get("path").and_then(Value::as_str)),
    );
    // Every reported target must be inside the workspace. Missing targets
    // provide no evidence that a file approval is safe to automate.
    !paths.is_empty() && paths.iter().all(|path| path_is_within(path, cwd))
}

fn structured_path_values(value: Option<&Value>) -> Box<dyn Iterator<Item = &str> + '_> {
    match value {
        Some(Value::String(path)) => Box::new(std::iter::once(path.as_str())),
        Some(Value::Array(paths)) => Box::new(paths.iter().filter_map(Value::as_str)),
        _ => Box::new(std::iter::empty()),
    }
}

fn path_is_within(path: &str, cwd: &Path) -> bool {
    let candidate = Path::new(path);
    let candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        cwd.join(candidate)
    };
    canonicalize_for_comparison(&candidate)
        .map(|canonical| canonical.starts_with(cwd))
        .unwrap_or(false)
}

fn canonicalize_for_comparison(path: &Path) -> Option<std::path::PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }
    let file_name = path.file_name()?.to_owned();
    let parent = path.parent()?;
    canonicalize_for_comparison(parent).map(|canonical_parent| canonical_parent.join(file_name))
}

fn timeout_ms(timeout: Duration) -> u128 {
    timeout.as_millis()
}

fn view_of(id: &str, record: &ApprovalRecord) -> ApprovalView {
    let (state, available_decisions) = match &record.state {
        ApprovalState::Pending {
            request,
            expires_at_ms: _,
        } => ("pending", request.available_decisions.clone()),
        ApprovalState::Approved { decision: _ } => {
            ("approved", record.request.available_decisions.clone())
        }
        ApprovalState::Denied => ("denied", record.request.available_decisions.clone()),
        ApprovalState::Expired => ("expired", record.request.available_decisions.clone()),
        ApprovalState::Cancelled => ("cancelled", record.request.available_decisions.clone()),
    };
    ApprovalView {
        id: id.into(),
        state,
        available_decisions,
        details: record.request.details.clone(),
        expires_at_ms: match &record.state {
            ApprovalState::Pending { expires_at_ms, .. } => Some(*expires_at_ms),
            _ => None,
        },
        reply_status: record.reply_status,
    }
}

#[derive(Debug, Deserialize)]
pub struct ApprovalDecisionRequest {
    pub expected_turn_id: Option<String>,
    pub expected_thread_id: Option<String>,
    pub decision: String,
}

fn parse_wire_decision(value: &str) -> Option<ApprovalDecision> {
    match value {
        "accept" => Some(ApprovalDecision::Accept),
        "accept_for_session" => Some(ApprovalDecision::AcceptForSession),
        "decline" => Some(ApprovalDecision::Decline),
        "cancel" => Some(ApprovalDecision::Cancel),
        _ => None,
    }
}

fn default_decisions_for(method: &str) -> Vec<ApprovalDecision> {
    if matches!(
        method,
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
    ) {
        vec![
            ApprovalDecision::Accept,
            ApprovalDecision::AcceptForSession,
            ApprovalDecision::Decline,
            ApprovalDecision::Cancel,
        ]
    } else {
        Vec::new()
    }
}

fn parse_codex_decision(value: &str) -> Option<ApprovalDecision> {
    match value {
        "accept" => Some(ApprovalDecision::Accept),
        "acceptForSession" | "accept_for_session" => Some(ApprovalDecision::AcceptForSession),
        "decline" => Some(ApprovalDecision::Decline),
        "cancel" => Some(ApprovalDecision::Cancel),
        _ => None,
    }
}

fn codex_decision(decision: &ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::Accept => "accept",
        ApprovalDecision::AcceptForSession => "acceptForSession",
        ApprovalDecision::Decline => "decline",
        ApprovalDecision::Cancel => "cancel",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_is_exposed_in_milliseconds_without_fixed_default() {
        assert_eq!(timeout_ms(Duration::from_secs(10)), 10_000);
        assert_eq!(timeout_ms(Duration::from_millis(250)), 250);
    }

    #[test]
    fn command_approval_without_available_decisions_uses_standard_choices() {
        assert_eq!(
            default_decisions_for("item/commandExecution/requestApproval"),
            vec![
                ApprovalDecision::Accept,
                ApprovalDecision::AcceptForSession,
                ApprovalDecision::Decline,
                ApprovalDecision::Cancel,
            ]
        );
    }

    #[test]
    fn unknown_approval_method_does_not_gain_implicit_choices() {
        assert!(default_decisions_for("item/permissions/requestApproval").is_empty());
    }

    #[test]
    fn workspace_request_is_detected_by_cwd() {
        let cwd = Path::new("/home/tane/work");
        assert!(request_is_in_workspace(
            "item/commandExecution/requestApproval",
            &json!({"cwd": "/home/tane/work", "command": "python3 script.py"}),
            cwd
        ));
        assert!(!request_is_in_workspace(
            "item/commandExecution/requestApproval",
            &json!({"cwd": "/tmp", "command": "python3 script.py"}),
            cwd
        ));
        assert!(!request_is_in_workspace(
            "item/commandExecution/requestApproval",
            &json!({
                "cwd": "/tmp",
                "command": "python3 --output /home/tane/work/result.txt"
            }),
            cwd
        ));
    }

    #[test]
    fn workspace_request_is_detected_by_structured_file_path() {
        let root = Path::new("/tmp").canonicalize().unwrap();
        assert!(request_is_in_workspace(
            "item/fileChange/requestApproval",
            &json!({"file_path": root.join(".agents/skills/foo/SKILL.md")}),
            &root
        ));
        assert!(request_is_in_workspace(
            "item/fileChange/requestApproval",
            &json!({"path": root.join("new-file.txt")}),
            &root
        ));
        assert!(!request_is_in_workspace(
            "item/fileChange/requestApproval",
            &json!({"targetPath": "/var/tmp/outside.txt"}),
            &root
        ));
        assert!(request_is_in_workspace(
            "item/fileChange/requestApproval",
            &json!({
                "fileChanges": {
                    "/tmp/.agents/skills/foo/SKILL.md": {"type": "add"}
                }
            }),
            &root
        ));
        assert!(request_is_in_workspace(
            "item/fileChange/requestApproval",
            &json!({"grantRoot": "/tmp/.agents"}),
            &root
        ));
        assert!(!request_is_in_workspace(
            "item/fileChange/requestApproval",
            &json!({}),
            &root
        ));
        assert!(!request_is_in_workspace(
            "item/permissions/requestApproval",
            &json!({"cwd": "/tmp"}),
            &root
        ));
    }

    #[test]
    fn workspace_auto_approval_requires_all_reported_paths_to_be_inside() {
        let cwd = Path::new("/tmp").canonicalize().unwrap();
        for params in [
            json!({"paths": ["/tmp/inside", "/var/outside"]}),
            json!({"fileChanges": {"/tmp/inside": {}, "/var/outside": {}}}),
            json!({"cwd": "/tmp", "commandActions": [{"path": "/var/outside"}]}),
            json!({"grantRoot": "/tmp", "paths": ["/var/outside"]}),
        ] {
            assert!(
                !request_is_in_workspace("item/fileChange/requestApproval", &params, &cwd),
                "{params}"
            );
        }
        assert!(request_is_in_workspace(
            "item/fileChange/requestApproval",
            &json!({"paths": ["/tmp/one", "/tmp/two"]}),
            &cwd
        ));
    }

    #[test]
    fn command_text_does_not_grant_workspace_auto_approval() {
        let cwd = Path::new("/home/tane/work");
        assert!(!request_is_in_workspace(
            "item/commandExecution/requestApproval",
            &json!({"command": "printf /home/tane/work > /tmp/outside"}),
            cwd
        ));
    }

    #[test]
    fn workspace_cwd_does_not_auto_grant_additional_permissions() {
        for (key, value) in [
            (
                "networkApprovalContext",
                json!({"host":"example.com","protocol":"https"}),
            ),
            (
                "additionalPermissions",
                json!({"fileSystem":{"write":["/var/outside"]}}),
            ),
            ("proposedExecpolicyAmendment", json!(["curl"])),
        ] {
            let mut params = json!({"cwd":"/tmp","command":"tool"});
            params[key] = value;
            assert!(!request_is_in_workspace(
                "item/commandExecution/requestApproval",
                &params,
                Path::new("/tmp")
            ));
        }
    }
}
