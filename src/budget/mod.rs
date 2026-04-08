//! Tenant-based budget management for API cost control.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

mod manager;
pub mod pricing;
pub mod report;
mod tracker;

pub use manager::{TenantBudget, TenantBudgetManager};
pub use pricing::{ModelPricing, PricingTable, PricingTableBuilder, global_pricing_table};
pub use report::{CostSummary, ModelCostEntry};
pub use tracker::{BudgetStatus, BudgetTracker, OnExceed};

/// Scale factor for storing Decimal costs as AtomicU64 (6 decimal places precision).
pub(crate) const COST_SCALE_FACTOR: Decimal = dec!(1_000_000);

/// Convert a `Decimal` cost into the scaled `u64` bits used for atomic
/// accumulation. Returns [`crate::Error::ResourceExhausted`] if the cost
/// exceeds the representable range (~$18.4 trillion at the current scale
/// factor) instead of silently clamping.
///
/// This is the only path through which costs enter the atomic counter, so
/// every recorded cost is guaranteed to round-trip losslessly.
pub(crate) fn cost_to_bits(cost: Decimal) -> crate::Result<u64> {
    (cost * COST_SCALE_FACTOR).try_into().map_err(|_| {
        tracing::error!(
            cost = %cost,
            "budget cost overflow: value exceeds u64 representable range"
        );
        crate::Error::ResourceExhausted(format!(
            "budget cost ${cost} exceeds representable range \
             (max ~${max} at {scale}x scale)",
            max = Decimal::from(u64::MAX) / COST_SCALE_FACTOR,
            scale = COST_SCALE_FACTOR,
        ))
    })
}
