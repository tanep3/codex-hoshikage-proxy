use crate::{
    approval::{
        ApprovalDecision, ApprovalEffect, ApprovalEvent, ApprovalRequest, ApprovalState,
        reduce_approval,
    },
    runtime::{CodexRuntime, RuntimeError},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;
use tokio::sync::Mutex;

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

pub struct ApprovalManager {
    runtime: Arc<CodexRuntime>,
    next_id: Mutex<u64>,
    turn_capabilities: Mutex<HashMap<String, ApprovalCapability>>,
    records: Mutex<HashMap<String, ApprovalRecord>>,
}

impl ApprovalManager {
    pub fn new(runtime: Arc<CodexRuntime>) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            next_id: Mutex::new(1),
            turn_capabilities: Mutex::new(HashMap::new()),
            records: Mutex::new(HashMap::new()),
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
                let _ = manager.handle_request(rpc_id, params).await;
            }
        });
    }

    pub async fn register_turn(&self, thread_id: &str, capability: ApprovalCapability) {
        self.turn_capabilities
            .lock()
            .await
            .insert(thread_id.into(), capability);
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
            "state": view.state,
        }));
        Ok(view)
    }

    async fn handle_request(
        self: &Arc<Self>,
        rpc_id: u64,
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
                    .collect()
            })
            .unwrap_or_default();
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
        let capability = self
            .turn_capabilities
            .lock()
            .await
            .get(&thread_id)
            .copied()
            .unwrap_or(ApprovalCapability::None);
        let state = if capability == ApprovalCapability::Interactive {
            ApprovalState::Pending {
                request: request.clone(),
                expires_at_ms: crate::journal::now_ms() + 300_000,
            }
        } else {
            ApprovalState::Cancelled
        };
        self.records
            .lock()
            .await
            .insert(approval_id.clone(), ApprovalRecord { request, state });
        if capability == ApprovalCapability::Interactive {
            let manager = Arc::clone(self);
            let timeout_id = approval_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                let _ = manager.expire(&timeout_id).await;
            });
            self.runtime.publish(json!({
                "kind": "approval_requested",
                "approval_id": approval_id,
                "threadId": thread_id,
                "turnId": turn_id,
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
            "state": view.state,
        }));
        Ok(())
    }
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
