//! Resource limits for task execution.
//!
//! A [`TaskBudget`] travels with a [`super::TaskInput`] so the execution
//! loop can stop when any limit is reached. After execution, the
//! [`remaining()`](TaskBudget::remaining) method computes leftover budget
//! for continuation or reporting.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Resource limits for a task execution.
///
/// When any limit is reached, the task should yield control back to the
/// caller rather than continuing execution. `None` means unlimited.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskBudget {
    /// Maximum number of agent iterations (LLM calls).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    /// Maximum total tokens consumed (input + output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Maximum cost in USD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<Decimal>,
    /// Maximum wall-clock duration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_duration: Option<Duration>,
}

impl TaskBudget {
    /// Create an unlimited budget (no constraints).
    pub fn unlimited() -> Self {
        Self::default()
    }

    /// Create a budget limited by iteration count.
    pub fn iterations(max: u32) -> Self {
        Self {
            max_iterations: Some(max),
            ..Default::default()
        }
    }

    /// Create a budget limited by token count.
    pub fn tokens(max: u64) -> Self {
        Self {
            max_tokens: Some(max),
            ..Default::default()
        }
    }

    /// Check if any limit has been exceeded given current consumption.
    pub fn is_exhausted(
        &self,
        iterations: u32,
        tokens: u64,
        cost: Decimal,
        elapsed: Duration,
    ) -> bool {
        self.max_iterations.is_some_and(|max| iterations >= max)
            || self.max_tokens.is_some_and(|max| tokens >= max)
            || self.max_cost_usd.is_some_and(|max| cost >= max)
            || self.max_duration.is_some_and(|max| elapsed >= max)
    }

    /// Compute remaining budget after partial consumption.
    pub fn remaining(
        &self,
        iterations: u32,
        tokens: u64,
        cost: Decimal,
        elapsed: Duration,
    ) -> Self {
        Self {
            max_iterations: self.max_iterations.map(|m| m.saturating_sub(iterations)),
            max_tokens: self.max_tokens.map(|m| m.saturating_sub(tokens)),
            max_cost_usd: self.max_cost_usd.map(|m| (m - cost).max(Decimal::ZERO)),
            max_duration: self.max_duration.map(|m| m.saturating_sub(elapsed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn unlimited_is_never_exhausted() {
        let budget = TaskBudget::unlimited();
        assert!(!budget.is_exhausted(1000, 999_999, dec!(100.0), Duration::from_secs(9999)));
    }

    #[test]
    fn iterations_limit() {
        let budget = TaskBudget::iterations(5);
        assert!(!budget.is_exhausted(4, 0, Decimal::ZERO, Duration::ZERO));
        assert!(budget.is_exhausted(5, 0, Decimal::ZERO, Duration::ZERO));
        assert!(budget.is_exhausted(6, 0, Decimal::ZERO, Duration::ZERO));
    }

    #[test]
    fn tokens_limit() {
        let budget = TaskBudget::tokens(1000);
        assert!(!budget.is_exhausted(0, 999, Decimal::ZERO, Duration::ZERO));
        assert!(budget.is_exhausted(0, 1000, Decimal::ZERO, Duration::ZERO));
    }

    #[test]
    fn cost_limit() {
        let budget = TaskBudget {
            max_cost_usd: Some(dec!(1.50)),
            ..Default::default()
        };
        assert!(!budget.is_exhausted(0, 0, dec!(1.49), Duration::ZERO));
        assert!(budget.is_exhausted(0, 0, dec!(1.50), Duration::ZERO));
        assert!(budget.is_exhausted(0, 0, dec!(2.00), Duration::ZERO));
    }

    #[test]
    fn duration_limit() {
        let budget = TaskBudget {
            max_duration: Some(Duration::from_secs(60)),
            ..Default::default()
        };
        assert!(!budget.is_exhausted(0, 0, Decimal::ZERO, Duration::from_secs(59)));
        assert!(budget.is_exhausted(0, 0, Decimal::ZERO, Duration::from_secs(60)));
        assert!(budget.is_exhausted(0, 0, Decimal::ZERO, Duration::from_secs(61)));
    }

    #[test]
    fn multiple_limits_any_triggers_exhaustion() {
        let budget = TaskBudget {
            max_iterations: Some(10),
            max_tokens: Some(5000),
            max_cost_usd: Some(dec!(2.00)),
            max_duration: Some(Duration::from_secs(120)),
        };
        // Only iterations exceeded
        assert!(budget.is_exhausted(10, 100, dec!(0.01), Duration::from_secs(1)));
        // Only tokens exceeded
        assert!(budget.is_exhausted(1, 5000, dec!(0.01), Duration::from_secs(1)));
        // None exceeded
        assert!(!budget.is_exhausted(9, 4999, dec!(1.99), Duration::from_secs(119)));
    }

    #[test]
    fn remaining_subtracts_consumption() {
        let budget = TaskBudget {
            max_iterations: Some(10),
            max_tokens: Some(5000),
            max_cost_usd: Some(dec!(2.00)),
            max_duration: Some(Duration::from_secs(120)),
        };
        let rest = budget.remaining(3, 1500, dec!(0.75), Duration::from_secs(30));
        assert_eq!(rest.max_iterations, Some(7));
        assert_eq!(rest.max_tokens, Some(3500));
        assert_eq!(rest.max_cost_usd, Some(dec!(1.25)));
        assert_eq!(rest.max_duration, Some(Duration::from_secs(90)));
    }

    #[test]
    fn remaining_saturates_at_zero() {
        let budget = TaskBudget {
            max_iterations: Some(5),
            max_tokens: Some(1000),
            max_cost_usd: Some(dec!(1.00)),
            max_duration: Some(Duration::from_secs(60)),
        };
        let rest = budget.remaining(100, 9999, dec!(5.00), Duration::from_secs(999));
        assert_eq!(rest.max_iterations, Some(0));
        assert_eq!(rest.max_tokens, Some(0));
        assert_eq!(rest.max_cost_usd, Some(Decimal::ZERO));
        assert_eq!(rest.max_duration, Some(Duration::ZERO));
    }

    #[test]
    fn remaining_preserves_none_fields() {
        let budget = TaskBudget::iterations(10);
        let rest = budget.remaining(3, 9999, dec!(99.0), Duration::from_secs(9999));
        assert_eq!(rest.max_iterations, Some(7));
        assert_eq!(rest.max_tokens, None);
        assert_eq!(rest.max_cost_usd, None);
        assert_eq!(rest.max_duration, None);
    }

    #[test]
    fn serde_round_trip() {
        let budget = TaskBudget {
            max_iterations: Some(10),
            max_tokens: Some(5000),
            max_cost_usd: Some(dec!(2.50)),
            max_duration: Some(Duration::from_secs(120)),
        };
        let json = serde_json::to_string(&budget).unwrap();
        let deserialized: TaskBudget = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_iterations, budget.max_iterations);
        assert_eq!(deserialized.max_tokens, budget.max_tokens);
        assert_eq!(deserialized.max_cost_usd, budget.max_cost_usd);
        assert_eq!(deserialized.max_duration, budget.max_duration);
    }

    #[test]
    fn default_deserializes_from_empty_object() {
        let budget: TaskBudget = serde_json::from_str("{}").unwrap();
        assert_eq!(budget.max_iterations, None);
        assert_eq!(budget.max_tokens, None);
        assert_eq!(budget.max_cost_usd, None);
        assert_eq!(budget.max_duration, None);
    }

    #[test]
    fn skip_serializing_none_fields() {
        let budget = TaskBudget::iterations(5);
        let json = serde_json::to_string(&budget).unwrap();
        assert!(json.contains("max_iterations"));
        assert!(!json.contains("max_tokens"));
        assert!(!json.contains("max_cost_usd"));
        assert!(!json.contains("max_duration"));
    }
}
