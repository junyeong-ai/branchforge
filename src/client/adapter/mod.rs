//! Provider adapters for different cloud platforms.

mod anthropic;
#[cfg(any(feature = "aws", feature = "gcp", feature = "azure"))]
mod base;
mod config;
#[cfg(feature = "gemini")]
mod gemini;
#[cfg(feature = "openai")]
mod openai;
#[cfg(any(feature = "gcp", feature = "azure"))]
mod request;
mod traits;

#[cfg(any(feature = "aws", feature = "gcp", feature = "azure"))]
mod token_cache;

#[cfg(feature = "aws")]
pub(crate) mod bedrock;
#[cfg(feature = "aws")]
pub(crate) mod bedrock_stream;
#[cfg(feature = "azure")]
mod foundry;
#[cfg(feature = "gcp")]
mod vertex;

pub use anthropic::AnthropicAdapter;
pub use config::{
    BetaConfig, BetaFeature, DEFAULT_MODEL, DEFAULT_REASONING_MODEL, DEFAULT_SMALL_MODEL,
    FRONTIER_MODEL, ModelConfig, ModelType, ProviderConfig,
};
pub use traits::{ProviderAdapter, StreamFormat};

#[cfg(feature = "aws")]
pub use bedrock::BedrockAdapter;
#[cfg(feature = "azure")]
pub use foundry::FoundryAdapter;
#[cfg(feature = "gemini")]
pub use gemini::GeminiAdapter;
#[cfg(feature = "openai")]
pub use openai::OpenAiAdapter;
#[cfg(feature = "gcp")]
pub use vertex::VertexAdapter;

use crate::Result;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CloudProvider {
    #[default]
    Anthropic,
    #[cfg(feature = "aws")]
    Bedrock,
    #[cfg(feature = "gcp")]
    Vertex,
    #[cfg(feature = "azure")]
    Foundry,
    #[cfg(feature = "openai")]
    OpenAi,
    #[cfg(feature = "gemini")]
    Gemini,
}

impl CloudProvider {
    pub fn from_env() -> Self {
        // Explicit opt-in env vars take priority (consistent with existing providers).
        // API key env vars alone do NOT trigger auto-selection to avoid surprising
        // behavior when users have OPENAI_API_KEY set for other tooling.
        #[cfg(feature = "openai")]
        if std::env::var("CLAUDE_CODE_USE_OPENAI").is_ok() {
            return Self::OpenAi;
        }
        #[cfg(feature = "gemini")]
        if std::env::var("CLAUDE_CODE_USE_GEMINI").is_ok() {
            return Self::Gemini;
        }
        #[cfg(feature = "aws")]
        if std::env::var("CLAUDE_CODE_USE_BEDROCK").is_ok() {
            return Self::Bedrock;
        }
        #[cfg(feature = "gcp")]
        if std::env::var("CLAUDE_CODE_USE_VERTEX").is_ok() {
            return Self::Vertex;
        }
        #[cfg(feature = "azure")]
        if std::env::var("CLAUDE_CODE_USE_FOUNDRY").is_ok() {
            return Self::Foundry;
        }
        Self::Anthropic
    }

    pub fn default_models(&self) -> ModelConfig {
        match self {
            Self::Anthropic => ModelConfig::anthropic(),
            #[cfg(feature = "aws")]
            Self::Bedrock => ModelConfig::bedrock(),
            #[cfg(feature = "gcp")]
            Self::Vertex => ModelConfig::vertex(),
            #[cfg(feature = "azure")]
            Self::Foundry => ModelConfig::foundry(),
            #[cfg(feature = "openai")]
            Self::OpenAi => ModelConfig::openai(),
            #[cfg(feature = "gemini")]
            Self::Gemini => ModelConfig::gemini(),
        }
    }
}

pub async fn create_adapter(
    provider: CloudProvider,
    config: ProviderConfig,
) -> Result<Box<dyn ProviderAdapter>> {
    match provider {
        CloudProvider::Anthropic => Ok(Box::new(AnthropicAdapter::new(config))),
        #[cfg(feature = "aws")]
        CloudProvider::Bedrock => Ok(Box::new(BedrockAdapter::from_env(config).await?)),
        #[cfg(feature = "gcp")]
        CloudProvider::Vertex => Ok(Box::new(VertexAdapter::from_env(config).await?)),
        #[cfg(feature = "azure")]
        CloudProvider::Foundry => Ok(Box::new(FoundryAdapter::from_env(config).await?)),
        #[cfg(feature = "openai")]
        CloudProvider::OpenAi => Ok(Box::new(OpenAiAdapter::new(config))),
        #[cfg(feature = "gemini")]
        CloudProvider::Gemini => Ok(Box::new(GeminiAdapter::new(config))),
    }
}
