//! 6-state lifecycle FSM for subagent execution.
//!
//! The existing [`super::TaskStatus`] is a flat enum of terminal +
//! transient states without transition validation. `SubagentState` is
//! a stricter, layered companion that models the *lifecycle* of a
//! single subagent invocation as a state machine with validated
//! transitions:
//!
//! ```text
//!     Spawning ──▶ Awaiting ──▶ Ready ──▶ Running ──▶ Finished
//!         │            │           │          │
//!         │            │           │          ▼
//!         └────────────┴───────────┴───────▶ Failed
//! ```
//!
//! Every state transition goes through
//! [`SubagentState::transition_to`], which returns
//! [`SubagentTransitionError`] if the move is illegal. This catches
//! "Finished → Running" and similar bugs at the type-system level
//! without requiring callers to remember the legal arrows.
//!
//! The FSM is a strict superset of the legacy `TaskStatus` flow:
//! orchestrator code can opt into it incrementally without disturbing
//! callers that still read the flat status. See
//! [`SubagentState::is_terminal`] for the `Finished` / `Failed`
//! check that gates cleanup.

use std::fmt;

/// Lifecycle state of a single subagent invocation.
///
/// States are ordered by progress and a transition is legal only if
/// it advances forward in the diagram above (with `Failed` reachable
/// from any non-terminal state).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum SubagentState {
    /// The orchestrator has been asked to start the subagent but the
    /// runtime task / process is not yet alive. This is the only
    /// legal initial state.
    #[default]
    Spawning,
    /// The subagent process is up and the orchestrator is waiting
    /// for it to handshake (e.g. report its tool catalogue, send a
    /// hello message). The subagent has not yet accepted work.
    Awaiting,
    /// The subagent finished its handshake and is idle, ready to
    /// receive work.
    Ready,
    /// The subagent is actively executing a task. From here it can
    /// only return to `Finished` or `Failed`.
    Running,
    /// Terminal success state. The subagent's last task completed
    /// successfully and the orchestrator can collect the result.
    /// Cleanup runs immediately after entering this state.
    Finished,
    /// Terminal failure state. The subagent crashed, timed out, or
    /// returned a non-recoverable error. Cleanup runs as for
    /// `Finished`.
    Failed,
}

impl SubagentState {
    /// Stable string id for logs and metrics.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Spawning => "spawning",
            Self::Awaiting => "awaiting",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Finished => "finished",
            Self::Failed => "failed",
        }
    }

    /// `true` if this state cannot transition any further.
    /// Orchestrator code uses this to gate cleanup paths.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished | Self::Failed)
    }

    /// `true` if a transition from `self` to `next` is legal.
    ///
    /// The legal moves form a strict forward DAG with `Failed`
    /// reachable from any non-terminal state. Self-loops are
    /// **disallowed** to keep transitions observable.
    pub fn can_transition_to(&self, next: SubagentState) -> bool {
        if self.is_terminal() {
            return false;
        }
        if *self == next {
            return false;
        }
        // Failed is reachable from any non-terminal state.
        if next == SubagentState::Failed {
            return true;
        }
        // Forward progression along the happy path.
        matches!(
            (*self, next),
            (Self::Spawning, Self::Awaiting)
                | (Self::Awaiting, Self::Ready)
                | (Self::Ready, Self::Running)
                | (Self::Running, Self::Finished)
        )
    }

    /// Attempt a transition. Returns the new state on success, or
    /// [`SubagentTransitionError`] if the move is illegal.
    pub fn transition_to(
        self,
        next: SubagentState,
    ) -> Result<SubagentState, SubagentTransitionError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(SubagentTransitionError {
                from: self,
                to: next,
            })
        }
    }
}

impl fmt::Display for SubagentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returned when [`SubagentState::transition_to`] is called with an
/// illegal target. Carries both endpoints so the caller can render a
/// useful error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("illegal subagent transition {from} → {to}")]
pub struct SubagentTransitionError {
    pub from: SubagentState,
    pub to: SubagentState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_transitions_succeed() {
        let s = SubagentState::default();
        assert_eq!(s, SubagentState::Spawning);
        let s = s.transition_to(SubagentState::Awaiting).unwrap();
        let s = s.transition_to(SubagentState::Ready).unwrap();
        let s = s.transition_to(SubagentState::Running).unwrap();
        let s = s.transition_to(SubagentState::Finished).unwrap();
        assert!(s.is_terminal());
    }

    #[test]
    fn failed_reachable_from_any_non_terminal() {
        for start in [
            SubagentState::Spawning,
            SubagentState::Awaiting,
            SubagentState::Ready,
            SubagentState::Running,
        ] {
            let next = start.transition_to(SubagentState::Failed).unwrap();
            assert_eq!(next, SubagentState::Failed);
            assert!(next.is_terminal());
        }
    }

    #[test]
    fn terminal_states_reject_all_transitions() {
        for terminal in [SubagentState::Finished, SubagentState::Failed] {
            for target in [
                SubagentState::Spawning,
                SubagentState::Awaiting,
                SubagentState::Ready,
                SubagentState::Running,
                SubagentState::Finished,
                SubagentState::Failed,
            ] {
                assert!(
                    terminal.transition_to(target).is_err(),
                    "{terminal:?} → {target:?} should be illegal"
                );
            }
        }
    }

    #[test]
    fn self_loops_disallowed() {
        for s in [
            SubagentState::Spawning,
            SubagentState::Awaiting,
            SubagentState::Ready,
            SubagentState::Running,
        ] {
            assert!(s.transition_to(s).is_err());
        }
    }

    #[test]
    fn skipping_phases_disallowed() {
        // Cannot jump Spawning → Running (must pass Awaiting + Ready).
        assert!(
            SubagentState::Spawning
                .transition_to(SubagentState::Running)
                .is_err()
        );
        // Cannot go Running → Awaiting (backwards).
        assert!(
            SubagentState::Running
                .transition_to(SubagentState::Awaiting)
                .is_err()
        );
    }

    #[test]
    fn transition_error_carries_both_endpoints() {
        let err = SubagentState::Finished
            .transition_to(SubagentState::Running)
            .unwrap_err();
        assert_eq!(err.from, SubagentState::Finished);
        assert_eq!(err.to, SubagentState::Running);
        assert!(err.to_string().contains("finished"));
        assert!(err.to_string().contains("running"));
    }

    #[test]
    fn as_str_round_trips_through_display() {
        assert_eq!(SubagentState::Awaiting.to_string(), "awaiting");
        assert_eq!(SubagentState::Ready.as_str(), "ready");
    }
}
