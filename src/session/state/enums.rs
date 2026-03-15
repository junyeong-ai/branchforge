//! Session state enumerations.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    #[default]
    Created,
    Active,
    WaitingForTools,
    Completing,
    Failing,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl SessionState {
    /// Parse from string with lenient matching (case-insensitive, accepts common aliases).
    pub fn from_str_lenient(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "active" => Self::Active,
            "waitingfortools" | "waiting_for_tools" => Self::WaitingForTools,
            "completing" => Self::Completing,
            "failing" => Self::Failing,
            "cancelling" | "canceling" => Self::Cancelling,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" | "canceled" => Self::Cancelled,
            _ => Self::Created,
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, Self::Active | Self::WaitingForTools)
    }

    pub fn is_finalizing(self) -> bool {
        matches!(self, Self::Completing | Self::Failing | Self::Cancelling)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn terminal_from_finalizing(self) -> Option<Self> {
        match self {
            Self::Completing => Some(Self::Completed),
            Self::Failing => Some(Self::Failed),
            Self::Cancelling => Some(Self::Cancelled),
            _ => None,
        }
    }
}

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
    fn test_session_state_from_str_lenient() {
        assert_eq!(
            SessionState::from_str_lenient("active"),
            SessionState::Active
        );
        assert_eq!(
            SessionState::from_str_lenient("waitingfortools"),
            SessionState::WaitingForTools
        );
        assert_eq!(
            SessionState::from_str_lenient("waiting_for_tools"),
            SessionState::WaitingForTools
        );
        assert_eq!(
            SessionState::from_str_lenient("completing"),
            SessionState::Completing
        );
        assert_eq!(
            SessionState::from_str_lenient("failing"),
            SessionState::Failing
        );
        assert_eq!(
            SessionState::from_str_lenient("cancelling"),
            SessionState::Cancelling
        );
        assert_eq!(
            SessionState::from_str_lenient("canceling"),
            SessionState::Cancelling
        );
        assert_eq!(
            SessionState::from_str_lenient("completed"),
            SessionState::Completed
        );
        assert_eq!(
            SessionState::from_str_lenient("failed"),
            SessionState::Failed
        );
        assert_eq!(
            SessionState::from_str_lenient("cancelled"),
            SessionState::Cancelled
        );
        assert_eq!(
            SessionState::from_str_lenient("canceled"),
            SessionState::Cancelled
        );
        assert_eq!(
            SessionState::from_str_lenient("unknown"),
            SessionState::Created
        );
    }

    #[test]
    fn test_session_state_helpers() {
        assert!(SessionState::Active.is_running());
        assert!(SessionState::WaitingForTools.is_running());
        assert!(SessionState::Completing.is_finalizing());
        assert!(SessionState::Failing.is_finalizing());
        assert!(SessionState::Cancelling.is_finalizing());
        assert_eq!(
            SessionState::Completing.terminal_from_finalizing(),
            Some(SessionState::Completed)
        );
        assert!(SessionState::Completed.is_terminal());
        assert!(SessionState::Failed.is_terminal());
        assert!(SessionState::Cancelled.is_terminal());
    }
}
