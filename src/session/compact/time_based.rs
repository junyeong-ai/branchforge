//! Time-based compaction — idle-time triggered content truncation.
//!
//! Applies the same content truncation as [`MicroCompaction`]
//! but is triggered by idle time between interactions rather than token thresholds.
//! This aligns with server-side prompt cache TTLs: after a period of inactivity,
//! cached prefixes may have expired, making old tool results pure overhead.

use std::time::Duration;

use async_trait::async_trait;

use super::micro::MicroCompaction;
use super::strategy::{CompactionContext, CompactionPlan, CompactionStrategy};
use crate::session::state::Session;
use crate::session::SessionResult;
use crate::types::CompactResult;

/// Time-based compaction strategy.
///
/// Delegates truncation logic to [`MicroCompaction`] but only activates
/// when `idle_duration` exceeds the configured threshold.
pub struct TimeBasedCompaction {
    /// Idle time threshold before triggering.
    pub idle_threshold: Duration,
    /// Inner micro-compaction for actual truncation.
    inner: MicroCompaction,
}

impl TimeBasedCompaction {
    pub fn new(idle_threshold: Duration) -> Self {
        Self {
            idle_threshold,
            inner: MicroCompaction::default(),
        }
    }

    pub fn with_micro(mut self, micro: MicroCompaction) -> Self {
        self.inner = micro;
        self
    }
}

impl Default for TimeBasedCompaction {
    fn default() -> Self {
        // No fixed default — SDK doesn't assume server infrastructure.
        // Users must set idle_threshold explicitly.
        Self::new(Duration::from_secs(3600))
    }
}

#[async_trait]
impl CompactionStrategy for TimeBasedCompaction {
    fn name(&self) -> &str {
        "time_based"
    }

    fn requires_llm(&self) -> bool {
        false
    }

    fn is_durable(&self) -> bool {
        false
    }

    fn needs_compact(&self, ctx: &CompactionContext) -> bool {
        ctx.idle_duration
            .is_some_and(|idle| idle >= self.idle_threshold)
    }

    fn plan(&self, session: &Session) -> SessionResult<CompactionPlan> {
        self.inner.plan(session)
    }

    async fn execute(
        &self,
        plan: CompactionPlan,
        session: &mut Session,
        client: Option<&crate::Client>,
    ) -> crate::Result<CompactResult> {
        self.inner.execute(plan, session, client).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_based_metadata() {
        let tb = TimeBasedCompaction::default();
        assert_eq!(tb.name(), "time_based");
        assert!(!tb.requires_llm());
        assert!(!tb.is_durable());
    }

    #[test]
    fn time_based_needs_compact_idle() {
        let tb = TimeBasedCompaction::new(Duration::from_secs(300));

        let not_idle = CompactionContext {
            current_tokens: 50_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: Some(Duration::from_secs(60)),
            last_compact_at: None,
            consecutive_failures: 0,
        };
        assert!(!tb.needs_compact(&not_idle));

        let idle = CompactionContext {
            current_tokens: 50_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: Some(Duration::from_secs(600)),
            last_compact_at: None,
            consecutive_failures: 0,
        };
        assert!(tb.needs_compact(&idle));
    }

    #[test]
    fn time_based_no_idle_info() {
        let tb = TimeBasedCompaction::default();
        let ctx = CompactionContext {
            current_tokens: 90_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: None,
            last_compact_at: None,
            consecutive_failures: 0,
        };
        assert!(!tb.needs_compact(&ctx));
    }
}
