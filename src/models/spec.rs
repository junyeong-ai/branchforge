use serde::{Deserialize, Serialize};

use super::family::ModelFamily;
use super::provider::{ProviderIds, ProviderKind};
use crate::budget::ModelPricing;

crate::string_id!(ModelId);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub id: ModelId,
    pub family: ModelFamily,
    pub version: ModelVersion,
    pub capabilities: Capabilities,
    pub pricing: ModelPricing,
    pub provider_ids: ProviderIds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelVersion {
    pub version: String,
    pub snapshot: Option<String>,
    pub knowledge_cutoff: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Capabilities {
    pub context_window: u64,
    pub extended_context_window: Option<u64>,
    pub max_output_tokens: u64,
    pub thinking: bool,
    pub vision: bool,
    pub tool_use: bool,
    pub caching: bool,
}

impl Capabilities {
    pub fn effective_context(&self, extended_enabled: bool) -> u64 {
        if extended_enabled {
            self.extended_context_window.unwrap_or(self.context_window)
        } else {
            self.context_window
        }
    }

    pub fn supports_extended_context(&self) -> bool {
        self.extended_context_window.is_some()
    }
}

pub const LONG_CONTEXT_THRESHOLD: u64 = 200_000;

impl ModelSpec {
    pub fn provider_id(&self, provider: ProviderKind) -> Option<&str> {
        self.provider_ids.for_provider(provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_pricing_standard() {
        let pricing = ModelPricing::from_base(dec!(3), dec!(15));
        // Standard pricing: context < 200K
        let cost = pricing.calculate_raw(100_000, 100_000, 0, 0);
        // input: 0.1 * 3.0 = 0.3, output: 0.1 * 15.0 = 1.5
        assert_eq!(cost, dec!(1.8));
    }

    #[test]
    fn test_pricing_large_volume() {
        let pricing = ModelPricing::from_base(dec!(3), dec!(15));
        // 1M tokens each, context = 1M > 200K, so 2x multiplier on input
        let cost = pricing.calculate_raw(1_000_000, 1_000_000, 0, 0);
        // input: 1.0 * 3.0 * 2.0 = 6.0, output: 1.0 * 15.0 = 15.0
        assert_eq!(cost, dec!(21));
    }

    #[test]
    fn test_pricing_long_context() {
        let pricing = ModelPricing::from_base(dec!(3), dec!(15));
        let cost = pricing.calculate_raw(250_000, 0, 0, 0);
        // 0.25 * 3.0 * 2.0 = 1.5
        assert_eq!(cost, dec!(1.5));
    }

    #[test]
    fn test_effective_context() {
        let caps = Capabilities {
            context_window: 200_000,
            extended_context_window: Some(1_000_000),
            max_output_tokens: 64_000,
            thinking: true,
            vision: true,
            tool_use: true,
            caching: true,
        };

        assert_eq!(caps.effective_context(false), 200_000);
        assert_eq!(caps.effective_context(true), 1_000_000);
    }
}
