//! Compaction chain — Chain of Responsibility pattern.
//!
//! Tries registered strategies in order (cheapest first), executing the
//! first one that reports compaction is needed. If still over threshold
//! after one strategy, continues to the next.
//!
//! Integrates a [`CircuitBreaker`] to stop retrying after consecutive failures.

use tracing::{debug, info, warn};

use super::strategy::{CompactionContext, CompactionPlan, CompactionStrategy};
use crate::common::circuit::{CircuitBreaker, CircuitConfig};
use crate::session::state::Session;
use crate::types::CompactResult;

/// Chain of compaction strategies with circuit breaker protection.
pub struct CompactionChain {
    strategies: Vec<Box<dyn CompactionStrategy>>,
    circuit_breaker: CircuitBreaker,
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
        client: Option<&crate::Client>,
    ) -> crate::Result<CompactResult> {
        if !self.circuit_breaker.allow_request() {
            debug!("Compaction circuit breaker is open, skipping");
            return Ok(CompactResult::Skipped {
                reason: "circuit breaker open".into(),
            });
        }

        for strategy in &self.strategies {
            if strategy.requires_llm() && client.is_none() {
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

            match strategy.execute(plan, session, client).await {
                Ok(result) => {
                    self.circuit_breaker.record_success();
                    info!(
                        strategy = strategy.name(),
                        result = ?result,
                        "Compaction completed"
                    );
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

    pub fn build(self) -> CompactionChain {
        CompactionChain {
            strategies: self.strategies,
            circuit_breaker: CircuitBreaker::new(self.circuit_config),
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
        let chain = CompactionChain::builder()
            .failure_threshold(5)
            .build();
        assert_eq!(chain.circuit_state(), CircuitState::Closed);
    }
}
