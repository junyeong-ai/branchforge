//! Multi-tenant budget management.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use rust_decimal::Decimal;

use super::pricing::global_pricing_table;
use super::tracker::BudgetExceedPolicy;
use super::{COST_SCALE_FACTOR, cost_to_bits};

#[derive(Debug)]
pub struct TenantBudget {
    pub tenant_id: String,
    max_cost_usd: Decimal,
    used_cost_usd: AtomicU64,
    on_exceed: BudgetExceedPolicy,
}

impl TenantBudget {
    fn new(tenant_id: impl Into<String>, max_cost_usd: Decimal) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            max_cost_usd,
            used_cost_usd: AtomicU64::new(0),
            on_exceed: BudgetExceedPolicy::Stop,
        }
    }

    pub fn on_exceed(mut self, on_exceed: BudgetExceedPolicy) -> Self {
        self.on_exceed = on_exceed;
        self
    }

    /// Record a usage observation against this tenant's budget.
    ///
    /// Returns the computed cost on success, or
    /// [`crate::Error::ResourceExhausted`] if the cost would overflow the
    /// internal `u64` accumulator.
    pub fn record(&self, model: &str, usage: &crate::ir::Usage) -> crate::Result<Decimal> {
        let cost = global_pricing_table().calculate(model, usage);
        let cost_bits = cost_to_bits(cost)?;
        self.used_cost_usd.fetch_add(cost_bits, Ordering::Relaxed);
        Ok(cost)
    }

    pub fn used_cost_usd(&self) -> Decimal {
        Decimal::from(self.used_cost_usd.load(Ordering::Relaxed)) / COST_SCALE_FACTOR
    }

    pub fn remaining(&self) -> Decimal {
        (self.max_cost_usd - self.used_cost_usd()).max(Decimal::ZERO)
    }

    pub fn is_exceeded(&self) -> bool {
        self.used_cost_usd() >= self.max_cost_usd
    }

    pub fn should_stop(&self) -> bool {
        matches!(self.on_exceed, BudgetExceedPolicy::Stop) && self.is_exceeded()
    }

    pub fn max_cost_usd(&self) -> Decimal {
        self.max_cost_usd
    }

    pub fn on_exceed_action(&self) -> &BudgetExceedPolicy {
        &self.on_exceed
    }

    /// Check whether adding `estimated_cost` to the tenant's current
    /// usage would exceed its limit. See
    /// [`crate::budget::BudgetTracker::project`] for the contract.
    pub fn project(
        &self,
        estimated_cost: Decimal,
    ) -> std::result::Result<Decimal, (Decimal, Decimal)> {
        let used = self.used_cost_usd();
        let projected = used + estimated_cost;
        if projected > self.max_cost_usd {
            Err((used, self.max_cost_usd))
        } else {
            Ok(projected)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TenantBudgetManager {
    budgets: Arc<DashMap<String, Arc<TenantBudget>>>,
}

impl TenantBudgetManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_budget(
        &self,
        tenant_id: impl Into<String>,
        max_cost_usd: Decimal,
    ) -> Arc<TenantBudget> {
        let tenant_id = tenant_id.into();
        let budget = Arc::new(TenantBudget::new(tenant_id.clone(), max_cost_usd));
        self.budgets.insert(tenant_id, Arc::clone(&budget));
        budget
    }

    pub fn set_budget_with_options(
        &self,
        tenant_id: impl Into<String>,
        max_cost_usd: Decimal,
        on_exceed: BudgetExceedPolicy,
    ) -> Arc<TenantBudget> {
        let tenant_id = tenant_id.into();
        let budget =
            Arc::new(TenantBudget::new(tenant_id.clone(), max_cost_usd).on_exceed(on_exceed));
        self.budgets.insert(tenant_id, Arc::clone(&budget));
        budget
    }

    pub fn get(&self, tenant_id: &str) -> Option<Arc<TenantBudget>> {
        self.budgets.get(tenant_id).map(|v| Arc::clone(&v))
    }

    /// Record usage for a known tenant.
    ///
    /// Returns `None` if the tenant is unknown, otherwise the result of
    /// [`TenantBudget::record`] (cost on success, or
    /// [`crate::Error::ResourceExhausted`] on overflow).
    pub fn record(
        &self,
        tenant_id: &str,
        model: &str,
        usage: &crate::ir::Usage,
    ) -> Option<crate::Result<Decimal>> {
        self.budgets
            .get(tenant_id)
            .map(|budget| budget.record(model, usage))
    }

    pub fn should_stop(&self, tenant_id: &str) -> bool {
        self.budgets
            .get(tenant_id)
            .map(|b| b.should_stop())
            .unwrap_or(false)
    }

    pub fn remove(&self, tenant_id: &str) -> Option<Arc<TenantBudget>> {
        self.budgets.remove(tenant_id).map(|(_, v)| v)
    }

    pub fn tenant_ids(&self) -> Vec<String> {
        self.budgets.iter().map(|e| e.key().clone()).collect()
    }

    pub fn summary(&self) -> Vec<TenantBudgetSummary> {
        self.budgets
            .iter()
            .map(|e| TenantBudgetSummary {
                tenant_id: e.key().clone(),
                max_cost_usd: e.value().max_cost_usd(),
                used_cost_usd: e.value().used_cost_usd(),
                remaining: e.value().remaining(),
                is_exceeded: e.value().is_exceeded(),
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct TenantBudgetSummary {
    pub tenant_id: String,
    pub max_cost_usd: Decimal,
    pub used_cost_usd: Decimal,
    pub remaining: Decimal,
    pub is_exceeded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Usage;
    use rust_decimal_macros::dec;

    #[test]
    fn test_tenant_budget_manager() {
        let manager = TenantBudgetManager::new();

        manager.set_budget("tenant-a", dec!(100));
        manager.set_budget("tenant-b", dec!(50));

        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 50_000,
            ..Default::default()
        };

        // tenant-a: 0.1M * $3 + 0.05M * $15 = $0.3 + $0.75 = $1.05
        let cost = manager.record("tenant-a", "claude-sonnet-4-5", &usage);
        assert_eq!(cost.unwrap().unwrap(), dec!(1.05));

        let budget_a = manager.get("tenant-a").unwrap();
        assert_eq!(budget_a.used_cost_usd(), dec!(1.05));
        assert!(!budget_a.is_exceeded());

        // tenant-b unaffected
        let budget_b = manager.get("tenant-b").unwrap();
        assert_eq!(budget_b.used_cost_usd(), Decimal::ZERO);
    }

    #[test]
    fn test_concurrent_updates() {
        use std::thread;

        let manager = TenantBudgetManager::new();
        manager.set_budget("tenant-concurrent", dec!(10000));

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let m = manager.clone();
                thread::spawn(move || {
                    let usage = Usage {
                        input_tokens: 100_000,
                        output_tokens: 50_000,
                        ..Default::default()
                    };
                    for _ in 0..100 {
                        m.record("tenant-concurrent", "claude-sonnet-4-5", &usage)
                            .unwrap()
                            .unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let budget = manager.get("tenant-concurrent").unwrap();
        // 1000 calls * $1.05 = $1050
        assert_eq!(budget.used_cost_usd(), dec!(1050));
    }

    #[test]
    fn test_budget_exceeded() {
        let manager = TenantBudgetManager::new();
        manager.set_budget("small-budget", dec!(5));

        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            ..Default::default()
        };

        // First call: ~$13.5 (long context), exceeds $5 limit
        manager
            .record("small-budget", "claude-sonnet-4-5", &usage)
            .unwrap()
            .unwrap();

        assert!(manager.should_stop("small-budget"));
    }
}
