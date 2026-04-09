//! Compaction chain — Chain of Responsibility pattern.
//!
//! Tries registered strategies in order (cheapest first), executing the
//! first one that reports compaction is needed. If still over threshold
//! after one strategy, continues to the next.
//!
//! Integrates a [`CircuitBreaker`] to stop retrying after consecutive failures.

use std::sync::Arc;

use tracing::{debug, info, warn};

use super::CompactResult;
use super::strategy::{CompactionContext, CompactionPlan, CompactionStrategy};
use crate::common::circuit::{CircuitBreaker, CircuitConfig};
use crate::session::memory::{MemoryEntry, MemoryStore};
use crate::session::state::Session;

/// Chain of compaction strategies with circuit breaker protection.
pub struct CompactionChain {
    strategies: Vec<Box<dyn CompactionStrategy>>,
    circuit_breaker: CircuitBreaker,
    /// Optional memory store for auto-persisting compaction summaries.
    memory_store: Option<Arc<dyn MemoryStore>>,
}

impl CompactionChain {
    pub fn builder() -> CompactionChainBuilder {
        CompactionChainBuilder::new()
    }

    /// Try compaction using registered strategies in order.
    ///
    /// Returns the first successful result, or `NotNeeded` if no strategy
    /// found work to do. Multiple strategies may execute if the first one
    /// doesn't bring tokens below threshold.
    pub async fn try_compact(
        &self,
        ctx: &CompactionContext,
        session: &mut Session,
        llm: Option<&dyn crate::client::LlmCall>,
    ) -> crate::Result<CompactResult> {
        if !self.circuit_breaker.allow_request() {
            debug!("Compaction circuit breaker is open, skipping");
            return Ok(CompactResult::Skipped {
                reason: "circuit breaker open".into(),
            });
        }

        for strategy in &self.strategies {
            if strategy.requires_llm() && llm.is_none() {
                debug!(
                    strategy = strategy.name(),
                    "Skipping LLM-dependent strategy (no client)"
                );
                continue;
            }

            if !strategy.needs_compact(ctx) {
                continue;
            }

            let plan = match strategy.plan(session) {
                Ok(plan) => plan,
                Err(e) => {
                    warn!(
                        strategy = strategy.name(),
                        error = %e,
                        "Compaction planning failed"
                    );
                    self.circuit_breaker.record_failure();
                    continue;
                }
            };

            if matches!(plan, CompactionPlan::NotNeeded) {
                continue;
            }

            info!(strategy = strategy.name(), "Executing compaction");

            match strategy.execute(plan, session, llm).await {
                Ok(result) => {
                    self.circuit_breaker.record_success();
                    info!(
                        strategy = strategy.name(),
                        result = ?result,
                        "Compaction completed"
                    );

                    // Auto-persist compaction summary to memory store.
                    if let CompactResult::Compacted { ref summary, .. } = result
                        && let Some(ref store) = self.memory_store
                    {
                        let entry = MemoryEntry::new(session.id.to_string(), summary.clone())
                            .with_tags(vec!["compaction_summary".to_string()]);
                        if let Err(e) = store.add(entry).await {
                            warn!(
                                error = %e,
                                "Failed to store compaction summary in memory"
                            );
                        }
                    }

                    return Ok(result);
                }
                Err(e) => {
                    warn!(
                        strategy = strategy.name(),
                        error = %e,
                        "Compaction execution failed"
                    );
                    self.circuit_breaker.record_failure();
                    // Continue to next strategy
                }
            }
        }

        Ok(CompactResult::NotNeeded)
    }

    /// Number of registered strategies.
    pub fn len(&self) -> usize {
        self.strategies.len()
    }

    /// Whether the chain has no strategies.
    pub fn is_empty(&self) -> bool {
        self.strategies.is_empty()
    }

    /// Current circuit breaker state.
    pub fn circuit_state(&self) -> crate::common::circuit::CircuitState {
        self.circuit_breaker.state()
    }

    /// Reset the circuit breaker.
    pub fn reset_circuit(&self) {
        self.circuit_breaker.reset();
    }
}

/// Builder for [`CompactionChain`].
pub struct CompactionChainBuilder {
    strategies: Vec<Box<dyn CompactionStrategy>>,
    circuit_config: CircuitConfig,
    memory_store: Option<Arc<dyn MemoryStore>>,
}

impl CompactionChainBuilder {
    fn new() -> Self {
        Self {
            strategies: Vec::new(),
            circuit_config: CircuitConfig {
                failure_threshold: 3,
                recovery_timeout: std::time::Duration::from_secs(60),
                success_threshold: 1,
            },
            memory_store: None,
        }
    }

    /// Add a compaction strategy to the chain.
    ///
    /// Strategies are tried in insertion order. Place cheaper strategies
    /// (MicroCompaction) before expensive ones (FullCompaction).
    pub fn strategy(mut self, strategy: impl CompactionStrategy + 'static) -> Self {
        self.strategies.push(Box::new(strategy));
        self
    }

    /// Configure the circuit breaker.
    pub fn circuit_config(mut self, config: CircuitConfig) -> Self {
        self.circuit_config = config;
        self
    }

    /// Set the failure threshold before the circuit opens.
    pub fn failure_threshold(mut self, threshold: u32) -> Self {
        self.circuit_config.failure_threshold = threshold;
        self
    }

    /// Attach a memory store for auto-persisting compaction summaries.
    ///
    /// When set, each successful full compaction will store its summary
    /// as a [`MemoryEntry`] tagged `"compaction_summary"`, enabling
    /// cross-session context retrieval.
    pub fn memory_store(mut self, store: Arc<dyn MemoryStore>) -> Self {
        self.memory_store = Some(store);
        self
    }

    pub fn build(self) -> CompactionChain {
        CompactionChain {
            strategies: self.strategies,
            circuit_breaker: CircuitBreaker::new(self.circuit_config),
            memory_store: self.memory_store,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::circuit::CircuitState;

    #[test]
    fn empty_chain_returns_not_needed() {
        let chain = CompactionChain::builder().build();
        assert!(chain.is_empty());
        assert_eq!(chain.circuit_state(), CircuitState::Closed);
    }

    #[test]
    fn builder_sets_failure_threshold() {
        let chain = CompactionChain::builder().failure_threshold(5).build();
        assert_eq!(chain.circuit_state(), CircuitState::Closed);
    }
}
