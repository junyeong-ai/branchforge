//! Top-level request and response types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::content::ContentPart;
use super::finish::FinishReason;
use super::provider_options::ProviderOptions;
use super::settings::ModelSettings;
use super::usage::Usage;
use super::warning::ModelWarning;

/// A complete model invocation request.
///
/// `ModelRequest` is the canonical input to a [`ModelCodec::encode_request`]
/// call. Codecs translate this into the provider-native wire format,
/// emitting [`ModelWarning`]s for anything they cannot honour exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    /// Model identifier as the provider expects to see it
    /// (e.g. `"claude-sonnet-4-5"`, `"gpt-4o-mini"`,
    /// `"gemini-2.5-flash"`, `"claude-sonnet-4-5@20250929"` for Vertex,
    /// `"global.anthropic.claude-sonnet-4-5-20250929-v1:0"` for Bedrock).
    pub model: String,

    /// Conversation history.
    pub messages: Vec<Message>,

    /// System prompt. Kept as a top-level field — and not as a
    /// [`Role::System`] message — because Anthropic supports structured
    /// system blocks with per-block `cache_control` that cannot round-trip
    /// through a flat message string. Codecs whose wire format expects a
    /// `role: "system"` message flatten on encode and emit a
    /// [`ModelWarning::LossyEncode`] if any block-level metadata is dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,

    /// Tool catalogue exposed to the model on this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,

    /// Tool selection strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,

    /// Structured output format. When set to [`ResponseFormat::JsonSchema`]
    /// the codec instructs the provider to constrain the model output to
    /// the given JSON Schema; codecs that only support free-form JSON
    /// (`json_object`) or that emulate the feature via tool calls emit a
    /// [`ModelWarning`] explaining the degradation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,

    /// Portable generation knobs (max_tokens, temperature, …).
    #[serde(default)]
    pub settings: ModelSettings,

    /// Per-provider typed extension knobs (Anthropic `cache_control`, OpenAI
    /// `logit_bias`, Gemini `safety_settings`, …). Codecs read only their
    /// own field; sibling fields are dropped with a warning.
    #[serde(default)]
    pub provider_options: ProviderOptions,

    /// Stateful continuation token, for providers that support server-side
    /// conversation state (OpenAI Responses `previous_response_id`). When
    /// set, [`Self::messages`] should contain only the delta since the
    /// continuation point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Continuation>,

    /// Free-form metadata attached to the request for downstream
    /// observability. Not sent to the provider unless the codec also reads
    /// it (OpenAI Responses `metadata`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,

    /// Optional client-supplied idempotency key. Transports that support
    /// it (Anthropic Messages: `Idempotency-Key` header) attach it to the
    /// outbound request so network retries are safe. Transports that do
    /// not support idempotency keys silently ignore this field — no
    /// information is lost because semantically idempotency is a
    /// best-effort safety net.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

impl ModelRequest {
    /// Construct a request with just a model and a list of messages.
    /// Everything else is default.
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            system: None,
            tools: Vec::new(),
            tool_choice: None,
            response_format: None,
            settings: ModelSettings::default(),
            provider_options: ProviderOptions::default(),
            continuation: None,
            metadata: BTreeMap::new(),
            idempotency_key: None,
        }
    }

    /// Builder-style: set [`response_format`](Self::response_format).
    pub fn with_response_format(mut self, format: ResponseFormat) -> Self {
        self.response_format = Some(format);
        self
    }

    /// Builder-style: set `max_output_tokens`.
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.settings.max_output_tokens = Some(n);
        self
    }
}

/// A complete (non-streaming) model response.
///
/// Streaming responses are assembled from [`ModelStreamChunk`](super::stream::ModelStreamChunk)
/// values into a final `ModelResponse` by the high-level `Client` API.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    /// Provider-assigned response id (e.g. Anthropic `msg_01...`, OpenAI
    /// `chatcmpl-...` or `resp_...`).
    pub id: String,

    /// Echo of the model that produced the response. Codecs that omit this
    /// from their wire format fill it in from the request.
    pub model: String,

    /// Assistant content (text, tool calls, reasoning, sources, …).
    pub content: Vec<ContentPart>,

    /// Why the model stopped generating.
    pub finish_reason: FinishReason,

    /// Token usage and accounting.
    pub usage: Usage,

    /// Continuation token to send back on the next turn for stateful
    /// providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<Continuation>,

    /// Non-fatal degradations the codec encountered while encoding the
    /// request or decoding the response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ModelWarning>,

    /// Verbatim provider response payload, kept for diagnostics.
    /// **Application logic must not read this field.**
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
}

impl ModelResponse {
    /// Sum of all text content concatenated. Convenience for the common
    /// "just give me the model's text reply" case.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for part in &self.content {
            if let ContentPart::Text { text } = part {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    /// Iterator over tool-call parts in the response.
    pub fn tool_calls(&self) -> impl Iterator<Item = &ContentPart> {
        self.content
            .iter()
            .filter(|p| matches!(p, ContentPart::ToolCall { .. }))
    }

    /// Construct a minimal response containing a single text part.
    ///
    /// Useful for tests and mocks where only the text payload matters.
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            id: String::new(),
            model: String::new(),
            content: vec![ContentPart::text(text)],
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
            continuation: None,
            warnings: Vec::new(),
            raw: None,
        }
    }
}

/// One conversation message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
}

impl Message {
    /// Construct a user message with a single text part.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentPart::text(text)],
        }
    }

    /// Construct an assistant message with a single text part.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentPart::text(text)],
        }
    }

    /// Construct a tool message carrying a single tool result.
    pub fn tool_result(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::tool_result_text(call_id, text)],
        }
    }

    /// Concatenate all text parts. Used by the session/agent layer.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| p.as_text())
            .collect::<Vec<_>>()
            .join("")
    }

    /// `true` if any content part is a tool call.
    pub fn has_tool_calls(&self) -> bool {
        self.content.iter().any(|p| p.is_tool_call())
    }
}

/// Conversation participant role.
///
/// **There is no `System` variant.** System prompts live on
/// [`ModelRequest::system`] as a top-level field, not as a message role.
/// This is the only intentional asymmetry in the IR; see the design notes
/// on [`ModelRequest::system`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

/// Top-level system prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    /// A single string. Lossless on every codec.
    Text(String),
    /// Structured blocks. Lossy on codecs whose wire format only supports a
    /// flat string.
    Blocks(Vec<SystemBlock>),
}

impl SystemPrompt {
    /// Flatten to a single string, joining blocks with double newlines.
    /// Used by codecs whose wire format expects a single system string.
    pub fn flatten(&self) -> String {
        match self {
            SystemPrompt::Text(s) => s.clone(),
            SystemPrompt::Blocks(blocks) => blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }

    /// `true` if any block carries metadata that cannot round-trip through
    /// a flat string. Codecs that flatten check this to decide whether to
    /// emit a [`ModelWarning::LossyEncode`].
    pub fn has_block_metadata(&self) -> bool {
        match self {
            SystemPrompt::Text(_) => false,
            SystemPrompt::Blocks(blocks) => blocks.iter().any(|b| b.cache_marker.is_some()),
        }
    }
}

impl From<String> for SystemPrompt {
    fn from(s: String) -> Self {
        SystemPrompt::Text(s)
    }
}

impl From<&str> for SystemPrompt {
    fn from(s: &str) -> Self {
        SystemPrompt::Text(s.to_owned())
    }
}

/// One block of a structured system prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemBlock {
    pub text: String,
    /// Per-block cache marker. Lossy on every codec that does not support
    /// per-block caching (currently only Anthropic Messages does).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_marker: Option<super::provider_options::CacheMarker>,
}

impl SystemBlock {
    /// Create a block with caching enabled (provider-default TTL).
    pub fn cached(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cache_marker: Some(super::provider_options::CacheMarker::ephemeral()),
        }
    }

    /// Create a block with caching and a specific TTL string (`"5m"`, `"1h"`).
    pub fn cached_with_ttl(text: impl Into<String>, ttl: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cache_marker: Some(super::provider_options::CacheMarker::with_ttl(ttl)),
        }
    }

    /// Create a block without caching.
    pub fn uncached(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cache_marker: None,
        }
    }
}

/// Tool / function definition exposed to the model.
///
/// The schema field is named `parameters` (not `input_schema`) to match the
/// OpenAI / Gemini / JSON-Schema convention. The Anthropic Messages codec
/// renames to `input_schema` on encode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the tool's input. Should be a JSON object schema.
    pub parameters: serde_json::Value,
    /// `true` if the codec should request strict server-side schema
    /// validation (OpenAI `strict: true`). Ignored by codecs that do not
    /// support strict mode.
    #[serde(default)]
    pub strict: bool,
}

impl ToolDefinition {
    /// Construct a tool with name and parameters schema only.
    pub fn new(name: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            description: None,
            parameters,
            strict: false,
        }
    }
}

/// Tool selection strategy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    /// Model decides whether to call a tool.
    Auto,
    /// Model must call exactly one tool.
    Required,
    /// Model is forbidden from calling tools on this turn.
    None,
    /// Model must call this specific tool.
    Tool { name: String },
}

/// Structured-output response format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    /// Plain text (default).
    Text,
    /// JSON object output. The model may produce any valid JSON object.
    JsonObject,
    /// JSON output conforming to the supplied JSON Schema.
    JsonSchema {
        name: String,
        schema: serde_json::Value,
        #[serde(default)]
        strict: bool,
    },
}

/// Stateful continuation handle for providers that retain conversation
/// state on their side (currently only OpenAI Responses).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Continuation {
    /// OpenAI Responses API stateful conversation. The
    /// `previous_response_id` is sent on the next turn so the provider
    /// can recover the prior context server-side.
    OpenAiResponses { previous_response_id: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_constructors() {
        let m = Message::user("hi");
        assert_eq!(m.role, Role::User);
        assert_eq!(m.content.len(), 1);

        let t = Message::tool_result("call_1", "ok");
        assert_eq!(t.role, Role::Tool);
    }

    #[test]
    fn role_serializes_snake_case_no_system() {
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"tool\"");
        // Ensure the enum has no System variant.
        let json = serde_json::to_string(&Role::Assistant).unwrap();
        assert_eq!(json, "\"assistant\"");
    }

    #[test]
    fn system_prompt_flattens_blocks() {
        let sp = SystemPrompt::Blocks(vec![
            SystemBlock {
                text: "first".into(),
                cache_marker: None,
            },
            SystemBlock {
                text: "second".into(),
                cache_marker: None,
            },
        ]);
        assert_eq!(sp.flatten(), "first\n\nsecond");
        assert!(!sp.has_block_metadata());
    }

    #[test]
    fn system_prompt_has_block_metadata_when_cache_marker_set() {
        use super::super::provider_options::CacheMarker;
        let sp = SystemPrompt::Blocks(vec![SystemBlock {
            text: "x".into(),
            cache_marker: Some(CacheMarker::ephemeral()),
        }]);
        assert!(sp.has_block_metadata());
    }

    #[test]
    fn model_request_default_constructor_is_minimal() {
        let r = ModelRequest::new("gpt-4o-mini", vec![Message::user("hi")]);
        assert_eq!(r.model, "gpt-4o-mini");
        assert!(r.tools.is_empty());
        assert!(r.system.is_none());
        assert!(r.provider_options.is_empty());
    }

    #[test]
    fn model_response_text_concatenates_text_parts() {
        let r = ModelResponse {
            id: "msg_1".into(),
            model: "claude-sonnet-4-5".into(),
            content: vec![
                ContentPart::text("hello"),
                ContentPart::text("world"),
                ContentPart::ToolCall {
                    id: "c1".into(),
                    name: "x".into(),
                    arguments: serde_json::json!({}),
                    origin: super::super::content::ToolOrigin::Local,
                },
            ],
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
            continuation: None,
            warnings: Vec::new(),
            raw: None,
        };
        assert_eq!(r.text(), "hello\nworld");
        assert_eq!(r.tool_calls().count(), 1);
    }

    #[test]
    fn tool_choice_round_trips() {
        let c = ToolChoice::Tool {
            name: "calc".into(),
        };
        let j = serde_json::to_string(&c).unwrap();
        let back: ToolChoice = serde_json::from_str(&j).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn continuation_round_trips() {
        let c = Continuation::OpenAiResponses {
            previous_response_id: "resp_123".into(),
        };
        let j = serde_json::to_string(&c).unwrap();
        let back: Continuation = serde_json::from_str(&j).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn response_format_json_schema_round_trips() {
        let rf = ResponseFormat::JsonSchema {
            name: "Person".into(),
            schema: serde_json::json!({"type": "object"}),
            strict: true,
        };
        let j = serde_json::to_string(&rf).unwrap();
        let back: ResponseFormat = serde_json::from_str(&j).unwrap();
        assert_eq!(rf, back);
    }
}
