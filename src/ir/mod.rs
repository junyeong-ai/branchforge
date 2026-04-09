//! Provider-neutral intermediate representation for model requests, responses,
//! and streaming chunks.
//!
//! This module is the **canonical IR** that branchforge uses internally to
//! describe a single round-trip with any LLM provider, regardless of wire
//! format. Codecs translate between this IR and the on-the-wire shape of a
//! particular provider (Anthropic Messages, OpenAI Responses, OpenAI Chat
//! Completions, Gemini GenerateContent, Bedrock Converse, …).
//!
//! Design goals:
//!
//! - **Provider-neutral**: no field is shaped after a single vendor's wire
//!   format. Where vendors disagree, the IR picks the most general shape and
//!   codecs translate on encode/decode.
//! - **Lossless within a codec**: a `(req, resp)` pair that originated from
//!   codec X round-trips through X without semantic loss.
//! - **Observably lossy across codecs**: when a field cannot be honoured by
//!   the receiving codec, the codec emits a [`ModelWarning`] rather than
//!   silently dropping the field.
//! - **Honest capabilities**: each codec publishes a [`ProviderCapabilities`]
//!   value with `Unsupported` defaults; consumers (budget, agent runtime)
//!   read it instead of assuming feature parity.
//!
//! See `/Users/mac/.claude/plans/snug-strolling-crane.md` (sections 1–3) for
//! the design rationale.
//!
//! # Module layout
//!
//! - [`model`] — [`ModelRequest`], [`ModelResponse`], [`Message`], [`Role`],
//!   [`ToolDefinition`], [`ToolChoice`], [`ResponseFormat`], [`Continuation`],
//!   [`SystemPrompt`].
//! - [`content`] — [`ContentPart`] and its sub-enums (media, tool, reasoning).
//! - [`stream`] — [`ModelStreamChunk`], [`StreamFraming`], [`StreamDecodeState`].
//! - [`settings`] — [`ModelSettings`], [`ReasoningSettings`].
//! - [`provider_options`] — [`ProviderOptions`] and the per-provider typed
//!   extension structs.
//! - [`usage`] — [`Usage`], [`PartialUsage`], [`ServerToolInvocations`].
//! - [`finish`] — [`FinishReason`].
//! - [`warning`] — [`ModelWarning`].
//! - [`capabilities`] — [`ProviderCapabilities`] and sub-structs.

pub mod capabilities;
pub mod content;
pub mod finish;
pub mod model;
pub mod provider_options;
pub mod settings;
pub mod stream;
pub mod token_count;
pub mod usage;
pub mod warning;

pub use capabilities::{
    CacheGranularity, CacheSupport, ProviderCapabilities, ReasoningSupport,
    StructuredOutputSupport, Support, SystemPromptShape, ToolCallSupport, ToolIdSemantics,
    VisionSupport,
};
pub use content::{
    ContentPart, MediaSource, ReasoningContent, ReasoningKind, ReasoningSignature, ToolOrigin,
    ToolResultContent,
};
pub use finish::FinishReason;
pub use model::{
    Continuation, Message, ModelRequest, ModelResponse, ResponseFormat, Role, SystemBlock,
    SystemPrompt, ToolChoice, ToolDefinition,
};
pub use provider_options::{
    AnthropicOptions, BedrockGuardrail, BedrockOptions, CacheControl, CacheMarker, GeminiOptions,
    OpenAiOptions, ProviderOptions, ReasoningEffort, SafetySetting, VertexOptions,
};
pub use settings::{ModelSettings, ReasoningSettings};
pub use stream::{ModelStreamChunk, PartialUsage, StreamDecodeState, StreamFraming};
pub use token_count::TokenCount;
pub use usage::{ServerToolInvocations, Usage};
pub use warning::ModelWarning;
