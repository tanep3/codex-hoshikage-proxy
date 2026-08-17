use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "accept_for_session")]
    AcceptForSession,
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "cancel")]
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub approval_id: String,
    pub rpc_id: u64,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub available_decisions: Vec<ApprovalDecision>,
    pub details: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalState {
    Pending {
        request: ApprovalRequest,
        expires_at_ms: u128,
    },
    Approved {
        decision: ApprovalDecision,
    },
    Denied,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalEvent {
    UserDecisionReceived(ApprovalDecision),
    TimeoutElapsed,
    TurnCancelled,
    RuntimeLost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Accepted,
    Declined,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalEffect {
    ReplyToCodex(ApprovalDecision),
    NotifyTurn(ApprovalOutcome),
    RejectDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalTransition {
    pub next: ApprovalState,
    pub effects: Vec<ApprovalEffect>,
}

pub fn reduce_approval(state: &ApprovalState, event: ApprovalEvent) -> ApprovalTransition {
    use ApprovalEffect::*;
    use ApprovalEvent::*;
    use ApprovalState::*;

    match (state, event) {
        (
            Pending {
                request,
                expires_at_ms,
            },
            UserDecisionReceived(decision),
        ) if request.available_decisions.contains(&decision) => match decision {
            ApprovalDecision::Accept | ApprovalDecision::AcceptForSession => ApprovalTransition {
                next: Approved {
                    decision: decision.clone(),
                },
                effects: vec![
                    ReplyToCodex(decision),
                    NotifyTurn(ApprovalOutcome::Accepted),
                ],
            },
            ApprovalDecision::Decline => ApprovalTransition {
                next: Denied,
                effects: vec![
                    ReplyToCodex(decision),
                    NotifyTurn(ApprovalOutcome::Declined),
                ],
            },
            ApprovalDecision::Cancel => ApprovalTransition {
                next: Cancelled,
                effects: vec![
                    ReplyToCodex(decision),
                    NotifyTurn(ApprovalOutcome::Cancelled),
                ],
            },
        },
        (Pending { .. }, UserDecisionReceived(_)) => ApprovalTransition {
            next: state.clone(),
            effects: vec![RejectDecision],
        },
        (Pending { .. }, TimeoutElapsed) => ApprovalTransition {
            next: Expired,
            effects: vec![
                ReplyToCodex(ApprovalDecision::Decline),
                NotifyTurn(ApprovalOutcome::Expired),
            ],
        },
        (Pending { .. }, TurnCancelled) => ApprovalTransition {
            next: Cancelled,
            effects: vec![
                ReplyToCodex(ApprovalDecision::Cancel),
                NotifyTurn(ApprovalOutcome::Cancelled),
            ],
        },
        (Pending { .. }, RuntimeLost) => ApprovalTransition {
            next: Cancelled,
            effects: vec![NotifyTurn(ApprovalOutcome::Cancelled)],
        },
        _ => ApprovalTransition {
            next: state.clone(),
            effects: vec![RejectDecision],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> ApprovalState {
        ApprovalState::Pending {
            request: ApprovalRequest {
                approval_id: "approval_1".into(),
                rpc_id: 7,
                thread_id: "thread_1".into(),
                turn_id: Some("turn_1".into()),
                available_decisions: vec![ApprovalDecision::Accept, ApprovalDecision::Decline],
                details: Value::Null,
            },
            expires_at_ms: 100,
        }
    }

    #[test]
    fn accepted_decision_notifies_turn() {
        let transition = reduce_approval(
            &pending(),
            ApprovalEvent::UserDecisionReceived(ApprovalDecision::Accept),
        );
        assert!(matches!(transition.next, ApprovalState::Approved { .. }));
        assert!(
            transition
                .effects
                .contains(&ApprovalEffect::NotifyTurn(ApprovalOutcome::Accepted))
        );
    }

    #[test]
    fn unavailable_decision_is_rejected_without_state_change() {
        let state = pending();
        let transition = reduce_approval(
            &state,
            ApprovalEvent::UserDecisionReceived(ApprovalDecision::Cancel),
        );
        assert_eq!(transition.next, state);
        assert_eq!(transition.effects, vec![ApprovalEffect::RejectDecision]);
    }

    #[test]
    fn timeout_declines_and_expires() {
        let transition = reduce_approval(&pending(), ApprovalEvent::TimeoutElapsed);
        assert_eq!(transition.next, ApprovalState::Expired);
        assert!(
            transition
                .effects
                .contains(&ApprovalEffect::ReplyToCodex(ApprovalDecision::Decline))
        );
    }

    #[test]
    fn terminal_approval_rejects_second_decision() {
        let state = reduce_approval(
            &pending(),
            ApprovalEvent::UserDecisionReceived(ApprovalDecision::Decline),
        )
        .next;
        let transition = reduce_approval(
            &state,
            ApprovalEvent::UserDecisionReceived(ApprovalDecision::Accept),
        );
        assert_eq!(transition.next, state);
        assert_eq!(transition.effects, vec![ApprovalEffect::RejectDecision]);
    }
}
