//! Full compaction — LLM-based conversation summarization.
//!
//! Permanent strategy: appends a Summary node to the graph, which moves
//! the projection boundary forward (all prior messages are excluded from
//! future API calls).

use async_trait::async_trait;

use super::CompactResult;
use super::service::{CompactConfig, Compactor};
use super::strategy::{CompactionContext, CompactionPlan, CompactionStrategy};
use crate::session::SessionResult;
use crate::session::state::Session;

/// LLM-based full summarization strategy.
///
/// Delegates to [`Compactor`] for the actual summarization prompt
/// and graph operations. This wrapper implements [`CompactionStrategy`]
/// so it can participate in a [`CompactionChain`](super::chain::CompactionChain).
pub struct FullCompaction {
    config: CompactConfig,
}

impl FullCompaction {
    pub fn new(config: CompactConfig) -> Self {
        Self { config }
    }

    /// Threshold ratio (0.0-1.0) above which compaction is triggered.
    pub fn threshold(&self) -> f32 {
        self.config.threshold_percent
    }
}

impl Default for FullCompaction {
    fn default() -> Self {
        Self::new(CompactConfig::default())
    }
}

#[async_trait]
impl CompactionStrategy for FullCompaction {
    fn name(&self) -> &str {
        "full"
    }

    fn requires_llm(&self) -> bool {
        true
    }

    fn is_durable(&self) -> bool {
        true
    }

    fn needs_compact(&self, ctx: &CompactionContext) -> bool {
        if !self.config.enabled {
            return false;
        }
        ctx.usage_ratio() >= self.config.threshold_percent as f64
    }

    fn plan(&self, session: &Session) -> SessionResult<CompactionPlan> {
        let service = Compactor::new(self.config.clone());
        let prepared = service.prepare_compact(session)?;
        match prepared {
            super::service::PreparedCompact::NotNeeded => Ok(CompactionPlan::NotNeeded),
            super::service::PreparedCompact::Ready {
                summary_prompt,
                message_count,
            } => Ok(CompactionPlan::Summarize {
                prompt: summary_prompt,
                message_count,
            }),
        }
    }

    async fn execute(
        &self,
        plan: CompactionPlan,
        session: &mut Session,
        llm: Option<&dyn crate::client::LlmCall>,
    ) -> crate::Result<CompactResult> {
        let CompactionPlan::Summarize { prompt, .. } = plan else {
            return Ok(CompactResult::NotNeeded);
        };

        let llm = llm
            .ok_or_else(|| crate::Error::Config("FullCompaction requires an LLM client".into()))?;

        let ir_request = crate::ir::ModelRequest::new(
            &self.config.summary_model,
            vec![crate::ir::Message::user(&prompt)],
        )
        .with_max_tokens(self.config.max_summary_tokens);

        let ir_response = llm.send(&ir_request).await?;
        let summary = ir_response.text();

        let service = Compactor::new(self.config.clone());
        let result = service.apply_compact(session, summary)?;
        service.record_compact(session, &result);

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_compaction_metadata() {
        let fc = FullCompaction::default();
        assert_eq!(fc.name(), "full");
        assert!(fc.requires_llm());
        assert!(fc.is_durable());
    }

    #[test]
    fn full_compaction_needs_compact_threshold() {
        let fc = FullCompaction::new(CompactConfig::default().threshold(0.8));

        let below = CompactionContext {
            current_tokens: 70_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: None,
            last_compact_at: None,
            consecutive_failures: 0,
        };
        assert!(!fc.needs_compact(&below));

        let above = CompactionContext {
            current_tokens: 85_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: None,
            last_compact_at: None,
            consecutive_failures: 0,
        };
        assert!(fc.needs_compact(&above));
    }

    #[test]
    fn full_compaction_disabled() {
        let fc = FullCompaction::new(CompactConfig::disabled());
        let ctx = CompactionContext {
            current_tokens: 95_000,
            max_tokens: 100_000,
            message_count: 10,
            idle_duration: None,
            last_compact_at: None,
            consecutive_failures: 0,
        };
        assert!(!fc.needs_compact(&ctx));
    }
}
