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
    request: ApprovalRequest,
    state: ApprovalState,
}

#[derive(Clone)]
struct TurnApprovalContext {
    capability: ApprovalCapability,
    cwd: PathBuf,
}

pub struct ApprovalManager {
    runtime: Arc<CodexRuntime>,
    next_id: Mutex<u64>,
    turn_contexts: Mutex<HashMap<String, TurnApprovalContext>>,
    records: Mutex<HashMap<String, ApprovalRecord>>,
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
            next_id: Mutex::new(1),
            turn_contexts: Mutex::new(HashMap::new()),
            records: Mutex::new(HashMap::new()),
            timeout,
            auto_approve_workspace,
        })
    }

    pub fn start(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let mut notifications = manager.runtime.subscribe();
            while let Ok(event) = notifications.recv().await {
                if event.get("kind").and_then(Value::as_str) != Some("server_request") {
                    continue;
                }
                let Some(method) = event.get("method").and_then(Value::as_str) else {
                    continue;
                };
                if !method.contains("requestApproval") {
                    continue;
                }
                let Some(rpc_id) = event.get("rpc_id").and_then(Value::as_u64) else {
                    continue;
                };
                let params = event.get("params").cloned().unwrap_or_else(|| json!({}));
                let _ = manager.handle_request(rpc_id, method, params).await;
            }
        });
    }

    pub async fn register_turn(&self, thread_id: &str, capability: ApprovalCapability, cwd: &Path) {
        self.turn_contexts.lock().await.insert(
            thread_id.into(),
            TurnApprovalContext {
                capability,
                cwd: cwd.to_path_buf(),
            },
        );
    }

    pub async fn pending_events_for_turn(&self, turn_id: &str) -> Vec<Value> {
        let records = self.records.lock().await;
        records
            .values()
            .filter_map(|record| {
                let request = &record.request;
                if request.turn_id.as_deref() != Some(turn_id)
                    || !matches!(record.state, ApprovalState::Pending { .. })
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
            let transition = reduce_approval(
                &record.state,
                ApprovalEvent::UserDecisionReceived(decision.clone()),
            );
            if transition.effects.contains(&ApprovalEffect::RejectDecision) {
                return Err(ApprovalManagerError::Rejected);
            }
            let effects = transition.effects.clone();
            record.state = transition.next;
            let view = view_of(approval_id, record);
            (record.request.rpc_id, effects, view)
        };
        for effect in transition {
            if let ApprovalEffect::ReplyToCodex(decision) = effect {
                self.runtime
                    .respond_to_server_request(
                        rpc_id,
                        json!({"decision": codex_decision(&decision)}),
                    )
                    .await?;
            }
        }
        self.runtime.publish(json!({
            "kind": "approval_resolved",
            "approval_id": approval_id,
            "threadId": view.details.get("threadId"),
            "turnId": view.details.get("turnId"),
            "state": view.state,
        }));
        tracing::info!(approval_id, decision = ?decision, state = view.state, "approval resolved");
        Ok(view)
    }

    async fn handle_request(
        self: &Arc<Self>,
        rpc_id: u64,
        method: &str,
        params: Value,
    ) -> Result<(), ApprovalManagerError> {
        let thread_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
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
        let approval_id = {
            let mut next = self.next_id.lock().await;
            let id = format!("approval_{}", *next);
            *next = next.saturating_add(1);
            id
        };
        let request = ApprovalRequest {
            approval_id: approval_id.clone(),
            rpc_id,
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            available_decisions,
            details: params.clone(),
        };
        let available_decisions = request.available_decisions.clone();
        let context = self
            .turn_contexts
            .lock()
            .await
            .get(&thread_id)
            .cloned()
            .unwrap_or(TurnApprovalContext {
                capability: ApprovalCapability::None,
                cwd: PathBuf::new(),
            });
        let capability = context.capability;
        let auto_approved =
            self.auto_approve_workspace && request_is_in_workspace(&params, &context.cwd);
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
        self.records
            .lock()
            .await
            .insert(approval_id.clone(), ApprovalRecord { request, state });
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
            (record.request.rpc_id, effects, view_of(approval_id, record))
        };
        for effect in effects {
            if let ApprovalEffect::ReplyToCodex(decision) = effect {
                self.runtime
                    .respond_to_server_request(
                        rpc_id,
                        json!({"decision": codex_decision(&decision)}),
                    )
                    .await?;
            }
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

fn request_is_in_workspace(params: &Value, cwd: &Path) -> bool {
    if cwd.as_os_str().is_empty() {
        return false;
    }
    params
        .get("cwd")
        .and_then(Value::as_str)
        .is_some_and(|request_cwd| Path::new(request_cwd) == cwd)
        || params
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(cwd.to_string_lossy().as_ref()))
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
    }
}

#[derive(Debug, Deserialize)]
pub struct ApprovalDecisionRequest {
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
            &json!({"cwd": "/home/tane/work", "command": "python3 script.py"}),
            cwd
        ));
        assert!(!request_is_in_workspace(
            &json!({"cwd": "/tmp", "command": "python3 script.py"}),
            cwd
        ));
    }
}
