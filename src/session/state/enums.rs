//! Session lifecycle state machine.
//!
//! `SessionState` is the single canonical FSM for both main sessions and
//! subagent invocations. The variants form a strict forward DAG with three
//! terminal outcomes, and the three "finalizing" phases
//! (`Completing`/`Failing`/`Cancelling`) encode the intended terminal
//! outcome at the type level — a session in `Completing` is guaranteed to
//! end in `Completed`, a guarantee enforced by
//! [`SessionState::terminal_from_finalizing`] and the transition graph.
//!
//! ```text
//! Created ──▶ Running ──┬─▶ Completing ──▶ Completed
//!                       ├─▶ Failing ─────▶ Failed
//!                       └─▶ Cancelling ──▶ Cancelled
//!
//! Failing and Cancelling are reachable from any non-terminal state
//! (hard-abort and user-cancel lanes).
//! ```
//!
//! All transitions go through [`SessionState::transition_to`], which
//! returns [`SessionTransitionError`] on an illegal move. Direct field
//! assignment via the `Session` API is no longer permitted — use
//! [`super::Session::transition`] instead.

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};
use std::fmt;

/// Canonical session lifecycle state.
///
/// This type is shared by main sessions, subagent sessions, and the task
/// tracker. There is no separate `TaskStatus` or `SubagentState` — the
/// lifecycle is modelled by this single FSM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The session is registered but no runtime work has started.
    #[default]
    Created,
    /// The session is actively executing — either driving the model loop
    /// or awaiting tool results. Tool-wait is an observable graph event,
    /// not a top-level state.
    Running,
    /// Finalization underway with the intent of a successful terminal.
    /// Guaranteed to end in [`SessionState::Completed`].
    Completing,
    /// Finalization underway due to an error. Guaranteed to end in
    /// [`SessionState::Failed`].
    Failing,
    /// Finalization underway due to a user-initiated cancel. Guaranteed
    /// to end in [`SessionState::Cancelled`].
    Cancelling,
    /// Terminal success.
    Completed,
    /// Terminal failure.
    Failed,
    /// Terminal cancellation.
    Cancelled,
}

impl SessionState {
    /// Stable lowercase id for logs and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Completing => "completing",
            Self::Failing => "failing",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// `true` while the session is actively executing.
    pub fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }

    /// `true` during the finalization phase, before a terminal state is
    /// reached.
    pub fn is_finalizing(self) -> bool {
        matches!(self, Self::Completing | Self::Failing | Self::Cancelling)
    }

    /// `true` for the three terminal states.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Map a finalizing state to its guaranteed terminal outcome.
    ///
    /// Returns `None` for any state that is not currently finalizing.
    pub fn terminal_from_finalizing(self) -> Option<Self> {
        match self {
            Self::Completing => Some(Self::Completed),
            Self::Failing => Some(Self::Failed),
            Self::Cancelling => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Inverse of [`Self::terminal_from_finalizing`]: given a terminal
    /// state, return the finalizing phase that leads into it.
    ///
    /// Returns `None` for any state that is not a terminal outcome.
    pub fn finalizing_phase(self) -> Option<Self> {
        match self {
            Self::Completed => Some(Self::Completing),
            Self::Failed => Some(Self::Failing),
            Self::Cancelled => Some(Self::Cancelling),
            _ => None,
        }
    }

    /// `true` if a transition from `self` to `next` is legal.
    ///
    /// Legal moves form a strict forward DAG:
    /// * `Created → Running`
    /// * `Running → {Completing, Failing, Cancelling}`
    /// * `Completing → Completed`
    /// * `Failing → Failed`
    /// * `Cancelling → Cancelled`
    /// * Any non-terminal state may jump directly to `Failing` (hard
    ///   abort) or `Cancelling` (user cancel).
    ///
    /// Self-loops and backward moves are rejected to keep transitions
    /// observable.
    pub fn can_transition_to(self, next: Self) -> bool {
        if self.is_terminal() || self == next {
            return false;
        }
        // Any non-terminal state may jump into the Failing or Cancelling
        // lanes (panic / user cancel). Terminals are already rejected.
        if matches!(next, Self::Failing | Self::Cancelling) {
            return true;
        }
        matches!(
            (self, next),
            (Self::Created, Self::Running)
                | (Self::Running, Self::Completing)
                | (Self::Completing, Self::Completed)
                | (Self::Failing, Self::Failed)
                | (Self::Cancelling, Self::Cancelled)
        )
    }

    /// Attempt a transition. Returns the new state on success, or
    /// [`SessionTransitionError`] if the move is illegal.
    pub fn transition_to(self, next: Self) -> Result<Self, SessionTransitionError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(SessionTransitionError {
                from: self,
                to: next,
            })
        }
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returned when [`SessionState::transition_to`] is called with an illegal
/// target. Carries both endpoints so callers can render a useful error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("illegal session transition {from} → {to}")]
pub struct SessionTransitionError {
    pub from: SessionState,
    pub to: SessionState,
}

#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionType {
    #[default]
    Main,
    Subagent {
        agent_type: String,
        description: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_transitions_succeed() {
        let s = SessionState::default();
        assert_eq!(s, SessionState::Created);
        let s = s.transition_to(SessionState::Running).unwrap();
        let s = s.transition_to(SessionState::Completing).unwrap();
        let s = s.transition_to(SessionState::Completed).unwrap();
        assert!(s.is_terminal());
    }

    #[test]
    fn failing_reachable_from_any_non_terminal() {
        for start in [SessionState::Created, SessionState::Running] {
            let next = start.transition_to(SessionState::Failing).unwrap();
            assert_eq!(next, SessionState::Failing);
        }
        let failed = SessionState::Failing
            .transition_to(SessionState::Failed)
            .unwrap();
        assert!(failed.is_terminal());
    }

    #[test]
    fn cancelling_reachable_from_any_non_terminal() {
        for start in [SessionState::Created, SessionState::Running] {
            let next = start.transition_to(SessionState::Cancelling).unwrap();
            assert_eq!(next, SessionState::Cancelling);
        }
    }

    #[test]
    fn terminal_states_reject_all_transitions() {
        for terminal in [
            SessionState::Completed,
            SessionState::Failed,
            SessionState::Cancelled,
        ] {
            for target in [
                SessionState::Created,
                SessionState::Running,
                SessionState::Completing,
                SessionState::Failing,
                SessionState::Cancelling,
                SessionState::Completed,
                SessionState::Failed,
                SessionState::Cancelled,
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
            SessionState::Created,
            SessionState::Running,
            SessionState::Completing,
            SessionState::Failing,
            SessionState::Cancelling,
        ] {
            assert!(s.transition_to(s).is_err());
        }
    }

    #[test]
    fn skipping_phases_disallowed() {
        // Cannot jump Created → Completing (must pass Running).
        assert!(
            SessionState::Created
                .transition_to(SessionState::Completing)
                .is_err()
        );
        // Cannot go Completing → Running (backwards).
        assert!(
            SessionState::Completing
                .transition_to(SessionState::Running)
                .is_err()
        );
        // Cannot jump Completing → Failed (wrong lane).
        assert!(
            SessionState::Completing
                .transition_to(SessionState::Failed)
                .is_err()
        );
    }

    #[test]
    fn terminal_from_finalizing_matches_transition_graph() {
        assert_eq!(
            SessionState::Completing.terminal_from_finalizing(),
            Some(SessionState::Completed)
        );
        assert_eq!(
            SessionState::Failing.terminal_from_finalizing(),
            Some(SessionState::Failed)
        );
        assert_eq!(
            SessionState::Cancelling.terminal_from_finalizing(),
            Some(SessionState::Cancelled)
        );
        assert_eq!(SessionState::Running.terminal_from_finalizing(), None);
    }

    #[test]
    fn transition_error_carries_both_endpoints() {
        let err = SessionState::Completed
            .transition_to(SessionState::Running)
            .unwrap_err();
        assert_eq!(err.from, SessionState::Completed);
        assert_eq!(err.to, SessionState::Running);
        assert!(err.to_string().contains("completed"));
        assert!(err.to_string().contains("running"));
    }

    #[test]
    fn display_and_as_str_agree() {
        assert_eq!(SessionState::Running.to_string(), "running");
        assert_eq!(SessionState::Cancelling.as_str(), "cancelling");
    }

    #[test]
    fn helpers_partition_state_space() {
        // Every variant is exactly one of: not-running/terminal/finalizing/running/other.
        assert!(SessionState::Running.is_running());
        assert!(SessionState::Completing.is_finalizing());
        assert!(SessionState::Failing.is_finalizing());
        assert!(SessionState::Cancelling.is_finalizing());
        assert!(SessionState::Completed.is_terminal());
        assert!(SessionState::Failed.is_terminal());
        assert!(SessionState::Cancelled.is_terminal());
        assert!(!SessionState::Created.is_running());
        assert!(!SessionState::Created.is_terminal());
    }
}
