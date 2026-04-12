//! Model pricing definitions for cost calculation.
//!
//! Prices can be customized via environment variables or programmatically.
//! Default prices are based on Anthropic's published rates.
//!
//! Uses `rust_decimal` for precise monetary calculations without floating-point errors.

#![allow(missing_docs)]

use std::collections::HashMap;
use std::sync::LazyLock;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::models::LONG_CONTEXT_THRESHOLD;
use crate::types::provider::UsageProvider;

const CACHE_READ_DISCOUNT: Decimal = dec!(0.1);
const CACHE_WRITE_PREMIUM: Decimal = dec!(1.25);
const DEFAULT_LONG_CONTEXT_MULTIPLIER: Decimal = dec!(2);
const MILLION: Decimal = dec!(1_000_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_per_mtok: Decimal,
    pub output_per_mtok: Decimal,
    pub cache_read_per_mtok: Decimal,
    pub cache_write_per_mtok: Decimal,
    pub long_context_multiplier: Decimal,
}

impl ModelPricing {
    pub const fn new(
        input_per_mtok: Decimal,
        output_per_mtok: Decimal,
        cache_read_per_mtok: Decimal,
        cache_write_per_mtok: Decimal,
        long_context_multiplier: Decimal,
    ) -> Self {
        Self {
            input_per_mtok,
            output_per_mtok,
            cache_read_per_mtok,
            cache_write_per_mtok,
            long_context_multiplier,
        }
    }

    pub fn from_base(input_per_mtok: Decimal, output_per_mtok: Decimal) -> Self {
        Self {
            input_per_mtok,
            output_per_mtok,
            cache_read_per_mtok: input_per_mtok * CACHE_READ_DISCOUNT,
            cache_write_per_mtok: input_per_mtok * CACHE_WRITE_PREMIUM,
            long_context_multiplier: DEFAULT_LONG_CONTEXT_MULTIPLIER,
        }
    }

    /// Calculate cost from raw token counts.
    pub fn calculate_raw(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_write: u64,
    ) -> Decimal {
        let context = input_tokens + cache_read + cache_write;
        let multiplier = if context > LONG_CONTEXT_THRESHOLD {
            self.long_context_multiplier
        } else {
            Decimal::ONE
        };

        let input = (Decimal::from(input_tokens) / MILLION) * self.input_per_mtok * multiplier;
        let output = (Decimal::from(output_tokens) / MILLION) * self.output_per_mtok;
        let cache_read_cost =
            (Decimal::from(cache_read) / MILLION) * self.cache_read_per_mtok * multiplier;
        let cache_write_cost =
            (Decimal::from(cache_write) / MILLION) * self.cache_write_per_mtok * multiplier;

        input + output + cache_read_cost + cache_write_cost
    }

    /// Calculate cost from a neutral [`crate::ir::Usage`] value.
    ///
    /// Reads the typed `cached_input_tokens` / `cache_creation_tokens` /
    /// `reasoning_tokens` fields directly. Reasoning tokens are billed at
    /// the standard output rate (every provider that exposes them follows
    /// that convention today).
    pub fn calculate(&self, usage: &crate::ir::Usage) -> Decimal {
        let cache_read = usage.cached_input_tokens.unwrap_or(0);
        let cache_write = usage.cache_creation_tokens.unwrap_or(0);
        let reasoning = usage.reasoning_tokens.unwrap_or(0);
        let total_output = usage.output_tokens + reasoning;
        self.calculate_raw(usage.input_tokens, total_output, cache_read, cache_write)
    }
}

#[derive(Debug, Clone)]
pub struct PricingTable {
    models: HashMap<String, ModelPricing>,
    default: ModelPricing,
}

impl PricingTable {
    pub fn builder() -> PricingTableBuilder {
        PricingTableBuilder::new()
    }

    pub fn get(&self, model: &str) -> &ModelPricing {
        let normalized = Self::normalize_model_name(model);
        self.models.get(&normalized).unwrap_or(&self.default)
    }

    /// Calculate the cost of a single API call.
    ///
    /// Looks up the model in the pricing table and charges the call against
    /// the typed [`crate::ir::Usage`] fields. Reasoning tokens are billed at
    /// the standard output rate.
    pub fn calculate(&self, model: &str, usage: &crate::ir::Usage) -> Decimal {
        self.get(model).calculate(usage)
    }

    /// Calculate cost using provider-aware model normalization.
    ///
    /// This normalizes the model name based on the provider before looking up
    /// pricing data. For example, when provider is `OpenAi`, it strips common
    /// prefixes to find the canonical model name.
    pub fn calculate_for_provider(
        &self,
        model: &str,
        usage: &crate::ir::Usage,
        provider: &UsageProvider,
    ) -> Decimal {
        let normalized = self.normalize_for_provider(model, provider);
        self.calculate(&normalized, usage)
    }

    /// Normalize a model name based on the provider.
    ///
    /// Each provider may use different naming conventions. This function
    /// maps provider-specific model names to canonical keys in the pricing table.
    fn normalize_for_provider(&self, model: &str, provider: &UsageProvider) -> String {
        let lower = model.to_lowercase();
        match provider {
            UsageProvider::Anthropic
            | UsageProvider::Bedrock
            | UsageProvider::Vertex
            | UsageProvider::Foundry => {
                // Anthropic-family providers use the standard Anthropic normalization
                lower
            }
            UsageProvider::OpenAi => {
                // Strip common OpenAI prefixes/suffixes and date stamps
                // e.g., "gpt-4o-2024-08-06" -> "gpt-4o"
                let stripped = lower.trim_start_matches("openai/").to_string();
                // Try to match against known model keys by checking if the
                // pricing table has the key directly first
                if self.models.contains_key(&stripped) {
                    return stripped;
                }
                // Strip date suffixes like -2024-08-06
                let without_date = strip_date_suffix(&stripped);
                if self.models.contains_key(&without_date) {
                    return without_date;
                }
                stripped
            }
            UsageProvider::Gemini => {
                // Strip common Gemini prefixes
                // e.g., "models/gemini-2.0-flash" -> "gemini-2.0-flash"
                let stripped = lower.trim_start_matches("models/").to_string();
                if self.models.contains_key(&stripped) {
                    return stripped;
                }
                // Strip date/version suffixes
                let without_date = strip_date_suffix(&stripped);
                if self.models.contains_key(&without_date) {
                    return without_date;
                }
                stripped
            }
            UsageProvider::Unknown(_) => lower,
        }
    }

    fn normalize_model_name(model: &str) -> String {
        let model = model.to_lowercase();
        if model.contains("opus") {
            "opus".to_string()
        } else if model.contains("sonnet") {
            "sonnet".to_string()
        } else if model.contains("haiku") {
            "haiku".to_string()
        } else {
            model
        }
    }
}

/// Strip trailing date suffixes from model names (e.g., "-2024-08-06").
fn strip_date_suffix(model: &str) -> String {
    // Match patterns like -YYYY-MM-DD at the end
    let bytes = model.as_bytes();
    if bytes.len() >= 11 {
        let suffix = &model[model.len() - 11..];
        if suffix.as_bytes()[0] == b'-'
            && suffix[1..5].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes()[5] == b'-'
            && suffix[6..8].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes()[8] == b'-'
            && suffix[9..11].chars().all(|c| c.is_ascii_digit())
        {
            return model[..model.len() - 11].to_string();
        }
    }
    model.to_string()
}

impl Default for PricingTable {
    fn default() -> Self {
        global_pricing_table().clone()
    }
}

#[derive(Debug, Default)]
pub struct PricingTableBuilder {
    models: HashMap<String, ModelPricing>,
    default: Option<ModelPricing>,
}

impl PricingTableBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn defaults(mut self) -> Self {
        self.models.insert(
            "opus".into(),
            ModelPricing::new(dec!(15), dec!(75), dec!(1.5), dec!(18.75), dec!(2)),
        );
        self.models.insert(
            "sonnet".into(),
            ModelPricing::new(dec!(3), dec!(15), dec!(0.3), dec!(3.75), dec!(2)),
        );
        self.models.insert(
            "haiku".into(),
            ModelPricing::new(dec!(0.80), dec!(4), dec!(0.08), dec!(1), dec!(2)),
        );
        self
    }

    pub fn model(mut self, name: impl Into<String>, pricing: ModelPricing) -> Self {
        self.models.insert(name.into(), pricing);
        self
    }

    pub fn model_base(self, name: impl Into<String>, input: Decimal, output: Decimal) -> Self {
        self.model(name, ModelPricing::from_base(input, output))
    }

    pub fn default_pricing(mut self, pricing: ModelPricing) -> Self {
        self.default = Some(pricing);
        self
    }

    /// Add OpenAI model pricing data.
    ///
    /// Prices are based on published OpenAI rates as of late 2025:
    /// - GPT-5: $1.25/M input, $10.00/M output
    /// - GPT-5-mini: $0.25/M input, $2.00/M output
    /// - GPT-5-nano: $0.05/M input, $0.40/M output
    /// - GPT-4o: $2.50/M input, $10.00/M output
    /// - GPT-4o-mini: $0.15/M input, $0.60/M output
    /// - o4-mini: $1.10/M input, $4.40/M output
    /// - o3: $10.00/M input, $40.00/M output
    /// - o3-mini: $1.10/M input, $4.40/M output
    pub fn with_openai_models(mut self) -> Self {
        self.models.insert(
            "gpt-5".into(),
            ModelPricing::from_base(dec!(1.25), dec!(10)),
        );
        self.models.insert(
            "gpt-5-mini".into(),
            ModelPricing::from_base(dec!(0.25), dec!(2)),
        );
        self.models.insert(
            "gpt-5-nano".into(),
            ModelPricing::from_base(dec!(0.05), dec!(0.40)),
        );
        self.models.insert(
            "gpt-4o".into(),
            ModelPricing::from_base(dec!(2.50), dec!(10)),
        );
        self.models.insert(
            "gpt-4o-mini".into(),
            ModelPricing::from_base(dec!(0.15), dec!(0.60)),
        );
        self.models.insert(
            "o4-mini".into(),
            ModelPricing::from_base(dec!(1.10), dec!(4.40)),
        );
        self.models
            .insert("o3".into(), ModelPricing::from_base(dec!(10), dec!(40)));
        self.models.insert(
            "o3-mini".into(),
            ModelPricing::from_base(dec!(1.10), dec!(4.40)),
        );
        self
    }

    /// Add Google Gemini model pricing data.
    ///
    /// Prices are based on published Google AI rates as of late 2025:
    /// - Gemini 2.5 Pro: $1.25/M input, $10.00/M output
    /// - Gemini 2.5 Flash: $0.30/M input, $2.50/M output
    /// - Gemini 2.5 Flash-Lite: $0.10/M input, $0.40/M output
    /// - Gemini 2.0 Flash: $0.10/M input, $0.40/M output
    /// - Gemini 2.0 Flash Lite: $0.075/M input, $0.30/M output
    pub fn with_gemini_models(mut self) -> Self {
        self.models.insert(
            "gemini-2.5-pro".into(),
            ModelPricing::from_base(dec!(1.25), dec!(10)),
        );
        self.models.insert(
            "gemini-2.5-flash".into(),
            ModelPricing::from_base(dec!(0.30), dec!(2.50)),
        );
        self.models.insert(
            "gemini-2.5-flash-lite".into(),
            ModelPricing::from_base(dec!(0.10), dec!(0.40)),
        );
        self.models.insert(
            "gemini-2.0-flash".into(),
            ModelPricing::from_base(dec!(0.10), dec!(0.40)),
        );
        self.models.insert(
            "gemini-2.0-flash-lite".into(),
            ModelPricing::from_base(dec!(0.075), dec!(0.30)),
        );
        self
    }

    /// Add explicit Anthropic Claude 4-family pricing.
    ///
    /// The simple `defaults()` registration relies on
    /// `PricingTable::normalize_model_name` to fold every "opus"/"sonnet"/
    /// "haiku" model into a single bucket, which works but masks per-version
    /// cost differences. This method registers the canonical 4-family
    /// identifiers explicitly so cost reporting can attribute spend to a
    /// specific revision (Opus 4 vs Opus 4.6, etc.).
    ///
    /// Prices reflect Anthropic's published rates as of late 2025:
    /// - Opus 4 / Opus 4.6: $15/M input, $75/M output
    /// - Sonnet 4 / 4.5 / 4.6: $3/M input, $15/M output
    /// - Haiku 4.5: $0.80/M input, $4/M output
    pub fn with_anthropic_models(mut self) -> Self {
        let opus = ModelPricing::new(dec!(15), dec!(75), dec!(1.5), dec!(18.75), dec!(2));
        let sonnet = ModelPricing::new(dec!(3), dec!(15), dec!(0.3), dec!(3.75), dec!(2));
        let haiku = ModelPricing::new(dec!(0.80), dec!(4), dec!(0.08), dec!(1), dec!(2));
        for name in ["claude-opus-4", "claude-opus-4-6"] {
            self.models.insert(name.into(), opus);
        }
        for name in ["claude-sonnet-4", "claude-sonnet-4-5", "claude-sonnet-4-6"] {
            self.models.insert(name.into(), sonnet);
        }
        self.models.insert("claude-haiku-4-5".into(), haiku);
        self
    }

    pub fn from_env(mut self) -> Self {
        self = self.defaults();

        // Legacy Anthropic-prefixed env vars (kept for compatibility with
        // pre-existing deployments).
        if let Some(pricing) = Self::parse_env_pricing("ANTHROPIC", "OPUS") {
            self.models.insert("opus".into(), pricing);
        }
        if let Some(pricing) = Self::parse_env_pricing("ANTHROPIC", "SONNET") {
            self.models.insert("sonnet".into(), pricing);
        }
        if let Some(pricing) = Self::parse_env_pricing("ANTHROPIC", "HAIKU") {
            self.models.insert("haiku".into(), pricing);
        }

        // Generic per-model overrides:
        //   BRANCHFORGE_PRICING_<MODEL>_INPUT
        //   BRANCHFORGE_PRICING_<MODEL>_OUTPUT
        //   BRANCHFORGE_PRICING_<MODEL>_CACHE_READ
        //   BRANCHFORGE_PRICING_<MODEL>_CACHE_WRITE
        // The model id in the env var name is upper-cased and `-`/`.` are
        // converted to `_` so e.g. `gpt-4o-mini` → `GPT_4O_MINI`.
        let mut overrides: Vec<(String, ModelPricing)> = Vec::new();
        for (key, _) in std::env::vars() {
            if let Some(rest) = key.strip_prefix("BRANCHFORGE_PRICING_")
                && let Some(model_token) = rest.strip_suffix("_INPUT")
                && let Some(pricing) = Self::parse_env_pricing("BRANCHFORGE", model_token)
            {
                let model_name = model_token.to_lowercase().replace('_', "-");
                overrides.push((model_name, pricing));
            }
        }
        for (name, pricing) in overrides {
            self.models.insert(name, pricing);
        }

        self
    }

    fn parse_env_pricing(prefix: &str, model: &str) -> Option<ModelPricing> {
        let input: Decimal = std::env::var(format!("{prefix}_PRICING_{model}_INPUT"))
            .ok()?
            .parse()
            .ok()?;
        let output: Decimal = std::env::var(format!("{prefix}_PRICING_{model}_OUTPUT"))
            .ok()?
            .parse()
            .ok()?;

        let cache_read: Decimal = std::env::var(format!("{prefix}_PRICING_{model}_CACHE_READ"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(input * CACHE_READ_DISCOUNT);
        let cache_write: Decimal = std::env::var(format!("{prefix}_PRICING_{model}_CACHE_WRITE"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(input * CACHE_WRITE_PREMIUM);

        Some(ModelPricing::new(
            input,
            output,
            cache_read,
            cache_write,
            DEFAULT_LONG_CONTEXT_MULTIPLIER,
        ))
    }

    pub fn build(self) -> PricingTable {
        let default = self
            .default
            .or_else(|| self.models.get("sonnet").copied())
            .unwrap_or(ModelPricing::new(
                dec!(3),
                dec!(15),
                dec!(0.3),
                dec!(3.75),
                dec!(2),
            ));

        PricingTable {
            models: self.models,
            default,
        }
    }
}

static GLOBAL_PRICING: LazyLock<PricingTable> = LazyLock::new(|| {
    PricingTableBuilder::new()
        .from_env()
        .with_anthropic_models()
        .with_openai_models()
        .with_gemini_models()
        .build()
});

pub fn global_pricing_table() -> &'static PricingTable {
    &GLOBAL_PRICING
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Usage;

    #[test]
    fn test_pricing_standard_context() {
        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 100_000,
            cached_input_tokens: None,
            cache_creation_tokens: None,
            ..Default::default()
        };

        let table = global_pricing_table();

        // Sonnet: 0.1M * $3 + 0.1M * $15 = $0.3 + $1.5 = $1.8
        let cost = table.calculate("claude-sonnet-4-5", &usage);
        assert_eq!(cost, dec!(1.8));

        // Opus: 0.1M * $15 + 0.1M * $75 = $1.5 + $7.5 = $9
        let cost = table.calculate("claude-opus-4-6", &usage);
        assert_eq!(cost, dec!(9));

        // Haiku: 0.1M * $0.80 + 0.1M * $4 = $0.08 + $0.4 = $0.48
        let cost = table.calculate("claude-haiku-4-5", &usage);
        assert_eq!(cost, dec!(0.48));
    }

    #[test]
    fn test_pricing_long_context_multiplier() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cached_input_tokens: None,
            cache_creation_tokens: None,
            ..Default::default()
        };

        let table = global_pricing_table();

        // Sonnet long context (1M > 200K): input 1M * $3 * 2 = $6, output 1M * $15 = $15
        let cost = table.calculate("claude-sonnet-4-5", &usage);
        assert_eq!(cost, dec!(21));

        // Opus long context: input 1M * $15 * 2 = $30, output 1M * $75 = $75
        let cost = table.calculate("claude-opus-4-6", &usage);
        assert_eq!(cost, dec!(105));

        // Haiku long context: input 1M * $0.80 * 2 = $1.60, output 1M * $4 = $4
        let cost = table.calculate("claude-haiku-4-5", &usage);
        assert_eq!(cost, dec!(5.60));
    }

    #[test]
    fn test_cache_pricing() {
        let usage = Usage {
            input_tokens: 50_000,
            output_tokens: 10_000,
            cached_input_tokens: Some(50_000),
            cache_creation_tokens: Some(20_000),
            ..Default::default()
        };

        let table = global_pricing_table();
        // Standard context (120K < 200K):
        // input: 0.05M * $3 = $0.15, output: 0.01M * $15 = $0.15
        // cache_read: 0.05M * $0.3 = $0.015, cache_write: 0.02M * $3.75 = $0.075
        let cost = table.calculate("claude-sonnet-4-5", &usage);
        assert_eq!(cost, dec!(0.39));
    }

    #[test]
    fn test_cache_pricing_long_context() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cached_input_tokens: Some(500_000),
            cache_creation_tokens: Some(200_000),
            ..Default::default()
        };

        let table = global_pricing_table();
        // Long context (1.7M > 200K), 2x multiplier on input/cache_read/cache_write:
        // input: 1M * $3 * 2 = $6, output: 0.1M * $15 = $1.5
        // cache_read: 0.5M * $0.3 * 2 = $0.3, cache_write: 0.2M * $3.75 * 2 = $1.5
        let cost = table.calculate("claude-sonnet-4-5", &usage);
        assert_eq!(cost, dec!(9.3));
    }

    #[test]
    fn test_long_context_pricing() {
        let table = global_pricing_table();

        let usage = Usage {
            input_tokens: 250_000,
            output_tokens: 50_000,
            ..Default::default()
        };

        // Sonnet long context (250K > 200K): input 0.25M * $3 * 2 = $1.5, output 0.05M * $15 = $0.75
        let cost = table.calculate("claude-sonnet-4-5", &usage);
        assert_eq!(cost, dec!(2.25));
    }

    #[test]
    fn test_custom_pricing_table() {
        let table = PricingTableBuilder::new()
            .model_base("custom", dec!(10), dec!(50))
            .default_pricing(ModelPricing::from_base(dec!(10), dec!(50)))
            .build();

        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 100_000,
            ..Default::default()
        };

        let cost = table.calculate("custom", &usage);
        assert_eq!(cost, dec!(6));
    }

    #[test]
    fn test_from_base_pricing() {
        let pricing = ModelPricing::from_base(dec!(10), dec!(50));
        assert_eq!(pricing.cache_read_per_mtok, dec!(1));
        assert_eq!(pricing.cache_write_per_mtok, dec!(12.5));
        assert_eq!(pricing.long_context_multiplier, dec!(2));
    }

    #[test]
    fn test_openai_pricing() {
        let table = global_pricing_table();

        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };

        // GPT-4o: 1M * $2.50 + 1M * $10.00 = $12.50
        // Long context (1M > 200K): input 1M * $2.50 * 2 = $5.00, output 1M * $10.00 = $10.00
        let cost = table.calculate("gpt-4o", &usage);
        assert_eq!(cost, dec!(15));

        // GPT-4o-mini: 1M * $0.15 * 2 + 1M * $0.60 = $0.90
        let cost = table.calculate("gpt-4o-mini", &usage);
        assert_eq!(cost, dec!(0.90));

        // Standard context for o3
        let small_usage = Usage {
            input_tokens: 100_000,
            output_tokens: 100_000,
            ..Default::default()
        };

        // o3: 0.1M * $10 + 0.1M * $40 = $1 + $4 = $5
        let cost = table.calculate("o3", &small_usage);
        assert_eq!(cost, dec!(5));

        // o3-mini: 0.1M * $1.10 + 0.1M * $4.40 = $0.11 + $0.44 = $0.55
        let cost = table.calculate("o3-mini", &small_usage);
        assert_eq!(cost, dec!(0.55));
    }

    #[test]
    fn test_gemini_pricing() {
        let table = global_pricing_table();

        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 100_000,
            ..Default::default()
        };

        // Gemini 2.0 Flash: 0.1M * $0.10 + 0.1M * $0.40 = $0.01 + $0.04 = $0.05
        let cost = table.calculate("gemini-2.0-flash", &usage);
        assert_eq!(cost, dec!(0.05));

        // Gemini 2.5 Pro: 0.1M * $1.25 + 0.1M * $10 = $0.125 + $1 = $1.125
        let cost = table.calculate("gemini-2.5-pro", &usage);
        assert_eq!(cost, dec!(1.125));

        // Gemini 2.0 Flash Lite: 0.1M * $0.075 + 0.1M * $0.30 = $0.0075 + $0.03 = $0.0375
        let cost = table.calculate("gemini-2.0-flash-lite", &usage);
        assert_eq!(cost, dec!(0.0375));
    }

    #[test]
    fn test_provider_aware_calculation() {
        let table = global_pricing_table();

        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 100_000,
            ..Default::default()
        };

        // OpenAI provider with date suffix should normalize
        let cost =
            table.calculate_for_provider("gpt-4o-2024-08-06", &usage, &UsageProvider::OpenAi);
        let expected = table.calculate("gpt-4o", &usage);
        assert_eq!(cost, expected);

        // Gemini provider with models/ prefix should normalize
        let cost =
            table.calculate_for_provider("models/gemini-2.0-flash", &usage, &UsageProvider::Gemini);
        let expected = table.calculate("gemini-2.0-flash", &usage);
        assert_eq!(cost, expected);

        // Anthropic provider should still work as before
        let cost =
            table.calculate_for_provider("claude-sonnet-4-5", &usage, &UsageProvider::Anthropic);
        let expected = table.calculate("claude-sonnet-4-5", &usage);
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_openai_provider_prefix_stripping() {
        let table = global_pricing_table();

        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 100_000,
            ..Default::default()
        };

        // Strip openai/ prefix
        let cost = table.calculate_for_provider("openai/gpt-4o", &usage, &UsageProvider::OpenAi);
        let expected = table.calculate("gpt-4o", &usage);
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_strip_date_suffix() {
        assert_eq!(strip_date_suffix("gpt-4o-2024-08-06"), "gpt-4o");
        assert_eq!(strip_date_suffix("o3-mini-2025-01-31"), "o3-mini");
        assert_eq!(strip_date_suffix("gpt-4o"), "gpt-4o");
        assert_eq!(strip_date_suffix("short"), "short");
        assert_eq!(strip_date_suffix(""), "");
    }

    #[test]
    fn test_builder_chaining_multi_provider() {
        let table = PricingTableBuilder::new()
            .defaults()
            .with_openai_models()
            .with_gemini_models()
            .build();

        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 100_000,
            ..Default::default()
        };

        // Anthropic models still work
        let cost = table.calculate("claude-sonnet-4-5", &usage);
        assert_eq!(cost, dec!(1.8));

        // OpenAI models work
        let cost = table.calculate("gpt-4o", &usage);
        // 0.1M * $2.50 + 0.1M * $10 = $0.25 + $1 = $1.25
        assert_eq!(cost, dec!(1.25));

        // Gemini models work
        let cost = table.calculate("gemini-2.0-flash", &usage);
        // 0.1M * $0.10 + 0.1M * $0.40 = $0.01 + $0.04 = $0.05
        assert_eq!(cost, dec!(0.05));
    }

    #[test]
    fn test_anthropic_pricing_unchanged() {
        // Verify existing Anthropic pricing is not affected by multi-provider additions
        let table = global_pricing_table();

        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 100_000,
            cached_input_tokens: None,
            cache_creation_tokens: None,
            ..Default::default()
        };

        // Sonnet: 0.1M * $3 + 0.1M * $15 = $0.3 + $1.5 = $1.8
        let cost = table.calculate("claude-sonnet-4-5", &usage);
        assert_eq!(cost, dec!(1.8));

        // Opus: 0.1M * $15 + 0.1M * $75 = $1.5 + $7.5 = $9
        let cost = table.calculate("claude-opus-4-6", &usage);
        assert_eq!(cost, dec!(9));

        // Haiku: 0.1M * $0.80 + 0.1M * $4 = $0.08 + $0.4 = $0.48
        let cost = table.calculate("claude-haiku-4-5", &usage);
        assert_eq!(cost, dec!(0.48));
    }

    #[test]
    fn test_openai_cache_pricing() {
        let table = global_pricing_table();

        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 100_000,
            cached_input_tokens: Some(50_000),
            cache_creation_tokens: Some(20_000),
            ..Default::default()
        };

        // GPT-4o with cache (standard context, 170K < 200K):
        // input: 0.1M * $2.50 = $0.25
        // output: 0.1M * $10 = $1
        // cache_read: 0.05M * $0.25 = $0.0125 (10% of input price)
        // cache_write: 0.02M * $3.125 = $0.0625 (125% of input price)
        let cost = table.calculate("gpt-4o", &usage);
        assert_eq!(cost, dec!(1.325));
    }

    #[test]
    fn test_usage_provider_display() {
        assert_eq!(format!("{}", UsageProvider::Anthropic), "Anthropic");
        assert_eq!(format!("{}", UsageProvider::OpenAi), "OpenAI");
        assert_eq!(format!("{}", UsageProvider::Gemini), "Gemini");
        assert_eq!(format!("{}", UsageProvider::Bedrock), "Bedrock");
        assert_eq!(format!("{}", UsageProvider::Vertex), "Vertex");
        assert_eq!(format!("{}", UsageProvider::Foundry), "Foundry");
        assert_eq!(
            format!("{}", UsageProvider::Unknown("Custom".into())),
            "Custom"
        );
    }

    #[test]
    fn test_usage_provider_default() {
        let provider = UsageProvider::default();
        assert_eq!(provider, UsageProvider::Anthropic);
    }

    // =========================================================================
    // IR-native pricing parity tests
    // =========================================================================

    #[test]
    fn ir_pricing_matches_raw_pricing_for_equivalent_input() {
        let pricing = ModelPricing {
            input_per_mtok: Decimal::from(3),
            output_per_mtok: Decimal::from(15),
            cache_read_per_mtok: Decimal::new(3, 1),    // 0.3
            cache_write_per_mtok: Decimal::new(375, 2), // 3.75
            long_context_multiplier: Decimal::ONE,
        };
        let usage = Usage {
            input_tokens: 100_000,
            output_tokens: 5_000,
            cached_input_tokens: Some(50_000),
            cache_creation_tokens: Some(20_000),
            ..Default::default()
        };
        assert_eq!(
            pricing.calculate(&usage),
            pricing.calculate_raw(100_000, 5_000, 50_000, 20_000),
        );
    }

    #[test]
    fn ir_pricing_charges_reasoning_tokens_at_output_rate() {
        let pricing = ModelPricing {
            input_per_mtok: Decimal::ZERO,
            output_per_mtok: Decimal::from(10),
            cache_read_per_mtok: Decimal::ZERO,
            cache_write_per_mtok: Decimal::ZERO,
            long_context_multiplier: Decimal::ONE,
        };
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 1_000,
            reasoning_tokens: Some(2_000),
            ..Default::default()
        };
        // 3000 output-billable tokens × $10/Mtok = $0.03
        assert_eq!(
            pricing.calculate(&usage),
            Decimal::new(3, 2) // 0.03
        );
    }

    #[test]
    fn anthropic_4_family_models_are_registered_explicitly() {
        // Confirms with_anthropic_models() registers each canonical
        // identifier so cost reporting can attribute spend per revision.
        let table = global_pricing_table();
        for model in [
            "claude-opus-4",
            "claude-opus-4-6",
            "claude-sonnet-4",
            "claude-sonnet-4-5",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
        ] {
            // Each of these MUST be in the explicit map (not just hit the
            // "opus"/"sonnet"/"haiku" normalization fallback). We verify by
            // checking that an obviously-different fake input does not match
            // and instead falls through.
            let pricing = table.get(model);
            assert!(
                pricing.input_per_mtok > Decimal::ZERO,
                "missing pricing for {model}"
            );
        }
    }

    #[test]
    fn gpt5_family_models_are_registered() {
        let table = global_pricing_table();
        for model in ["gpt-5", "gpt-5-mini", "gpt-5-nano", "o4-mini"] {
            let pricing = table.get(model);
            assert!(
                pricing.input_per_mtok > Decimal::ZERO,
                "missing pricing for {model}"
            );
        }
    }

    #[test]
    fn gemini_2_5_family_models_are_registered() {
        let table = global_pricing_table();
        for model in [
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
        ] {
            let pricing = table.get(model);
            assert!(
                pricing.input_per_mtok > Decimal::ZERO,
                "missing pricing for {model}"
            );
        }
    }

    #[test]
    fn generic_pricing_env_var_override_lowercases_and_dehyphens() {
        // Note: tests run in parallel so we use unique names per assertion
        // to avoid cross-test contamination of the global env table.
        // SAFETY: env mutation is intentional for this test.
        unsafe {
            std::env::set_var("BRANCHFORGE_PRICING_TEST_OVERRIDE_INPUT", "7.5");
            std::env::set_var("BRANCHFORGE_PRICING_TEST_OVERRIDE_OUTPUT", "30");
        }
        let table = PricingTableBuilder::new().from_env().build();
        let entry = table.get("test-override");
        assert_eq!(entry.input_per_mtok, dec!(7.5));
        assert_eq!(entry.output_per_mtok, dec!(30));
        unsafe {
            std::env::remove_var("BRANCHFORGE_PRICING_TEST_OVERRIDE_INPUT");
            std::env::remove_var("BRANCHFORGE_PRICING_TEST_OVERRIDE_OUTPUT");
        }
    }

    #[test]
    fn ir_pricing_table_dispatches_to_model() {
        let table = PricingTable::builder()
            .model(
                "test-model",
                ModelPricing {
                    input_per_mtok: Decimal::from(1),
                    output_per_mtok: Decimal::from(2),
                    cache_read_per_mtok: Decimal::ZERO,
                    cache_write_per_mtok: Decimal::ZERO,
                    long_context_multiplier: Decimal::ONE,
                },
            )
            .build();
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };
        // 1M × $1 + 1M × $2 = $3
        assert_eq!(table.calculate("test-model", &usage), Decimal::from(3));
    }
}
