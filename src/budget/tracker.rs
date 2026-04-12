//! Budget tracking for individual agent sessions.

use std::sync::atomic::{AtomicU64, Ordering};

use rust_decimal::Decimal;

use super::pricing::{PricingTable, global_pricing_table};
use super::{COST_SCALE_FACTOR, cost_to_bits};

/// Policy for what to do when a [`BudgetTracker`] (or
/// [`super::TenantBudget`]) detects an over-budget condition.
///
/// # Two-stage application
///
/// The agent loop applies this policy in **two stages** per
/// iteration:
///
/// 1. **Pre-build (model swap)** — at the top of the iteration the
///    runtime calls `tracker.should_fallback()` and, if it returns
///    `Some(model_id)`, swaps the request's model id to the
///    fallback before building the IR request. This is the only
///    place the `Fallback` variant has runtime effect.
/// 2. **Pre-send (preflight)** — after the request is built, the
///    runtime runs the budget preflight against the (possibly
///    already-swapped) request. The `Stop` policy fails the call,
///    `Warn` logs and proceeds, and `Fallback` is a no-op here
///    because the swap already happened in stage 1.
///
/// This split is intentional: the model swap must happen before
/// request construction (it changes which pricing table the
/// preflight uses), but the budget enforcement decision must
/// happen after construction (it sees the final estimate).
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq)]
pub enum BudgetExceedPolicy {
    /// Stop execution before the next API call. The default —
    /// fail-fast on budget overruns.
    #[default]
    Stop,
    /// Log a warning and continue execution. Useful for
    /// observability-first deployments where budget is a soft
    /// limit.
    Warn,
    /// Swap to a cheaper model when budget is exceeded. The
    /// fallback model id is captured here at type level so the
    /// invariant "fallback policy carries a model id" is enforced
    /// by the compiler.
    Fallback(String),
}

impl BudgetExceedPolicy {
    /// Construct a `Fallback` policy with the given model id.
    pub fn fallback(model: impl Into<String>) -> Self {
        Self::Fallback(model.into())
    }

    /// Returns the fallback model id, if this policy is `Fallback`.
    pub fn fallback_model(&self) -> Option<&str> {
        match self {
            Self::Fallback(model) => Some(model),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct BudgetTracker {
    max_cost_usd: Option<Decimal>,
    used_cost_bits: AtomicU64,
    on_exceed: BudgetExceedPolicy,
    pricing: &'static PricingTable,
}

impl Default for BudgetTracker {
    fn default() -> Self {
        Self {
            max_cost_usd: None,
            used_cost_bits: AtomicU64::new(0),
            on_exceed: BudgetExceedPolicy::default(),
            pricing: global_pricing_table(),
        }
    }
}

impl Clone for BudgetTracker {
    fn clone(&self) -> Self {
        Self {
            max_cost_usd: self.max_cost_usd,
            used_cost_bits: AtomicU64::new(self.used_cost_bits.load(Ordering::Relaxed)),
            on_exceed: self.on_exceed.clone(),
            pricing: self.pricing,
        }
    }
}

impl BudgetTracker {
    pub fn new(max_cost_usd: Decimal) -> Self {
        Self {
            max_cost_usd: Some(max_cost_usd),
            ..Default::default()
        }
    }

    pub fn on_exceed(mut self, on_exceed: BudgetExceedPolicy) -> Self {
        self.on_exceed = on_exceed;
        self
    }

    pub fn unlimited() -> Self {
        Self::default()
    }

    /// Record a usage observation against this tracker.
    ///
    /// Returns the computed cost on success, or
    /// [`crate::Error::ResourceExhausted`] if the cost would overflow the
    /// internal `u64` accumulator (practically unreachable, but enforced
    /// rather than silently clamped).
    pub fn record(&self, model: &str, usage: &crate::ir::Usage) -> crate::Result<Decimal> {
        let cost = self.pricing.calculate(model, usage);
        let cost_bits = cost_to_bits(cost)?;
        self.used_cost_bits.fetch_add(cost_bits, Ordering::Relaxed);
        Ok(cost)
    }

    /// Seed the tracker with cost already spent **outside** this
    /// process — used by `AgentCheckpoint` resume to rehydrate a
    /// fresh tracker with the accumulated spend from a prior run.
    ///
    /// Returns [`crate::Error::ResourceExhausted`] on overflow, just
    /// like [`Self::record`]. Callers should invoke this **once**,
    /// right after constructing the tracker and before any
    /// [`Self::record`] calls, so the baseline is established before
    /// new usage is accumulated on top.
    pub fn restore_spent(&self, cost: Decimal) -> crate::Result<()> {
        let bits = cost_to_bits(cost)?;
        self.used_cost_bits.fetch_add(bits, Ordering::Relaxed);
        Ok(())
    }

    fn used_cost_usd_internal(&self) -> Decimal {
        Decimal::from(self.used_cost_bits.load(Ordering::Relaxed)) / COST_SCALE_FACTOR
    }

    pub fn check(&self) -> BudgetStatus {
        let used = self.used_cost_usd_internal();
        match self.max_cost_usd {
            None => BudgetStatus::Unlimited { used },
            Some(max) if used >= max => BudgetStatus::Exceeded {
                used,
                limit: max,
                overage: used - max,
            },
            Some(max) => BudgetStatus::WithinBudget {
                used,
                limit: max,
                remaining: max - used,
            },
        }
    }

    pub fn should_stop(&self) -> bool {
        matches!(self.on_exceed, BudgetExceedPolicy::Stop)
            && matches!(self.check(), BudgetStatus::Exceeded { .. })
    }

    pub fn should_fallback(&self) -> Option<&str> {
        if matches!(self.check(), BudgetStatus::Exceeded { .. }) {
            self.on_exceed.fallback_model()
        } else {
            None
        }
    }

    pub fn used_cost_usd(&self) -> Decimal {
        self.used_cost_usd_internal()
    }

    pub fn remaining(&self) -> Option<Decimal> {
        self.max_cost_usd
            .map(|max| (max - self.used_cost_usd_internal()).max(Decimal::ZERO))
    }

    pub fn on_exceed_action(&self) -> &BudgetExceedPolicy {
        &self.on_exceed
    }

    /// Compute the cost of an estimated-token request against this
    /// tracker's pricing table. Pure — does not mutate the tracker.
    ///
    /// Used by the preflight path to decide whether a request would
    /// push `used + estimated_cost` past the configured limit before
    /// the request is sent.
    pub fn estimate_cost(&self, model: &str, estimate: super::RequestTokenEstimate) -> Decimal {
        let pricing = self.pricing.get(model);
        pricing.calculate_raw(estimate.input, estimate.output, 0, 0)
    }

    /// Check whether adding `estimated_cost` to the current usage
    /// would exceed the tracker's limit.
    ///
    /// Returns:
    /// - `None` if the tracker is unlimited.
    /// - `Some(Ok(projected))` if the call is within budget. The
    ///   projected total (`used + estimate`) is returned for
    ///   telemetry.
    /// - `Some(Err((used, limit)))` if the call would overrun.
    ///   Callers translate this into [`crate::Error::BudgetExceeded`]
    ///   when `on_exceed_action()` is [`BudgetExceedPolicy::Stop`].
    pub fn project(
        &self,
        estimated_cost: Decimal,
    ) -> Option<std::result::Result<Decimal, (Decimal, Decimal)>> {
        let used = self.used_cost_usd_internal();
        let max = self.max_cost_usd?;
        let projected = used + estimated_cost;
        if projected > max {
            Some(Err((used, max)))
        } else {
            Some(Ok(projected))
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum BudgetStatus {
    Unlimited {
        used: Decimal,
    },
    WithinBudget {
        used: Decimal,
        limit: Decimal,
        remaining: Decimal,
    },
    Exceeded {
        used: Decimal,
        limit: Decimal,
        overage: Decimal,
    },
}

impl BudgetStatus {
    pub fn is_exceeded(&self) -> bool {
        matches!(self, Self::Exceeded { .. })
    }

    pub fn used(&self) -> Decimal {
        match self {
            Self::Unlimited { used } => *used,
            Self::WithinBudget { used, .. } => *used,
            Self::Exceeded { used, .. } => *used,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Usage;
    use rust_decimal_macros::dec;

    #[test]
    fn test_budget_tracking() {
        let tracker = BudgetTracker::new(dec!(10));

        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 50_000,
            ..Default::default()
        };

        // Sonnet: 0.1M * $3 + 0.05M * $15 = $0.30 + $0.75 = $1.05
        let cost = tracker.record("claude-sonnet-4-5", &usage).unwrap();
        assert_eq!(cost, dec!(1.05));
        assert!(!tracker.should_stop());

        // Add more usage to exceed budget
        for _ in 0..10 {
            tracker.record("claude-sonnet-4-5", &usage).unwrap();
        }

        assert!(tracker.should_stop());
        assert!(matches!(tracker.check(), BudgetStatus::Exceeded { .. }));
    }

    #[test]
    fn test_unlimited_budget() {
        let tracker = BudgetTracker::unlimited();

        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };

        for _ in 0..100 {
            tracker.record("claude-opus-4-6", &usage).unwrap();
        }

        assert!(!tracker.should_stop());
        assert!(matches!(tracker.check(), BudgetStatus::Unlimited { .. }));
    }

    #[test]
    fn test_warn_and_continue() {
        let tracker = BudgetTracker::new(dec!(1)).on_exceed(BudgetExceedPolicy::Warn);

        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };

        tracker.record("claude-sonnet-4-5", &usage).unwrap();

        assert!(matches!(tracker.check(), BudgetStatus::Exceeded { .. }));
        assert!(!tracker.should_stop()); // WarnAndContinue doesn't stop
    }

    /// B-1 checkpoint/resume contract: a fresh tracker seeded with
    /// `restore_spent` plus one additional `record` must match the
    /// total cost a single tracker would have after two records.
    /// Proves the restore path is the identity element for the
    /// `record` accumulator — no rounding, no drift.
    #[test]
    fn restore_spent_resumes_accumulator() {
        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 50_000,
            ..Default::default()
        };

        // Baseline: single tracker records the full usage twice.
        let live = BudgetTracker::new(dec!(10));
        live.record("claude-sonnet-4-5", &usage).unwrap();
        live.record("claude-sonnet-4-5", &usage).unwrap();
        let live_total = live.used_cost_usd();

        // Simulated restart: the first tracker is dropped, a new one
        // is built and seeded with the first tracker's cost, then
        // the second record arrives.
        let before_checkpoint = BudgetTracker::new(dec!(10));
        before_checkpoint
            .record("claude-sonnet-4-5", &usage)
            .unwrap();
        let checkpointed = before_checkpoint.used_cost_usd();

        let after_restart = BudgetTracker::new(dec!(10));
        after_restart.restore_spent(checkpointed).unwrap();
        after_restart.record("claude-sonnet-4-5", &usage).unwrap();

        assert_eq!(after_restart.used_cost_usd(), live_total);
    }

    /// `restore_spent` must cooperate with the Stop policy: if the
    /// seeded cost already exceeds the limit, the next `check()`
    /// must report Exceeded even before a fresh `record` is made.
    #[test]
    fn restore_spent_triggers_exceeded_without_new_record() {
        let tracker = BudgetTracker::new(dec!(5));
        tracker.restore_spent(dec!(6)).unwrap();
        assert!(matches!(tracker.check(), BudgetStatus::Exceeded { .. }));
        assert!(tracker.should_stop());
    }
}
