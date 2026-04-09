//! Serializable snapshot of agent runtime state for crash recovery.
//!
//! Session data (messages, graph, todos) is already persisted by the
//! persistence backend. This checkpoint captures the *runtime metadata*
//! that would otherwise be lost on restart: iteration count, accumulated
//! metrics, and remaining budget.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::authorization::ExecutionMode;
use crate::session::SessionId;

use super::task_budget::TaskBudget;

/// Serializable snapshot of agent runtime state for crash recovery.
///
/// Session data (messages, graph, todos) is already persisted by the
/// persistence backend. This checkpoint captures the *runtime metadata*
/// that would otherwise be lost on restart: iteration count, accumulated
/// metrics, and remaining budget.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCheckpoint {
    /// Session ID to resume from.
    pub session_id: SessionId,
    /// Approximate iteration count, derived from session message count.
    pub iteration: u32,
    /// Total API calls made.
    pub api_calls: u32,
    /// Total tool calls executed.
    pub tool_calls: usize,
    /// Accumulated cost in USD.
    pub total_cost_usd: Decimal,
    /// Execution mode at checkpoint time.
    pub execution_mode: ExecutionMode,
    /// Remaining task budget, if any.
    pub budget_remaining: Option<TaskBudget>,
    /// When this checkpoint was created.
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn checkpoint_serde_round_trip() {
        let checkpoint = AgentCheckpoint {
            session_id: SessionId::new(),
            iteration: 7,
            api_calls: 14,
            tool_calls: 23,
            total_cost_usd: dec!(0.42),
            execution_mode: ExecutionMode::Auto,
            budget_remaining: Some(TaskBudget {
                max_iterations: Some(93),
                max_cost_usd: Some(dec!(9.58)),
                ..Default::default()
            }),
            created_at: Utc::now(),
        };

        let json = serde_json::to_string(&checkpoint).expect("serialize");
        let restored: AgentCheckpoint = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.session_id, checkpoint.session_id);
        assert_eq!(restored.iteration, 7);
        assert_eq!(restored.api_calls, 14);
        assert_eq!(restored.tool_calls, 23);
        assert_eq!(restored.total_cost_usd, dec!(0.42));
        assert!(matches!(restored.execution_mode, ExecutionMode::Auto));
        let budget = restored.budget_remaining.unwrap();
        assert_eq!(budget.max_iterations, Some(93));
        assert_eq!(budget.max_cost_usd, Some(dec!(9.58)));
        assert_eq!(budget.max_tokens, None);
    }

    #[test]
    fn checkpoint_serde_no_budget() {
        let checkpoint = AgentCheckpoint {
            session_id: SessionId::new(),
            iteration: 0,
            api_calls: 0,
            tool_calls: 0,
            total_cost_usd: Decimal::ZERO,
            execution_mode: ExecutionMode::Supervised,
            budget_remaining: None,
            created_at: Utc::now(),
        };

        let json = serde_json::to_string(&checkpoint).expect("serialize");
        let restored: AgentCheckpoint = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.session_id, checkpoint.session_id);
        assert!(restored.budget_remaining.is_none());
        assert!(matches!(restored.execution_mode, ExecutionMode::Supervised));
    }

    #[test]
    fn checkpoint_serde_plan_mode() {
        let checkpoint = AgentCheckpoint {
            session_id: SessionId::new(),
            iteration: 3,
            api_calls: 6,
            tool_calls: 10,
            total_cost_usd: dec!(1.23),
            execution_mode: ExecutionMode::Plan,
            budget_remaining: None,
            created_at: Utc::now(),
        };

        let json = serde_json::to_string(&checkpoint).expect("serialize");
        let restored: AgentCheckpoint = serde_json::from_str(&json).expect("deserialize");

        assert!(restored.execution_mode.is_plan());
    }
}
