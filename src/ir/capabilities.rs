//! Honest, structured provider capability declarations.
//!
//! [`ProviderCapabilities`] replaces the previous `ProviderProfile`. It is
//! richer (more capability axes), it defaults to `Unsupported` rather than
//! `Native`, and each codec returns a `const` value where possible so the
//! agent runtime and budget tracker can make decisions without runtime
//! reflection.
//!
//! See plan §3 for the rationale behind each axis.

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

/// Tri-state support level for a single capability.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// The codec/provider does not support this capability at all.
    Unsupported,
    /// The codec emulates the capability on top of a primitive that the
    /// provider does support. Behaviour is best-effort and may not match a
    /// `Native` provider exactly.
    Emulated,
    /// The provider supports this capability natively.
    Native,
}

impl Support {
    /// `true` if the capability can be used at all (Native or Emulated).
    pub fn is_available(self) -> bool {
        matches!(self, Support::Emulated | Support::Native)
    }
}

/// Full capability declaration for a codec.
///
/// Codecs construct this as a `const` where possible so the agent runtime
/// and budget tracker can branch on capabilities without runtime reflection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// Stable identifier for the codec this profile describes.
    pub codec_id: &'static str,
    /// Streaming responses (SSE / EventStream / JsonArray).
    pub streaming: Support,
    /// Tool / function calling.
    pub tool_calls: ToolCallSupport,
    /// JSON mode / response_format / response_schema.
    pub structured_output: StructuredOutputSupport,
    /// Multi-modal input.
    pub vision: VisionSupport,
    /// Prompt / context caching.
    pub prompt_caching: CacheSupport,
    /// Reasoning / extended thinking.
    pub reasoning: ReasoningSupport,
    /// How the system prompt is wired in the underlying wire format.
    pub system_prompt: SystemPromptShape,
    /// Maximum context window in tokens (input + output combined where the
    /// provider treats them as a single budget; otherwise the input budget).
    pub max_context_tokens: u64,
    /// Server-side token counting endpoint.
    pub count_tokens: Support,
    /// Asynchronous batch submission.
    pub batch: Support,
}

impl ProviderCapabilities {
    /// Empty capabilities — every axis is `Unsupported`. Codecs build their
    /// real capabilities by starting from this and overriding fields.
    pub const fn unsupported(codec_id: &'static str) -> Self {
        Self {
            codec_id,
            streaming: Support::Unsupported,
            tool_calls: ToolCallSupport::unsupported(),
            structured_output: StructuredOutputSupport::unsupported(),
            vision: VisionSupport::unsupported(),
            prompt_caching: CacheSupport::unsupported(),
            reasoning: ReasoningSupport::unsupported(),
            system_prompt: SystemPromptShape::RoleMessage,
            max_context_tokens: 0,
            count_tokens: Support::Unsupported,
            batch: Support::Unsupported,
        }
    }
}

/// Tool calling capability detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallSupport {
    pub mode: Support,
    /// Multiple tool calls in a single assistant turn.
    pub parallel: Support,
    /// Strict JSON-schema validation by the provider.
    pub strict_schema: bool,
    /// How tool-call ↔ tool-result linkage is conveyed on the wire.
    pub id_semantics: ToolIdSemantics,
}

impl ToolCallSupport {
    pub const fn unsupported() -> Self {
        Self {
            mode: Support::Unsupported,
            parallel: Support::Unsupported,
            strict_schema: false,
            id_semantics: ToolIdSemantics::Provided,
        }
    }
}

/// How tool-call IDs are conveyed by a provider.
///
/// Anthropic and OpenAI both put a stable id on each tool call. Gemini does
/// not — its `functionCall` parts have only a name. The codec must
/// synthesize an id and the agent runtime must know which strategy was used
/// in order to re-pair `tool_result` parts on the next turn.
///
/// **Note**: synthesized-by-name was intentionally removed from this enum
/// because it cannot disambiguate parallel calls to the same function.
/// Codecs whose wire format omits ids must use [`ToolIdSemantics::SynthesizedByIndex`] and
/// the agent runtime must preserve part ordering when echoing tool results.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolIdSemantics {
    /// The provider returns a stable id on each tool call. Use it verbatim.
    Provided,
    /// No id on the wire; the codec synthesizes one from the positional
    /// index. The agent runtime must preserve part ordering for re-pairing.
    SynthesizedByIndex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredOutputSupport {
    pub json_object: Support,
    pub json_schema: Support,
    /// `true` if the provider validates the schema strictly server-side.
    pub strict: bool,
}

impl StructuredOutputSupport {
    pub const fn unsupported() -> Self {
        Self {
            json_object: Support::Unsupported,
            json_schema: Support::Unsupported,
            strict: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisionSupport {
    pub images: Support,
    pub pdfs: Support,
    pub video: Support,
    /// `true` if the provider accepts HTTP(S) URLs for media; `false` means
    /// the codec must inline media as base64.
    pub accepts_url: bool,
}

impl VisionSupport {
    pub const fn unsupported() -> Self {
        Self {
            images: Support::Unsupported,
            pdfs: Support::Unsupported,
            video: Support::Unsupported,
            accepts_url: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheSupport {
    pub mode: Support,
    pub granularity: CacheGranularity,
}

impl CacheSupport {
    pub const fn unsupported() -> Self {
        Self {
            mode: Support::Unsupported,
            granularity: CacheGranularity::None,
        }
    }
}

/// At what granularity cache markers can be placed.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheGranularity {
    /// No caching available.
    None,
    /// Cache the conversation as a whole (provider chooses cut points).
    Conversation,
    /// Cache up to a specific message boundary.
    Message,
    /// Per-content-block cache markers (Anthropic).
    Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningSupport {
    pub mode: Support,
    /// `true` if the provider returns the reasoning text (full or summary)
    /// in the response. Anthropic, Gemini, OpenAI Responses do; OpenAI Chat
    /// (o-series) does not.
    pub exposes_text: bool,
    /// `true` if the provider returns reasoning token counts in usage.
    pub exposes_tokens: bool,
    /// `true` if reasoning blocks carry an opaque signature that the agent
    /// runtime **must** echo back on the next turn (Anthropic extended
    /// thinking with tool use).
    pub requires_signature_passthrough: bool,
}

impl ReasoningSupport {
    pub const fn unsupported() -> Self {
        Self {
            mode: Support::Unsupported,
            exposes_text: false,
            exposes_tokens: false,
            requires_signature_passthrough: false,
        }
    }
}

/// How the system prompt is conveyed in the underlying wire format.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemPromptShape {
    /// Top-level field on the request body (Anthropic, Gemini
    /// `systemInstruction`, OpenAI Responses `instructions`, Bedrock
    /// Converse `system`).
    TopLevel,
    /// Conveyed as a `role: "system"` message at the front of the
    /// conversation (OpenAI Chat Completions).
    RoleMessage,
    /// The provider accepts both shapes.
    Both,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_const_initialises_all_axes() {
        const C: ProviderCapabilities = ProviderCapabilities::unsupported("test");
        assert_eq!(C.codec_id, "test");
        assert_eq!(C.streaming, Support::Unsupported);
        assert_eq!(C.tool_calls.mode, Support::Unsupported);
        assert_eq!(C.max_context_tokens, 0);
    }

    #[test]
    fn support_is_available() {
        assert!(!Support::Unsupported.is_available());
        assert!(Support::Emulated.is_available());
        assert!(Support::Native.is_available());
    }

    #[test]
    fn id_semantics_round_trips() {
        let s = ToolIdSemantics::SynthesizedByIndex;
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(j, "\"synthesized_by_index\"");
        let back: ToolIdSemantics = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn capabilities_can_be_built_by_const_override() {
        const C: ProviderCapabilities = ProviderCapabilities {
            streaming: Support::Native,
            tool_calls: ToolCallSupport {
                mode: Support::Native,
                parallel: Support::Native,
                strict_schema: false,
                id_semantics: ToolIdSemantics::Provided,
            },
            max_context_tokens: 200_000,
            ..ProviderCapabilities::unsupported("anthropic-messages")
        };
        assert_eq!(C.streaming, Support::Native);
        assert_eq!(C.max_context_tokens, 200_000);
    }
}
