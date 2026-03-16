//! Provider and model configuration.

use std::collections::{HashMap, HashSet};
use std::env;

use crate::client::messages::{DEFAULT_MAX_TOKENS, MIN_THINKING_BUDGET};

// Anthropic API models
pub const DEFAULT_MODEL: &str = "claude-sonnet-4-5-20250929";
pub const DEFAULT_SMALL_MODEL: &str = "claude-haiku-4-5-20251001";
pub const DEFAULT_REASONING_MODEL: &str = "claude-opus-4-6";
pub const FRONTIER_MODEL: &str = DEFAULT_REASONING_MODEL;

// AWS Bedrock models (using global endpoint prefix for maximum availability)
#[cfg(feature = "aws")]
pub const BEDROCK_MODEL: &str = "global.anthropic.claude-sonnet-4-5-20250929-v1:0";
#[cfg(feature = "aws")]
pub const BEDROCK_SMALL_MODEL: &str = "global.anthropic.claude-haiku-4-5-20251001-v1:0";
#[cfg(feature = "aws")]
pub const BEDROCK_REASONING_MODEL: &str = "global.anthropic.claude-opus-4-6-v1:0";

// GCP Vertex AI models
#[cfg(feature = "gcp")]
pub const VERTEX_MODEL: &str = "claude-sonnet-4-5@20250929";
#[cfg(feature = "gcp")]
pub const VERTEX_SMALL_MODEL: &str = "claude-haiku-4-5@20251001";
#[cfg(feature = "gcp")]
pub const VERTEX_REASONING_MODEL: &str = "claude-opus-4-6";

// Azure Foundry models
#[cfg(feature = "azure")]
pub const FOUNDRY_MODEL: &str = "claude-sonnet-4-5";
#[cfg(feature = "azure")]
pub const FOUNDRY_SMALL_MODEL: &str = "claude-haiku-4-5";
#[cfg(feature = "azure")]
pub const FOUNDRY_REASONING_MODEL: &str = "claude-opus-4-6";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    #[default]
    Primary,
    Small,
    Reasoning,
}

#[derive(Clone, Debug)]
pub struct ModelConfig {
    pub primary: String,
    pub small: String,
    pub reasoning: Option<String>,
}

impl ModelConfig {
    pub fn new(primary: impl Into<String>, small: impl Into<String>) -> Self {
        Self {
            primary: primary.into(),
            small: small.into(),
            reasoning: None,
        }
    }

    pub fn anthropic() -> Self {
        Self::from_env_with_defaults(DEFAULT_MODEL, DEFAULT_SMALL_MODEL, DEFAULT_REASONING_MODEL)
    }

    fn from_env_with_defaults(
        default_primary: &str,
        default_small: &str,
        default_reasoning: &str,
    ) -> Self {
        Self {
            primary: env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| default_primary.into()),
            small: env::var("ANTHROPIC_SMALL_FAST_MODEL").unwrap_or_else(|_| default_small.into()),
            reasoning: Some(
                env::var("ANTHROPIC_REASONING_MODEL").unwrap_or_else(|_| default_reasoning.into()),
            ),
        }
    }

    #[cfg(feature = "aws")]
    pub fn bedrock() -> Self {
        Self::from_env_with_defaults(BEDROCK_MODEL, BEDROCK_SMALL_MODEL, BEDROCK_REASONING_MODEL)
    }

    #[cfg(feature = "gcp")]
    pub fn vertex() -> Self {
        Self::from_env_with_defaults(VERTEX_MODEL, VERTEX_SMALL_MODEL, VERTEX_REASONING_MODEL)
    }

    #[cfg(feature = "azure")]
    pub fn foundry() -> Self {
        Self::from_env_with_defaults(FOUNDRY_MODEL, FOUNDRY_SMALL_MODEL, FOUNDRY_REASONING_MODEL)
    }

    #[cfg(feature = "openai")]
    pub fn openai() -> Self {
        Self {
            primary: env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".into()),
            small: env::var("OPENAI_SMALL_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
            reasoning: Some(env::var("OPENAI_REASONING_MODEL").unwrap_or_else(|_| "o3".into())),
        }
    }

    #[cfg(feature = "gemini")]
    pub fn gemini() -> Self {
        Self {
            primary: env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.0-flash".into()),
            small: env::var("GEMINI_SMALL_MODEL")
                .unwrap_or_else(|_| "gemini-2.0-flash-lite".into()),
            reasoning: Some(
                env::var("GEMINI_REASONING_MODEL").unwrap_or_else(|_| "gemini-2.5-pro".into()),
            ),
        }
    }

    pub fn primary(mut self, model: impl Into<String>) -> Self {
        self.primary = model.into();
        self
    }

    pub fn small(mut self, model: impl Into<String>) -> Self {
        self.small = model.into();
        self
    }

    pub fn reasoning(mut self, model: impl Into<String>) -> Self {
        self.reasoning = Some(model.into());
        self
    }

    pub fn get(&self, model_type: ModelType) -> &str {
        match model_type {
            ModelType::Primary => &self.primary,
            ModelType::Small => &self.small,
            ModelType::Reasoning => self.reasoning.as_deref().unwrap_or(&self.primary),
        }
    }

    pub fn resolve_alias<'a>(&'a self, alias: &'a str) -> &'a str {
        match alias {
            "sonnet" => &self.primary,
            "haiku" => &self.small,
            "opus" => self.reasoning.as_deref().unwrap_or(&self.primary),
            other => other,
        }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self::anthropic()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BetaFeature {
    InterleavedThinking,
    ContextManagement,
    StructuredOutputs,
    PromptCaching,
    MaxTokens128k,
    CodeExecution,
    Mcp,
    WebSearch,
    WebFetch,
    OAuth,
    FilesApi,
    Effort,
    /// 1M token context window (for Sonnet 4.5 on Bedrock/Vertex).
    Context1M,
    /// Tool search for progressive disclosure of MCP tools.
    AdvancedToolUse,
}

impl BetaFeature {
    const FEATURES: &'static [(BetaFeature, &'static str)] = &[
        (Self::InterleavedThinking, "interleaved-thinking-2025-05-14"),
        (Self::ContextManagement, "context-management-2025-06-27"),
        (Self::StructuredOutputs, "structured-outputs-2025-11-13"),
        (Self::PromptCaching, "prompt-caching-2024-07-31"),
        (Self::MaxTokens128k, "max-tokens-3-5-sonnet-2024-07-15"),
        (Self::CodeExecution, "code-execution-2025-01-24"),
        (Self::Mcp, "mcp-2025-04-08"),
        (Self::WebSearch, "web-search-2025-03-05"),
        (Self::WebFetch, "web-fetch-2025-09-10"),
        (Self::OAuth, "oauth-2025-04-20"),
        (Self::FilesApi, "files-api-2025-04-14"),
        (Self::Effort, "effort-2025-11-24"),
        (Self::Context1M, "context-1m-2025-08-07"),
        (Self::AdvancedToolUse, "advanced-tool-use-2025-11-20"),
    ];

    pub fn header_value(&self) -> &'static str {
        Self::FEATURES
            .iter()
            .find(|(f, _)| f == self)
            .map(|(_, v)| *v)
            .expect("all variants covered in FEATURES")
    }

    fn from_header(value: &str) -> Option<Self> {
        Self::FEATURES
            .iter()
            .find(|(_, v)| *v == value)
            .map(|(f, _)| *f)
    }

    pub fn all() -> impl Iterator<Item = BetaFeature> {
        Self::FEATURES.iter().map(|(f, _)| *f)
    }
}

#[derive(Clone, Debug, Default)]
pub struct BetaConfig {
    features: HashSet<BetaFeature>,
    custom: Vec<String>,
}

impl BetaConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn all() -> Self {
        Self {
            features: BetaFeature::all().collect(),
            custom: Vec::new(),
        }
    }

    pub fn feature(mut self, feature: BetaFeature) -> Self {
        self.features.insert(feature);
        self
    }

    pub fn custom(mut self, flag: impl Into<String>) -> Self {
        self.custom.push(flag.into());
        self
    }

    pub fn add(&mut self, feature: BetaFeature) {
        self.features.insert(feature);
    }

    pub fn add_custom(&mut self, flag: impl Into<String>) {
        self.custom.push(flag.into());
    }

    pub fn from_env() -> Self {
        let mut config = Self::new();

        if let Ok(flags) = env::var("ANTHROPIC_BETA_FLAGS") {
            for flag in flags.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if let Some(feature) = BetaFeature::from_header(flag) {
                    config.features.insert(feature);
                } else {
                    config.custom.push(flag.to_string());
                }
            }
        }

        config
    }

    pub fn header_value(&self) -> Option<String> {
        let mut flags: Vec<&str> = self.features.iter().map(|f| f.header_value()).collect();
        flags.sort();

        for custom in &self.custom {
            if !flags.contains(&custom.as_str()) {
                flags.push(custom);
            }
        }

        if flags.is_empty() {
            None
        } else {
            Some(flags.join(","))
        }
    }

    pub fn is_empty(&self) -> bool {
        self.features.is_empty() && self.custom.is_empty()
    }

    pub fn has(&self, feature: BetaFeature) -> bool {
        self.features.contains(&feature)
    }
}

#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub models: ModelConfig,
    pub max_tokens: u32,
    pub thinking_budget: Option<u32>,
    pub enable_caching: bool,
    pub api_version: String,
    pub beta: BetaConfig,
    pub extra_headers: HashMap<String, String>,
}

impl ProviderConfig {
    pub fn new(models: ModelConfig) -> Self {
        Self {
            models,
            max_tokens: DEFAULT_MAX_TOKENS,
            thinking_budget: None,
            enable_caching: !env::var("DISABLE_PROMPT_CACHING")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            api_version: "2023-06-01".into(),
            beta: BetaConfig::from_env(),
            extra_headers: HashMap::new(),
        }
    }

    pub fn max_tokens(mut self, tokens: u32) -> Self {
        self.max_tokens = tokens;
        if tokens > DEFAULT_MAX_TOKENS {
            self.beta.add(BetaFeature::MaxTokens128k);
        }
        self
    }

    pub fn thinking(mut self, budget: u32) -> Self {
        self.thinking_budget = Some(budget.max(MIN_THINKING_BUDGET));
        self.beta.add(BetaFeature::InterleavedThinking);
        self
    }

    pub fn disable_caching(mut self) -> Self {
        self.enable_caching = false;
        self
    }

    pub fn api_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = version.into();
        self
    }

    pub fn beta(mut self, feature: BetaFeature) -> Self {
        self.beta.add(feature);
        self
    }

    pub fn beta_config(mut self, config: BetaConfig) -> Self {
        self.beta = config;
        self
    }

    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_headers.insert(key.into(), value.into());
        self
    }

    pub fn requires_128k_beta(&self) -> bool {
        self.max_tokens > DEFAULT_MAX_TOKENS
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self::new(ModelConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_config_get() {
        let config = ModelConfig::anthropic();
        assert!(config.get(ModelType::Primary).contains("sonnet"));
        assert!(config.get(ModelType::Small).contains("haiku"));
        assert!(config.get(ModelType::Reasoning).contains("opus"));
    }

    #[test]
    fn test_provider_config_default_max_tokens() {
        let config = ProviderConfig::default();
        assert_eq!(config.max_tokens, DEFAULT_MAX_TOKENS);
        assert!(!config.requires_128k_beta());
    }

    #[test]
    fn test_provider_config_builder() {
        let config = ProviderConfig::new(ModelConfig::anthropic())
            .max_tokens(16384)
            .thinking(10000)
            .disable_caching();

        assert_eq!(config.max_tokens, 16384);
        assert_eq!(config.thinking_budget, Some(10000));
        assert!(!config.enable_caching);
        assert!(config.requires_128k_beta());
        assert!(config.beta.has(BetaFeature::MaxTokens128k));
        assert!(config.beta.has(BetaFeature::InterleavedThinking));
    }

    #[test]
    fn test_provider_config_auto_128k_beta() {
        let config = ProviderConfig::default().max_tokens(DEFAULT_MAX_TOKENS);
        assert!(!config.beta.has(BetaFeature::MaxTokens128k));

        let config = ProviderConfig::default().max_tokens(DEFAULT_MAX_TOKENS + 1);
        assert!(config.beta.has(BetaFeature::MaxTokens128k));
    }

    #[test]
    fn test_provider_config_thinking_auto_beta() {
        let config = ProviderConfig::default().thinking(5000);
        assert!(config.beta.has(BetaFeature::InterleavedThinking));
        assert_eq!(config.thinking_budget, Some(5000));
    }

    #[test]
    fn test_provider_config_thinking_min_budget() {
        let config = ProviderConfig::default().thinking(500);
        assert_eq!(config.thinking_budget, Some(MIN_THINKING_BUDGET));
    }

    #[test]
    fn test_beta_feature_header() {
        assert_eq!(
            BetaFeature::InterleavedThinking.header_value(),
            "interleaved-thinking-2025-05-14"
        );
        assert_eq!(
            BetaFeature::MaxTokens128k.header_value(),
            "max-tokens-3-5-sonnet-2024-07-15"
        );
    }

    #[test]
    fn test_beta_config_with_features() {
        let config = BetaConfig::new()
            .feature(BetaFeature::InterleavedThinking)
            .feature(BetaFeature::ContextManagement);

        assert!(config.has(BetaFeature::InterleavedThinking));
        assert!(config.has(BetaFeature::ContextManagement));
        assert!(!config.has(BetaFeature::MaxTokens128k));

        let header = config.header_value().unwrap();
        assert!(header.contains("interleaved-thinking"));
        assert!(header.contains("context-management"));
    }

    #[test]
    fn test_beta_config_custom() {
        let config = BetaConfig::new()
            .feature(BetaFeature::InterleavedThinking)
            .custom("new-feature-2026-01-01");

        let header = config.header_value().unwrap();
        assert!(header.contains("interleaved-thinking"));
        assert!(header.contains("new-feature-2026-01-01"));
    }

    #[test]
    fn test_beta_config_all() {
        let config = BetaConfig::all();
        assert!(config.has(BetaFeature::InterleavedThinking));
        assert!(config.has(BetaFeature::ContextManagement));
        assert!(config.has(BetaFeature::MaxTokens128k));
    }

    #[test]
    fn test_provider_config_beta() {
        let config = ProviderConfig::default()
            .beta(BetaFeature::InterleavedThinking)
            .beta_config(
                BetaConfig::new()
                    .feature(BetaFeature::InterleavedThinking)
                    .custom("experimental-feature"),
            );

        assert!(config.beta.has(BetaFeature::InterleavedThinking));
        let header = config.beta.header_value().unwrap();
        assert!(header.contains("experimental-feature"));
    }

    #[test]
    fn test_beta_config_empty() {
        let config = BetaConfig::new();
        assert!(config.is_empty());
        assert!(config.header_value().is_none());
    }

    #[test]
    fn test_provider_config_extra_headers() {
        let config = ProviderConfig::default()
            .header("x-custom", "value")
            .header("x-another", "test");

        assert_eq!(config.extra_headers.get("x-custom"), Some(&"value".into()));
        assert_eq!(config.extra_headers.get("x-another"), Some(&"test".into()));
    }
}
