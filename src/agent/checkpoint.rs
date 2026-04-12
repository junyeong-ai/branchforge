//! Serializable snapshot of agent runtime state for crash recovery.
//!
//! # Design
//!
//! The agent runtime is **stateless across `execute()` calls** — each
//! run returns a fresh [`super::AgentResult`] and per-run metrics are
//! a DTO, not persistent state. There are exactly two pieces of state
//! that survive across calls and therefore must be captured for
//! crash recovery:
//!
//! 1. **`Session`** — the conversation graph, token usage, todos,
//!    plan, and cached context. The `SessionManager` persistence
//!    backend already handles this. The checkpoint records the
//!    `session_id` so resume can reload it.
//!
//! 2. **`BudgetTracker`** — the cross-session cost accumulator. The
//!    tracker is rebuilt every process boot, so we need to rehydrate
//!    it with the previously-consumed amount. The checkpoint records
//!    `budget_spent_usd` so resume can call
//!    [`crate::budget::BudgetTracker::restore_spent`].
//!
//! Everything else (per-run iterations, API call count, tool call
//! count) lives on `AgentMetrics` which is a per-run value returned
//! from `execute()`. Capturing those in a checkpoint would create
//! the wrong mental model where `execute()` is not idempotent across
//! process boundaries — it is, and should remain so.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::authorization::ExecutionMode;
use crate::ir::Usage;
use crate::session::SessionId;

/// Serializable snapshot of the two persistent state pieces that
/// survive across `execute()` calls: session identity + budget
/// accumulator. Combined with the session persistence backend, this
/// is everything needed to resume an agent after a crash.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example(agent: &branchforge::Agent) -> branchforge::Result<()> {
/// // Capture
/// let checkpoint = agent.checkpoint().await;
/// let bytes = serde_json::to_vec(&checkpoint)?;
/// std::fs::write("/tmp/agent.ckpt", bytes)?;
///
/// // Restore (different process)
/// let bytes = std::fs::read("/tmp/agent.ckpt")?;
/// let checkpoint: branchforge::AgentCheckpoint = serde_json::from_slice(&bytes)?;
/// let agent = branchforge::Agent::builder()
///     .model("claude-sonnet-4-5")
///     .max_budget_usd(rust_decimal_macros::dec!(10))
///     .resume_from(checkpoint)
///     .await?
///     .build()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentCheckpoint {
    /// Session id to resume from. The persistence backend loads the
    /// graph, todos, plan, usage, and cached context from this id.
    pub session_id: SessionId,

    /// Execution mode at checkpoint time (auto / supervised / plan).
    /// Restored onto the resumed agent so approval / plan semantics
    /// carry across process restarts.
    pub execution_mode: ExecutionMode,

    /// Cost already spent on this session before the checkpoint was
    /// taken. Restored into the resumed agent's `BudgetTracker` via
    /// [`crate::budget::BudgetTracker::restore_spent`] so over-budget
    /// detection fires at the correct accumulated total, not at zero.
    pub budget_spent_usd: Decimal,

    /// Aggregate token usage at checkpoint time. This is the exact
    /// `session.total_usage` snapshot — restored onto the resumed
    /// session so downstream metrics and cost dashboards see
    /// continuity across the restart.
    pub session_usage: Usage,

    /// When the checkpoint was captured. Callers can use this for
    /// staleness checks (e.g. refuse to resume a day-old checkpoint
    /// against a model whose pricing has changed).
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn sample_usage() -> Usage {
        Usage {
            input_tokens: 12_345,
            output_tokens: 6_789,
            cached_input_tokens: Some(1_000),
            cache_creation_tokens: Some(500),
            ..Default::default()
        }
    }

    #[test]
    fn checkpoint_serde_round_trip() {
        let checkpoint = AgentCheckpoint {
            session_id: SessionId::new(),
            execution_mode: ExecutionMode::Auto,
            budget_spent_usd: dec!(0.42),
            session_usage: sample_usage(),
            created_at: Utc::now(),
        };

        let json = serde_json::to_string(&checkpoint).expect("serialize");
        let restored: AgentCheckpoint = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.session_id, checkpoint.session_id);
        assert_eq!(restored.budget_spent_usd, dec!(0.42));
        assert_eq!(restored.session_usage.input_tokens, 12_345);
        assert_eq!(restored.session_usage.output_tokens, 6_789);
        assert_eq!(restored.session_usage.cached_input_tokens, Some(1_000));
        assert!(matches!(restored.execution_mode, ExecutionMode::Auto));
    }

    /// Supervised + Plan modes must also round-trip so that callers
    /// who paused an interactive session resume in the same mode.
    #[test]
    fn checkpoint_preserves_execution_mode() {
        for mode in [
            ExecutionMode::Auto,
            ExecutionMode::Supervised,
            ExecutionMode::Plan,
        ] {
            let checkpoint = AgentCheckpoint {
                session_id: SessionId::new(),
                execution_mode: mode.clone(),
                budget_spent_usd: Decimal::ZERO,
                session_usage: Usage::default(),
                created_at: Utc::now(),
            };
            let json = serde_json::to_string(&checkpoint).expect("serialize");
            let restored: AgentCheckpoint = serde_json::from_str(&json).expect("deserialize");
            assert!(
                std::mem::discriminant(&restored.execution_mode) == std::mem::discriminant(&mode),
                "mode {mode:?} did not round-trip"
            );
        }
    }
}
