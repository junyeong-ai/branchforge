//! Compaction strategy trait and supporting types.
//!
//! Defines the pluggable interface for context compaction. Implementations
//! decide *when* and *how* to reduce token usage:
//!
//! - [`FullCompaction`](super::full::FullCompaction): LLM-based summarization (permanent, modifies graph)
//! - [`MicroCompaction`](super::micro::MicroCompaction): Content truncation (temporary, graph untouched)
//! - [`TimeBasedCompaction`](super::time_based::TimeBasedCompaction): Idle-time triggered truncation

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::graph::NodeId;
use crate::ir::ContentPart;
use crate::session::SessionResult;
use crate::session::state::Session;
use crate::types::CompactResult;

/// Context available when deciding whether compaction is needed.
#[derive(Debug, Clone)]
pub struct CompactionContext {
    /// Current estimated input tokens.
    pub current_tokens: u64,
    /// Maximum context window tokens.
    pub max_tokens: u64,
    /// Number of messages in the current projection.
    pub message_count: usize,
    /// Time since last user interaction (for time-based strategies).
    pub idle_duration: Option<Duration>,
    /// When the last compaction occurred.
    pub last_compact_at: Option<DateTime<Utc>>,
    /// Number of consecutive compaction failures (for circuit breaker).
    pub consecutive_failures: u32,
}

impl CompactionContext {
    /// Token usage ratio (0.0 - 1.0).
    pub fn usage_ratio(&self) -> f64 {
        if self.max_tokens == 0 {
            return 0.0;
        }
        self.current_tokens as f64 / self.max_tokens as f64
    }
}

/// What a compaction strategy plans to do.
#[derive(Debug)]
pub enum CompactionPlan {
    /// No compaction needed.
    NotNeeded,

    /// LLM-based full summarization.
    /// Permanent: appends a Summary node to the graph.
    Summarize {
        /// The prompt to send to the summarization model.
        prompt: String,
        /// Number of messages being summarized.
        message_count: usize,
    },

    /// Register content overrides to truncate large tool results.
    /// Temporary: stored in `Session.content_overrides`, lost on reload.
    Override {
        /// Content blocks to replace in the projection.
        overrides: Vec<ContentOverrideEntry>,
        /// Estimated token savings from this operation.
        estimated_token_savings: u64,
    },
}

/// A single content override for micro-compaction.
#[derive(Debug, Clone)]
pub struct ContentOverrideEntry {
    /// Graph node ID whose content should be replaced.
    pub node_id: NodeId,
    /// Replacement content blocks (truncated version).
    pub replacement_content: Vec<ContentPart>,
    /// Original token count of the content being replaced.
    pub original_tokens: u64,
}

/// Pluggable compaction strategy.
///
/// Implementations determine when compaction is needed, what to compact,
/// and how to execute the compaction. The strategy pattern allows mixing
/// different approaches (full summarization, micro-compaction, time-based)
/// via [`CompactionChain`](super::chain::CompactionChain).
#[async_trait]
pub trait CompactionStrategy: Send + Sync {
    /// Human-readable name for logging and diagnostics.
    fn name(&self) -> &str;

    /// Whether this strategy requires an LLM call to execute.
    ///
    /// Used by `CompactionChain` to skip LLM-dependent strategies
    /// when no client is available.
    fn requires_llm(&self) -> bool;

    /// Whether this strategy's results survive session reload.
    ///
    /// - `true`: Modifies the graph (e.g., adds Summary node). Results
    ///   persist across session save/load.
    /// - `false`: Modifies only the in-memory projection. Results are
    ///   lost when the session is reloaded from persistence.
    fn is_durable(&self) -> bool;

    /// Check whether compaction is needed given the current context.
    fn needs_compact(&self, ctx: &CompactionContext) -> bool;

    /// Plan what to compact without executing.
    ///
    /// Returns `NotNeeded` if no suitable targets are found.
    fn plan(&self, session: &Session) -> SessionResult<CompactionPlan>;

    /// Execute the compaction plan.
    ///
    /// - For `Summarize` plans: requires `client` to call the LLM.
    /// - For `Override` plans: `client` is ignored.
    async fn execute(
        &self,
        plan: CompactionPlan,
        session: &mut Session,
        client: Option<&crate::Client>,
    ) -> crate::Result<CompactResult>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_context_usage_ratio() {
        let ctx = CompactionContext {
            current_tokens: 80_000,
            max_tokens: 100_000,
            message_count: 50,
            idle_duration: None,
            last_compact_at: None,
            consecutive_failures: 0,
        };
        assert!((ctx.usage_ratio() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn compaction_context_zero_max_tokens() {
        let ctx = CompactionContext {
            current_tokens: 100,
            max_tokens: 0,
            message_count: 1,
            idle_duration: None,
            last_compact_at: None,
            consecutive_failures: 0,
        };
        assert!((ctx.usage_ratio()).abs() < f64::EPSILON);
    }
}
