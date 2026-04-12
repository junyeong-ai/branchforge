//! Portable model knobs that every codec understands (and may emit
//! [`ModelWarning`](super::warning::ModelWarning) for unsupported values).

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

/// Provider-agnostic generation knobs.
///
/// Settings here are the **portable subset** that every chat-style LLM
/// supports in some form. Provider-specific knobs (Anthropic `cache_control`,
/// OpenAI `logit_bias`, Gemini `safety_settings`) live on
/// [`ProviderOptions`](super::provider_options::ProviderOptions) instead.
///
/// Codecs that do not support a given setting (e.g. `seed` on Anthropic
/// Messages) emit a [`ModelWarning::UnsupportedSetting`](super::warning::ModelWarning::UnsupportedSetting)
/// rather than failing the call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Reasoning / extended thinking budget. Maps to Anthropic
    /// `thinking.budget_tokens`, OpenAI `reasoning.effort`, Gemini
    /// `thinkingConfig.thinkingBudget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningSettings>,
}

impl ModelSettings {
    /// Builder-style: set `max_output_tokens`.
    pub fn with_max_output_tokens(mut self, n: u32) -> Self {
        self.max_output_tokens = Some(n);
        self
    }

    /// Builder-style: set `temperature`.
    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }
}

/// Reasoning / extended thinking configuration.
///
/// Codecs translate this into the provider-native shape:
/// - Anthropic: `thinking: { type: "enabled", budget_tokens: <budget> }`
/// - OpenAI Responses / o-series: `reasoning: { effort: <effort> }`
/// - Gemini 2.5: `generationConfig.thinkingConfig: { thinkingBudget: <budget>, includeThoughts: true }`
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningSettings {
    /// Token budget for reasoning. Used by Anthropic and Gemini directly;
    /// OpenAI codecs map ranges to `effort: low|medium|high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u64>,
    /// Discrete effort level. Preferred for OpenAI; Anthropic/Gemini codecs
    /// translate to a default budget when `budget_tokens` is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Include the reasoning trace in the response (where supported).
    /// Defaults to `true`.
    #[serde(default = "default_include_thoughts")]
    pub include_thoughts: bool,
}

fn default_include_thoughts() -> bool {
    true
}

/// Discrete reasoning effort level.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_serialize_to_empty_object() {
        let s = ModelSettings::default();
        assert_eq!(serde_json::to_string(&s).unwrap(), "{}");
    }

    #[test]
    fn builder_chain() {
        let s = ModelSettings::default()
            .with_max_output_tokens(1024)
            .with_temperature(0.7);
        assert_eq!(s.max_output_tokens, Some(1024));
        assert_eq!(s.temperature, Some(0.7));
    }

    #[test]
    fn reasoning_default_includes_thoughts() {
        let r: ReasoningSettings = serde_json::from_str("{}").unwrap();
        assert!(r.include_thoughts);
    }

    #[test]
    fn reasoning_effort_snake_case() {
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::High).unwrap(),
            "\"high\""
        );
    }
}
