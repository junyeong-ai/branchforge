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
