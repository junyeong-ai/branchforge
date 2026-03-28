//! Cloud provider environment configuration.

use std::collections::HashMap;
use std::env;

use crate::client::messages::{DEFAULT_MAX_TOKENS, MIN_THINKING_BUDGET};

#[derive(Clone, Debug, Default)]
pub struct CloudConfig {
    pub provider: ProviderSelection,
    pub tokens: TokenLimits,
    pub caching: PromptCachingConfig,
    pub gateway: GatewayConfig,
}

#[derive(Clone, Debug, Default)]
pub struct ProviderSelection {
    pub use_bedrock: bool,
    pub use_vertex: bool,
    pub use_foundry: bool,
}

#[derive(Clone, Debug)]
pub struct TokenLimits {
    pub max_output: u32,
    pub max_thinking: u32,
}

impl Default for TokenLimits {
    fn default() -> Self {
        Self {
            max_output: DEFAULT_MAX_TOKENS,
            max_thinking: MIN_THINKING_BUDGET,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PromptCachingConfig {
    pub disable_prompt_caching: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GatewayConfig {
    pub disable_experimental_betas: bool,
}

#[derive(Clone, Debug)]
pub struct BedrockConfig {
    pub region: Option<String>,
    pub small_model_region: Option<String>,
    pub bearer_token: Option<String>,
    pub auth_refresh_cmd: Option<String>,
    pub credential_export_cmd: Option<String>,
    /// Use global endpoint (global.anthropic.*) for maximum availability.
    /// Recommended for most use cases. Set to false for regional (CRIS) endpoints.
    pub use_global_endpoint: bool,
    /// Enable 1M context window beta feature (context-1m-2025-08-07).
    pub enable_1m_context: bool,
}

impl Default for BedrockConfig {
    fn default() -> Self {
        Self {
            region: None,
            small_model_region: None,
            bearer_token: None,
            auth_refresh_cmd: None,
            credential_export_cmd: None,
            use_global_endpoint: true, // Global endpoint is recommended
            enable_1m_context: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct VertexConfig {
    pub project_id: Option<String>,
    pub region: Option<String>,
    pub model_regions: HashMap<String, String>,
    pub enable_1m_context: bool,
}

#[derive(Clone, Debug, Default)]
pub struct FoundryConfig {
    pub resource: Option<String>,
    /// Alternative to resource: full base URL (e.g., `https://example-resource.services.ai.azure.com/anthropic/`)
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

impl CloudConfig {
    pub fn from_env() -> Self {
        Self {
            provider: ProviderSelection::from_env(),
            tokens: TokenLimits::from_env(),
            caching: PromptCachingConfig::from_env(),
            gateway: GatewayConfig::from_env(),
        }
    }

    pub fn active_provider(&self) -> Option<&'static str> {
        if self.provider.use_bedrock {
            Some("bedrock")
        } else if self.provider.use_vertex {
            Some("vertex")
        } else if self.provider.use_foundry {
            Some("foundry")
        } else {
            None
        }
    }
}

impl ProviderSelection {
    pub fn from_env() -> Self {
        Self {
            use_bedrock: is_flag_set("CLAUDE_CODE_USE_BEDROCK"),
            use_vertex: is_flag_set("CLAUDE_CODE_USE_VERTEX"),
            use_foundry: is_flag_set("CLAUDE_CODE_USE_FOUNDRY"),
        }
    }
}

impl TokenLimits {
    pub fn from_env() -> Self {
        Self {
            max_output: parse_env("CLAUDE_CODE_MAX_OUTPUT_TOKENS").unwrap_or(DEFAULT_MAX_TOKENS),
            max_thinking: parse_env("MAX_THINKING_TOKENS")
                .unwrap_or(MIN_THINKING_BUDGET)
                .max(MIN_THINKING_BUDGET),
        }
    }
}

impl PromptCachingConfig {
    pub fn from_env() -> Self {
        Self {
            disable_prompt_caching: is_flag_set("DISABLE_PROMPT_CACHING"),
        }
    }
}

impl GatewayConfig {
    pub fn from_env() -> Self {
        Self {
            disable_experimental_betas: is_flag_set("CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS"),
        }
    }
}

impl BedrockConfig {
    pub fn from_env() -> Self {
        Self {
            region: env::var("AWS_REGION").ok(),
            small_model_region: env::var("ANTHROPIC_SMALL_FAST_MODEL_AWS_REGION").ok(),
            bearer_token: env::var("AWS_BEARER_TOKEN_BEDROCK").ok(),
            auth_refresh_cmd: None, // Loaded from settings.json
            credential_export_cmd: None,
            use_global_endpoint: !is_flag_set("BEDROCK_USE_REGIONAL_ENDPOINT"),
            enable_1m_context: is_flag_set("BEDROCK_ENABLE_1M_CONTEXT"),
        }
    }

    /// Builder method to set use_global_endpoint.
    pub fn global_endpoint(mut self, enable: bool) -> Self {
        self.use_global_endpoint = enable;
        self
    }

    /// Builder method to enable extended context (1M tokens).
    pub fn extended_context(mut self, enable: bool) -> Self {
        self.enable_1m_context = enable;
        self
    }
}

impl VertexConfig {
    pub fn from_env() -> Self {
        let mut model_regions = HashMap::new();

        let region_vars = [
            ("VERTEX_REGION_CLAUDE_4_5_SONNET", "claude-4-5-sonnet"),
            ("VERTEX_REGION_CLAUDE_4_5_HAIKU", "claude-4-5-haiku"),
            ("VERTEX_REGION_CLAUDE_4_6_OPUS", "claude-4-6-opus"),
        ];

        for (env_var, model_key) in region_vars {
            if let Ok(region) = env::var(env_var) {
                model_regions.insert(model_key.to_string(), region);
            }
        }

        Self {
            project_id: env::var("ANTHROPIC_VERTEX_PROJECT_ID")
                .or_else(|_| env::var("GOOGLE_CLOUD_PROJECT"))
                .or_else(|_| env::var("GCLOUD_PROJECT"))
                .ok(),
            region: env::var("CLOUD_ML_REGION")
                .or_else(|_| env::var("GOOGLE_CLOUD_REGION"))
                .ok(),
            model_regions,
            enable_1m_context: is_flag_set("VERTEX_ENABLE_1M_CONTEXT"),
        }
    }

    pub fn region_for_model(&self, model: &str) -> Option<&str> {
        for (key, region) in &self.model_regions {
            if model.contains(key) {
                return Some(region);
            }
        }
        self.region.as_deref()
    }

    pub fn is_global(&self) -> bool {
        self.region.as_deref() == Some("global")
    }
}

impl FoundryConfig {
    pub fn from_env() -> Self {
        Self {
            resource: env::var("ANTHROPIC_FOUNDRY_RESOURCE")
                .or_else(|_| env::var("AZURE_RESOURCE_NAME"))
                .ok(),
            base_url: env::var("ANTHROPIC_FOUNDRY_BASE_URL").ok(),
            api_key: env::var("ANTHROPIC_FOUNDRY_API_KEY")
                .or_else(|_| env::var("AZURE_API_KEY"))
                .ok(),
        }
    }

    /// Builder method to set resource name.
    pub fn resource(mut self, resource: impl Into<String>) -> Self {
        self.resource = Some(resource.into());
        self
    }

    /// Builder method to set base URL (alternative to resource).
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Builder method to set API key.
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }
}

fn is_flag_set(var: &str) -> bool {
    env::var(var)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn parse_env<T: std::str::FromStr>(var: &str) -> Option<T> {
    env::var(var).ok().and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloud_config_default() {
        let config = CloudConfig::default();
        assert!(!config.provider.use_bedrock);
        assert!(!config.provider.use_vertex);
        assert!(!config.provider.use_foundry);
        assert_eq!(config.tokens.max_output, DEFAULT_MAX_TOKENS);
        assert_eq!(config.tokens.max_thinking, MIN_THINKING_BUDGET);
    }

    #[test]
    fn test_token_limits_default() {
        let limits = TokenLimits::default();
        assert_eq!(limits.max_output, DEFAULT_MAX_TOKENS);
        assert_eq!(limits.max_thinking, MIN_THINKING_BUDGET);
    }

    #[test]
    fn test_vertex_region_for_model() {
        let mut config = VertexConfig::default();
        config
            .model_regions
            .insert("claude-4-5-sonnet".into(), "us-east5".into());

        assert_eq!(
            config.region_for_model("claude-4-5-sonnet@20250929"),
            Some("us-east5")
        );
    }
}
