use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeState {
    Stopped,
    Starting { attempt: u32 },
    Initializing,
    Ready,
    Recovering { attempt: u32 },
    Failed { message: String },
    ShuttingDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    StartRequested,
    ProcessSpawned,
    InitializeSucceeded,
    InitializeFailed { message: String },
    ProcessExited { code: Option<i32> },
    ShutdownRequested,
    ShutdownCompleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEffect {
    SpawnProcess,
    SendInitialize,
    MarkReady,
    RecordFailure,
    CloseTransport,
    StopProcess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTransition {
    pub next: RuntimeState,
    pub effects: Vec<RuntimeEffect>,
}

pub fn reduce_runtime(state: &RuntimeState, event: RuntimeEvent) -> RuntimeTransition {
    use RuntimeEffect::*;
    use RuntimeEvent::*;
    use RuntimeState::*;

    match (state, event) {
        (Stopped, StartRequested) => RuntimeTransition {
            next: Starting { attempt: 1 },
            effects: vec![SpawnProcess],
        },
        (Starting { attempt: _ }, ProcessSpawned) => RuntimeTransition {
            next: Initializing,
            effects: vec![SendInitialize],
        },
        (Initializing, InitializeSucceeded) => RuntimeTransition {
            next: Ready,
            effects: vec![MarkReady],
        },
        (Initializing, InitializeFailed { message }) => RuntimeTransition {
            next: Failed {
                message: message.clone(),
            },
            effects: vec![RecordFailure, CloseTransport],
        },
        (Ready, ProcessExited { code: _ }) => RuntimeTransition {
            next: Recovering { attempt: 1 },
            effects: vec![RecordFailure],
        },
        (Starting { .. } | Initializing, ProcessExited { .. }) => RuntimeTransition {
            next: Recovering { attempt: 1 },
            effects: vec![RecordFailure],
        },
        (Recovering { attempt }, StartRequested) => RuntimeTransition {
            next: Starting {
                attempt: attempt.saturating_add(1),
            },
            effects: vec![SpawnProcess],
        },
        (
            Starting { .. } | Initializing | Ready | Recovering { .. } | Failed { .. },
            ShutdownRequested,
        ) => RuntimeTransition {
            next: ShuttingDown,
            effects: vec![StopProcess],
        },
        (ShuttingDown, ShutdownCompleted) => RuntimeTransition {
            next: Stopped,
            effects: vec![CloseTransport],
        },
        _ => RuntimeTransition {
            next: state.clone(),
            effects: Vec::new(),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub state: RuntimeState,
    pub observed_at: SystemTime,
}

impl HealthSnapshot {
    pub fn is_ready(&self) -> bool {
        self.state == RuntimeState::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_success_moves_runtime_to_ready() {
        let state = RuntimeState::Initializing;
        let transition = reduce_runtime(&state, RuntimeEvent::InitializeSucceeded);

        assert_eq!(transition.next, RuntimeState::Ready);
        assert_eq!(transition.effects, vec![RuntimeEffect::MarkReady]);
        assert_eq!(state, RuntimeState::Initializing);
    }

    #[test]
    fn invalid_event_does_not_change_state_or_create_effects() {
        let state = RuntimeState::Ready;
        let transition = reduce_runtime(&state, RuntimeEvent::InitializeSucceeded);

        assert_eq!(transition.next, state);
        assert!(transition.effects.is_empty());
    }

    #[test]
    fn shutdown_is_available_during_initialization() {
        let transition =
            reduce_runtime(&RuntimeState::Initializing, RuntimeEvent::ShutdownRequested);

        assert_eq!(transition.next, RuntimeState::ShuttingDown);
        assert_eq!(transition.effects, vec![RuntimeEffect::StopProcess]);
    }
}
