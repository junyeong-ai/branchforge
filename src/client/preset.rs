//! Preset compositions of `(codec × transport)`.
//!
//! A [`Preset`] is a named, opinionated combination of one
//! [`crate::client::codec::ModelCodec`] and one
//! [`crate::client::transport::ModelTransport`]. Presets are the
//! user-facing primitive: applications pick a preset, the preset returns
//! a fully-wired [`crate::client::provider_client::ProviderClient`].
//!
//! See plan §1 (preset table) for the canonical preset list.
//!
//! Naming convention: kebab-case `{transport}-{publisher}` for cloud
//! presets (`vertex-anthropic`, `bedrock-anthropic`, `foundry-anthropic`),
//! bare provider name for direct presets (`anthropic`, `openai`, `gemini`).
//! `vertex-gemini` is the documented user-recognizable exception (the URL
//! publisher is `google` but the model family is universally called
//! Gemini).

use std::sync::Arc;

use secrecy::SecretString;

#[cfg(feature = "aws")]
use crate::client::codec::BedrockConverseCodec;
use crate::client::codec::{
    AnthropicMessagesCodec, GeminiGenerateCodec, ModelCodec, OpenAiChatCodec, OpenAiResponsesCodec,
};
use crate::client::provider_client::ProviderClient;
use crate::client::transport::{DirectAuth, DirectTransport, ModelTransport};
use crate::{Error, Result};

/// One of the known preset compositions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// Anthropic Direct via `api.anthropic.com`. Requires
    /// `ANTHROPIC_API_KEY`.
    Anthropic,
    /// **Default OpenAI preset** — `/v1/responses` (the modern endpoint).
    /// Requires `OPENAI_API_KEY`.
    OpenAi,
    /// Legacy OpenAI preset — `/v1/chat/completions`. Required for
    /// OpenAI-compatible third-party servers (Grok, DeepSeek, Mistral,
    /// OpenRouter, Together, …) by setting `OPENAI_BASE_URL`.
    OpenAiChat,
    /// Gemini Direct via `generativelanguage.googleapis.com`. Requires
    /// `GEMINI_API_KEY` (or `GOOGLE_API_KEY`).
    Gemini,
    /// **Headline acceptance preset.** Gemini codec routed through
    /// Vertex transport with `publishers/google`. Requires GCP ADC plus
    /// `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`.
    #[cfg(feature = "gcp")]
    VertexGemini,
    /// Anthropic Messages routed through Vertex transport with
    /// `publishers/anthropic`. Requires GCP ADC plus
    /// `GOOGLE_CLOUD_PROJECT` / `GOOGLE_CLOUD_LOCATION`.
    #[cfg(feature = "gcp")]
    VertexAnthropic,
    /// Bedrock Converse via AWS SigV4 (or `AWS_BEARER_TOKEN_BEDROCK`).
    /// Carries Anthropic, Amazon Nova, Meta Llama, Mistral, Cohere, …
    /// — all routed through the same `BedrockConverseCodec`.
    #[cfg(feature = "aws")]
    Bedrock,
    /// Azure AI Foundry Anthropic endpoint via Entra ID or api-key.
    /// Requires `AZURE_AI_BASE_URL` (or `AZURE_AI_RESOURCE`).
    #[cfg(feature = "azure")]
    FoundryAnthropic,
}

impl Preset {
    /// Stable string id for this preset, used by env-var routing.
    pub fn id(self) -> &'static str {
        match self {
            Preset::Anthropic => "anthropic",
            Preset::OpenAi => "openai",
            Preset::OpenAiChat => "openai-chat",
            Preset::Gemini => "gemini",
            #[cfg(feature = "gcp")]
            Preset::VertexGemini => "vertex-gemini",
            #[cfg(feature = "gcp")]
            Preset::VertexAnthropic => "vertex-anthropic",
            #[cfg(feature = "aws")]
            Preset::Bedrock => "bedrock",
            #[cfg(feature = "azure")]
            Preset::FoundryAnthropic => "foundry-anthropic",
        }
    }

    /// Resolve a preset id from a string.
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Preset::Anthropic),
            "openai" => Some(Preset::OpenAi),
            "openai-chat" => Some(Preset::OpenAiChat),
            "gemini" => Some(Preset::Gemini),
            #[cfg(feature = "gcp")]
            "vertex-gemini" => Some(Preset::VertexGemini),
            #[cfg(feature = "gcp")]
            "vertex-anthropic" => Some(Preset::VertexAnthropic),
            #[cfg(feature = "aws")]
            "bedrock" => Some(Preset::Bedrock),
            #[cfg(feature = "azure")]
            "foundry-anthropic" => Some(Preset::FoundryAnthropic),
            _ => None,
        }
    }

    /// Build a fully-wired [`ProviderClient`] for this preset using
    /// standard vendor environment variables.
    ///
    /// - `Anthropic`: `ANTHROPIC_API_KEY`, optional `ANTHROPIC_BASE_URL`.
    /// - `Gemini`: `GEMINI_API_KEY` or `GOOGLE_API_KEY`, optional `GEMINI_BASE_URL`.
    /// - `VertexGemini` / `VertexAnthropic`: `GOOGLE_CLOUD_PROJECT`,
    ///   `GOOGLE_CLOUD_LOCATION` (or `GOOGLE_CLOUD_REGION` / `CLOUD_ML_REGION`),
    ///   `GOOGLE_CLOUD_QUOTA_PROJECT` (defaults to project), GCP ADC.
    pub async fn build_from_env(self) -> Result<ProviderClient> {
        let (codec, transport) = self.build_components_from_env().await?;
        ProviderClient::new(codec, transport)
    }

    async fn build_components_from_env(
        self,
    ) -> Result<(Arc<dyn ModelCodec>, Arc<dyn ModelTransport>)> {
        match self {
            Preset::Anthropic => {
                let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
                    Error::Config("ANTHROPIC_API_KEY not set for `anthropic` preset".into())
                })?;
                let base = std::env::var("ANTHROPIC_BASE_URL")
                    .unwrap_or_else(|_| "https://api.anthropic.com".into());
                let codec = Arc::new(AnthropicMessagesCodec::new()) as Arc<dyn ModelCodec>;
                let transport = Arc::new(
                    DirectTransport::new(base, DirectAuth::XApiKey(SecretString::from(key)))
                        .with_allowed_codecs(&["anthropic-messages"]),
                ) as Arc<dyn ModelTransport>;
                Ok((codec, transport))
            }
            Preset::OpenAi => {
                let key = std::env::var("OPENAI_API_KEY").map_err(|_| {
                    Error::Config("OPENAI_API_KEY not set for `openai` preset".into())
                })?;
                let base = std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com".into());
                let codec = Arc::new(OpenAiResponsesCodec::new()) as Arc<dyn ModelCodec>;
                let transport = Arc::new(
                    DirectTransport::new(base, DirectAuth::Bearer(SecretString::from(key)))
                        .with_allowed_codecs(&["openai-responses"]),
                ) as Arc<dyn ModelTransport>;
                Ok((codec, transport))
            }
            Preset::OpenAiChat => {
                let key = std::env::var("OPENAI_API_KEY").map_err(|_| {
                    Error::Config("OPENAI_API_KEY not set for `openai-chat` preset".into())
                })?;
                let base = std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com".into());
                let codec = Arc::new(OpenAiChatCodec::new()) as Arc<dyn ModelCodec>;
                let transport = Arc::new(
                    DirectTransport::new(base, DirectAuth::Bearer(SecretString::from(key)))
                        .with_allowed_codecs(&["openai-chat"]),
                ) as Arc<dyn ModelTransport>;
                Ok((codec, transport))
            }
            Preset::Gemini => {
                let key = std::env::var("GEMINI_API_KEY")
                    .or_else(|_| std::env::var("GOOGLE_API_KEY"))
                    .map_err(|_| {
                        Error::Config(
                            "GEMINI_API_KEY (or GOOGLE_API_KEY) not set for `gemini` preset".into(),
                        )
                    })?;
                let base = std::env::var("GEMINI_BASE_URL")
                    .unwrap_or_else(|_| "https://generativelanguage.googleapis.com".into());
                let codec = Arc::new(GeminiGenerateCodec::new()) as Arc<dyn ModelCodec>;
                let transport = Arc::new(
                    DirectTransport::new(
                        base,
                        DirectAuth::QueryParam {
                            param: "key",
                            value: SecretString::from(key),
                        },
                    )
                    .with_allowed_codecs(&["gemini-generate"]),
                ) as Arc<dyn ModelTransport>;
                Ok((codec, transport))
            }
            #[cfg(feature = "gcp")]
            Preset::VertexGemini => {
                use crate::client::transport::VertexTransport;
                let codec = Arc::new(GeminiGenerateCodec::new()) as Arc<dyn ModelCodec>;
                let transport =
                    Arc::new(VertexTransport::from_env().await?) as Arc<dyn ModelTransport>;
                Ok((codec, transport))
            }
            #[cfg(feature = "gcp")]
            Preset::VertexAnthropic => {
                use crate::client::transport::VertexTransport;
                let codec = Arc::new(AnthropicMessagesCodec::new()) as Arc<dyn ModelCodec>;
                let transport =
                    Arc::new(VertexTransport::from_env().await?) as Arc<dyn ModelTransport>;
                Ok((codec, transport))
            }
            #[cfg(feature = "aws")]
            Preset::Bedrock => {
                use crate::client::transport::BedrockTransport;
                let codec = Arc::new(BedrockConverseCodec::new()) as Arc<dyn ModelCodec>;
                let transport =
                    Arc::new(BedrockTransport::from_env().await?) as Arc<dyn ModelTransport>;
                Ok((codec, transport))
            }
            #[cfg(feature = "azure")]
            Preset::FoundryAnthropic => {
                use crate::client::transport::FoundryTransport;
                let codec = Arc::new(AnthropicMessagesCodec::new()) as Arc<dyn ModelCodec>;
                let transport = Arc::new(FoundryTransport::from_env()?) as Arc<dyn ModelTransport>;
                Ok((codec, transport))
            }
        }
    }
}

/// Resolve a preset from `BRANCHFORGE_PROVIDER` and build it from env.
///
/// This is the one-call entry point for examples and the (forthcoming)
/// `Client::from_env` builder.
pub async fn from_env() -> Result<ProviderClient> {
    let id = std::env::var("BRANCHFORGE_PROVIDER").map_err(|_| {
        Error::Config(
            "BRANCHFORGE_PROVIDER not set; choose one of: \
             anthropic, openai, openai-chat, gemini, vertex-anthropic, vertex-gemini, bedrock, foundry-anthropic"
                .into(),
        )
    })?;
    let preset = Preset::from_id(&id).ok_or_else(|| {
        Error::Config(format!("BRANCHFORGE_PROVIDER='{id}' is not a known preset"))
    })?;
    preset.build_from_env().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for p in [Preset::Anthropic, Preset::Gemini] {
            assert_eq!(Preset::from_id(p.id()), Some(p));
        }
        #[cfg(feature = "gcp")]
        for p in [Preset::VertexGemini, Preset::VertexAnthropic] {
            assert_eq!(Preset::from_id(p.id()), Some(p));
        }
    }

    #[test]
    fn unknown_id_returns_none() {
        assert_eq!(Preset::from_id("does-not-exist"), None);
    }
}
