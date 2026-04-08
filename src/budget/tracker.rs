//! Budget tracking for individual agent sessions.

use std::sync::atomic::{AtomicU64, Ordering};

use rust_decimal::Decimal;

use super::pricing::{PricingTable, global_pricing_table};
use super::{COST_SCALE_FACTOR, cost_to_bits};

/// Action to take when budget is exceeded.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum OnExceed {
    /// Stop execution before the next API call.
    #[default]
    StopBeforeNext,
    /// Log a warning and continue execution.
    WarnAndContinue,
    /// Switch to a cheaper model when budget is exceeded.
    FallbackModel(String),
}

impl OnExceed {
    pub fn fallback(model: impl Into<String>) -> Self {
        Self::FallbackModel(model.into())
    }

    pub fn fallback_model(&self) -> Option<&str> {
        match self {
            Self::FallbackModel(model) => Some(model),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct BudgetTracker {
    max_cost_usd: Option<Decimal>,
    used_cost_bits: AtomicU64,
    on_exceed: OnExceed,
    pricing: &'static PricingTable,
}

impl Default for BudgetTracker {
    fn default() -> Self {
        Self {
            max_cost_usd: None,
            used_cost_bits: AtomicU64::new(0),
            on_exceed: OnExceed::default(),
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

    pub fn on_exceed(mut self, on_exceed: OnExceed) -> Self {
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
        matches!(self.on_exceed, OnExceed::StopBeforeNext)
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

    pub fn on_exceed_action(&self) -> &OnExceed {
        &self.on_exceed
    }
}

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
        let tracker = BudgetTracker::new(dec!(1)).on_exceed(OnExceed::WarnAndContinue);

        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };

        tracker.record("claude-sonnet-4-5", &usage).unwrap();

        assert!(matches!(tracker.check(), BudgetStatus::Exceeded { .. }));
        assert!(!tracker.should_stop()); // WarnAndContinue doesn't stop
    }
}
