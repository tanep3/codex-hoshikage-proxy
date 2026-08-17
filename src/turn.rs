use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnState {
    Created,
    Starting,
    Running,
    Completed,
    Failed { message: String },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    Enqueued,
    CodexTurnStarted,
    CodexTurnCompleted,
    CodexTurnFailed { message: String },
    CancelRequested,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEffect {
    StartCodexTurn,
    CompleteResponse,
    FailResponse,
    CancelCodexTurn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnTransition {
    pub next: TurnState,
    pub effects: Vec<TurnEffect>,
}

pub fn reduce_turn(state: &TurnState, event: TurnEvent) -> TurnTransition {
    use TurnEffect::*;
    use TurnEvent::*;
    use TurnState::*;

    match (state, event) {
        (Created, Enqueued) => TurnTransition {
            next: Starting,
            effects: vec![StartCodexTurn],
        },
        (Starting, CodexTurnStarted) => TurnTransition {
            next: Running,
            effects: Vec::new(),
        },
        (Running, CodexTurnCompleted) => TurnTransition {
            next: Completed,
            effects: vec![CompleteResponse],
        },
        (Starting | Running, CodexTurnFailed { message }) => TurnTransition {
            next: Failed { message },
            effects: vec![FailResponse],
        },
        (Starting | Running, CancelRequested) => TurnTransition {
            next: Cancelled,
            effects: vec![CancelCodexTurn],
        },
        _ => TurnTransition {
            next: state.clone(),
            effects: Vec::new(),
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseRecord {
    pub id: String,
    pub object: &'static str,
    pub model: String,
    pub output: Vec<ResponseOutputItem>,
    pub status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseOutputItem {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: &'static str,
    pub role: &'static str,
    pub content: Vec<ResponseContentPart>,
    pub status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseContentPart {
    #[serde(rename = "type")]
    pub part_type: &'static str,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_has_explicit_start_and_completion_transitions() {
        let starting = reduce_turn(&TurnState::Created, TurnEvent::Enqueued);
        assert_eq!(starting.next, TurnState::Starting);
        assert_eq!(starting.effects, vec![TurnEffect::StartCodexTurn]);

        let running = reduce_turn(&starting.next, TurnEvent::CodexTurnStarted);
        assert_eq!(running.next, TurnState::Running);

        let completed = reduce_turn(&running.next, TurnEvent::CodexTurnCompleted);
        assert_eq!(completed.next, TurnState::Completed);
        assert_eq!(completed.effects, vec![TurnEffect::CompleteResponse]);
    }
}
