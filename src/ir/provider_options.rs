//! Typed per-provider extension options.
//!
//! [`ProviderOptions`] is the typed escape hatch for provider-specific knobs
//! that are not portable enough to live on
//! [`ModelSettings`](super::settings::ModelSettings). Each variant is a
//! strongly-typed struct so users get autocomplete and type checking; codecs
//! read only their own field and emit
//! [`ModelWarning::DroppedProviderOption`](super::warning::ModelWarning::DroppedProviderOption)
//! for any sibling field that was set.
//!
//! These types are **always compiled in** regardless of cargo features —
//! they are cheap to define and the DX of "turn on a feature and a struct
//! field appears" would be confusing.

use serde::{Deserialize, Serialize};

pub use super::settings::ReasoningEffort;

/// Per-provider typed extension options. Each codec reads only its own
/// field. Sibling fields are dropped with a warning.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<AnthropicOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai: Option<OpenAiOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini: Option<GeminiOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bedrock: Option<BedrockOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertex: Option<VertexOptions>,
}

impl ProviderOptions {
    /// `true` when no provider-specific options are set at all. Codecs use
    /// this as a fast path.
    pub fn is_empty(&self) -> bool {
        self.anthropic.is_none()
            && self.openai.is_none()
            && self.gemini.is_none()
            && self.bedrock.is_none()
            && self.vertex.is_none()
    }
}

// =============================================================================
// Anthropic
// =============================================================================

/// Anthropic-specific request knobs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicOptions {
    /// Strategy for applying `cache_control` markers to system blocks and
    /// the conversation tail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
    /// Beta features to enable, sent via the `anthropic-beta` header.
    /// Examples: `"context-1m-2025-08-07"`, `"prompt-caching-2024-07-31"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub beta_features: Vec<String>,
    /// Service tier preference (`auto`, `standard_only`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

/// How prompt caching markers are applied across the request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControl {
    pub mode: CacheControlMode,
    /// TTL hint (`"5m"`, `"1h"`). Provider may ignore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheControlMode {
    /// Mark the system prompt as cacheable.
    System,
    /// Mark each tool definition as cacheable.
    Tools,
    /// Mark the system prompt and the most recent user turn boundary.
    SystemAndConversation,
}

// =============================================================================
// OpenAI
// =============================================================================

/// OpenAI-specific request knobs (Chat Completions and Responses).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenAiOptions {
    /// `parallel_tool_calls` toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// `logit_bias` map (token id → bias in [-100, 100]).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub logit_bias: std::collections::BTreeMap<String, i32>,
    /// `store: true` to retain the response on OpenAI's side (Responses API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<bool>,
    /// Free-form metadata attached to the response (Responses API).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub metadata: std::collections::BTreeMap<String, String>,
    /// Reasoning effort override (Responses API / o-series). Takes
    /// precedence over [`ModelSettings::reasoning`](super::settings::ModelSettings::reasoning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Service tier (`auto`, `default`, `flex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// Truncation strategy for Responses API context overflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<String>,
}

// =============================================================================
// Gemini
// =============================================================================

/// Gemini-specific request knobs (both `generativelanguage.googleapis.com`
/// and `aiplatform.googleapis.com` flavours).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GeminiOptions {
    /// Per-category safety threshold settings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safety_settings: Vec<SafetySetting>,
    /// `generationConfig.responseMimeType` for structured output
    /// (`"application/json"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_mime_type: Option<String>,
    /// `generationConfig.thinkingConfig.thinkingBudget`. Takes precedence
    /// over [`ModelSettings::reasoning`](super::settings::ModelSettings::reasoning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u64>,
    /// Cached content resource name (`cachedContents/...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_content: Option<String>,
}

/// One Gemini safety category threshold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetySetting {
    /// Category, e.g. `"HARM_CATEGORY_HARASSMENT"`.
    pub category: String,
    /// Threshold, e.g. `"BLOCK_MEDIUM_AND_ABOVE"`.
    pub threshold: String,
}

// =============================================================================
// Bedrock
// =============================================================================

/// Bedrock-Converse-specific request knobs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BedrockOptions {
    /// Free-form `additionalModelRequestFields` payload merged into the
    /// Converse request body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields: Option<serde_json::Value>,
    /// Guardrail identifier and version applied to the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardrail: Option<BedrockGuardrail>,
    /// Performance configuration latency hint (`standard`, `optimized`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BedrockGuardrail {
    pub identifier: String,
    pub version: String,
    /// `enabled`, `disabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
}

// =============================================================================
// Vertex
// =============================================================================

/// GCP Vertex transport knobs that apply to any codec routed through Vertex.
/// Note these are *transport-level* hints, not codec-level — they live here
/// alongside the other provider option groups for DX uniformity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VertexOptions {
    /// Override the auto-injected `x-goog-user-project` quota project header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_project: Option<String>,
    /// Force the `global` location instead of the configured region.
    #[serde(default)]
    pub global_endpoint: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_provider_options_serialize_compact() {
        let p = ProviderOptions::default();
        assert!(p.is_empty());
        assert_eq!(serde_json::to_string(&p).unwrap(), "{}");
    }

    #[test]
    fn anthropic_options_round_trip() {
        let opts = AnthropicOptions {
            cache_control: Some(CacheControl {
                mode: CacheControlMode::SystemAndConversation,
                ttl: Some("5m".into()),
            }),
            beta_features: vec!["context-1m-2025-08-07".into()],
            service_tier: None,
        };
        let j = serde_json::to_string(&opts).unwrap();
        let back: AnthropicOptions = serde_json::from_str(&j).unwrap();
        assert_eq!(opts, back);
    }

    #[test]
    fn provider_options_isolation() {
        let p = ProviderOptions {
            openai: Some(OpenAiOptions {
                parallel_tool_calls: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!p.is_empty());
        assert!(p.openai.is_some());
        assert!(p.anthropic.is_none());
    }

    #[test]
    fn cache_control_mode_snake_case() {
        let m = CacheControlMode::SystemAndConversation;
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            "\"system_and_conversation\""
        );
    }
}
