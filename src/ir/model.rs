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
/// `ModelRequest` is the canonical input to a codec's `encode_request`
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
    /// `system` role message — because Anthropic supports structured
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
    /// Returns the model id used for **routing** (endpoint
    /// resolution / URL path), which may differ from
    /// [`Self::model`] when a provider option overrides it.
    ///
    /// Today this honours:
    /// - Bedrock `inference_profile` — when set, the Bedrock
    ///   Converse URL path uses the profile ARN/ID instead of the
    ///   declared model id, enabling cross-region failover and
    ///   quota pooling.
    ///
    /// Future provider-level routing overrides (Vertex publisher,
    /// OpenAI org alias, …) extend this single method without
    /// modifying transport / codec signatures. Callers (the
    /// provider client) use the routing id when calling
    /// `transport.resolve_endpoint`, and continue to use
    /// `self.model` when emitting the wire `model` field.
    pub fn routing_model_id(&self) -> &str {
        if let Some(opts) = &self.provider_options.bedrock
            && let Some(profile) = &opts.inference_profile
        {
            return profile.as_str();
        }
        &self.model
    }

    /// `true` if the request declares any prompt-cache markers in
    /// any provider-specific shape.
    ///
    /// Recognised shapes:
    /// - Anthropic top-level `cache_control` with at least one
    ///   breakpoint enabled,
    /// - Gemini `cached_content` resource reference,
    /// - Per-block `cache_marker` on any `SystemBlock`,
    /// - Structural `SystemBlockRole::Boundary` marker.
    ///
    /// Used by the provider client post-decode to detect unexpected
    /// cache breaks: if this returns `true` and the response reports
    /// zero `cached_input_tokens`, the upstream cache key was
    /// invalidated and observability should surface that.
    pub fn has_cache_markers(&self) -> bool {
        if let Some(opts) = &self.provider_options.anthropic
            && let Some(cc) = &opts.cache_control
            && cc.is_active()
        {
            return true;
        }
        if let Some(opts) = &self.provider_options.gemini
            && opts.cached_content.is_some()
        {
            return true;
        }
        if let Some(SystemPrompt::Blocks(blocks)) = &self.system
            && blocks
                .iter()
                .any(|b| b.cache_marker.is_some() || b.role.is_boundary())
        {
            return true;
        }
        false
    }

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

    /// Phase C-6: point-in-time rate-limit accounting parsed from
    /// provider response headers. `None` when the transport does not
    /// publish rate-limit headers or the codec-level response carries
    /// no HTTP context (e.g. streaming chunk reassembly path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<super::RateLimitSnapshot>,
}

impl ModelResponse {
    /// Parse the response's concatenated text content as `T`.
    ///
    /// Intended for use with [`ResponseFormat::JsonSchema`]: send a
    /// request with a JSON schema spec and call `.json::<T>()` on the
    /// response to deserialize it into a domain struct.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Parse`] in four distinct cases so the
    /// caller can disambiguate parse failures from model behavior:
    ///
    /// - **`FinishReason::ContentFilter`**: the model refused or was
    ///   filtered. The response text is typically a refusal message, not
    ///   schema-compliant JSON. Billed tokens were still consumed.
    /// - **`FinishReason::Length`**: the response was truncated by
    ///   `max_output_tokens`. The accumulated text is likely partial JSON.
    /// - Empty text content (no assistant text was returned).
    /// - `serde_json::from_str` failure on non-empty text.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use branchforge::client::LlmCall;
    /// use branchforge::ir::{JsonSchemaSpec, Message, ModelRequest, ResponseFormat};
    /// use schemars::JsonSchema;
    /// use serde::Deserialize;
    ///
    /// # async fn demo(client: impl LlmCall) -> branchforge::Result<()> {
    /// #[derive(JsonSchema, Deserialize, Debug)]
    /// struct Person {
    ///     name: String,
    ///     email: String,
    /// }
    ///
    /// let request = ModelRequest::new("claude-opus-4-6", vec![Message::user("...")])
    ///     .with_response_format(ResponseFormat::JsonSchema(
    ///         JsonSchemaSpec::from_type::<Person>()
    ///     ));
    /// let response = client.send(&request).await?;
    /// let person: Person = response.json()?;
    /// println!("{person:?}");
    /// # Ok(())
    /// # }
    /// ```
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> crate::Result<T> {
        use crate::Error;
        match self.finish_reason {
            FinishReason::ContentFilter => Err(Error::Parse(
                "ModelResponse::json: model refused or output was filtered; \
                 response does not match schema"
                    .to_string(),
            )),
            FinishReason::Length => Err(Error::Parse(
                "ModelResponse::json: response truncated by max_tokens; \
                 JSON is likely incomplete — retry with a larger max_output_tokens"
                    .to_string(),
            )),
            _ => {
                let text = self.text();
                if text.is_empty() {
                    return Err(Error::Parse(
                        "ModelResponse::json: response has no text content".to_string(),
                    ));
                }
                serde_json::from_str::<T>(&text).map_err(|e| {
                    Error::Parse(format!(
                        "ModelResponse::json: failed to parse response text as target type: {e}"
                    ))
                })
            }
        }
    }

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
            rate_limit: None,
        }
    }

    /// Construct a minimal response that calls a single tool.
    ///
    /// `finish_reason` is [`FinishReason::ToolCalls`] so the agent
    /// loop will dispatch the tool and continue. Useful for tests
    /// that script a multi-turn conversation where each turn is a
    /// discrete tool call.
    pub fn from_tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: String::new(),
            model: String::new(),
            content: vec![ContentPart::ToolCall {
                id: id.into(),
                name: name.into(),
                arguments,
                origin: crate::ir::ToolOrigin::Local,
            }],
            finish_reason: FinishReason::ToolCalls,
            usage: Usage::default(),
            continuation: None,
            warnings: Vec::new(),
            raw: None,
            rate_limit: None,
        }
    }

    /// Construct a response that emits a leading text part followed
    /// by a single tool call. Matches the common pattern where a
    /// model narrates its plan before invoking a tool.
    pub fn from_text_and_tool_call(
        text: impl Into<String>,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: String::new(),
            model: String::new(),
            content: vec![
                ContentPart::text(text),
                ContentPart::ToolCall {
                    id: id.into(),
                    name: name.into(),
                    arguments,
                    origin: crate::ir::ToolOrigin::Local,
                },
            ],
            finish_reason: FinishReason::ToolCalls,
            usage: Usage::default(),
            continuation: None,
            warnings: Vec::new(),
            raw: None,
            rate_limit: None,
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
#[non_exhaustive]
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
    /// Flatten to a single string, joining wire-visible blocks with
    /// double newlines.
    ///
    /// [`SystemBlockRole::Boundary`] blocks are **never** included —
    /// they exist only as a structural marker for cache-breakpoint
    /// placement. Including them would leak the marker text into the
    /// model's system prompt and corrupt every codec's cache key.
    pub fn flatten(&self) -> String {
        match self {
            SystemPrompt::Text(s) => s.clone(),
            SystemPrompt::Blocks(blocks) => blocks
                .iter()
                .filter(|b| !b.role.is_boundary())
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }

    /// `true` if any block carries metadata that cannot round-trip through
    /// a flat string. Codecs that flatten check this to decide whether to
    /// emit a [`ModelWarning::LossyEncode`].
    ///
    /// [`SystemBlockRole::Boundary`] blocks are excluded — they are
    /// not metadata, they are structural anchors that codecs handle
    /// uniformly via [`SystemPrompt::flatten`].
    pub fn has_block_metadata(&self) -> bool {
        match self {
            SystemPrompt::Text(_) => false,
            SystemPrompt::Blocks(blocks) => blocks
                .iter()
                .any(|b| !b.role.is_boundary() && b.cache_marker.is_some()),
        }
    }

    /// Position (index) of the [`SystemBlockRole::Boundary`] block,
    /// if present. Codecs that implement prompt caching use this to
    /// place their cache breakpoint on the block immediately
    /// preceding the boundary.
    pub fn boundary_index(&self) -> Option<usize> {
        match self {
            SystemPrompt::Text(_) => None,
            SystemPrompt::Blocks(blocks) => blocks.iter().position(|b| b.role.is_boundary()),
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

/// Semantic role of a [`SystemBlock`] within the system prompt.
///
/// The role is structural — it tells codecs how to handle the
/// block, independently of whether the block carries a per-block
/// `cache_marker`. The three roles answer three different questions:
///
/// - `Static` — content that is stable across many calls and is
///   eligible for prompt caching. Most blocks land here by default.
/// - `Dynamic` — content that changes between calls (active rules,
///   current date, last-tool output). Never cached. Always after
///   the [`SystemBlockRole::Boundary`].
/// - `Boundary` — a structural anchor with no wire content. The
///   block before the boundary is the last cacheable block; codecs
///   that implement prompt caching apply their cache breakpoint
///   there. The boundary block itself is **never serialised** to
///   the wire — every codec drops it via
///   [`SystemPrompt::flatten`] or its blocks-encoding equivalent.
///
/// This replaces the magic-string `SYSTEM_PROMPT_DYNAMIC_BOUNDARY`
/// approach: the role is a typed field that the IR contract
/// enforces uniformly, so a codec cannot accidentally let the
/// marker text leak into the system prompt.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemBlockRole {
    /// Stable, cache-eligible content. The default.
    #[default]
    Static,
    /// Per-call content. Never cached.
    Dynamic,
    /// Structural cache breakpoint marker. Never serialised to wire.
    Boundary,
}

impl SystemBlockRole {
    pub fn is_boundary(&self) -> bool {
        matches!(self, Self::Boundary)
    }
    pub fn is_dynamic(&self) -> bool {
        matches!(self, Self::Dynamic)
    }
    pub fn is_static(&self) -> bool {
        matches!(self, Self::Static)
    }
}

/// One block of a structured system prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemBlock {
    pub text: String,
    /// Structural role: `Static` (cacheable), `Dynamic` (per-call),
    /// or `Boundary` (structural marker, never serialised).
    /// Defaults to `Static` for `serde` round-trips of older
    /// payloads that pre-date the field.
    #[serde(default)]
    pub role: SystemBlockRole,
    /// Per-block cache marker. Lossy on every codec that does not support
    /// per-block caching (currently only Anthropic Messages does).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_marker: Option<super::provider_options::CacheMarker>,
}

impl SystemBlock {
    /// Create a static block with caching enabled (provider-default TTL).
    pub fn cached(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: SystemBlockRole::Static,
            cache_marker: Some(super::provider_options::CacheMarker::ephemeral()),
        }
    }

    /// Create a static block with caching and a specific TTL string
    /// (`"5m"`, `"1h"`).
    pub fn cached_with_ttl(text: impl Into<String>, ttl: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: SystemBlockRole::Static,
            cache_marker: Some(super::provider_options::CacheMarker::with_ttl(ttl)),
        }
    }

    /// Create a static block without caching.
    pub fn uncached(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: SystemBlockRole::Static,
            cache_marker: None,
        }
    }

    /// Create a dynamic (per-call, never-cached) block.
    pub fn dynamic(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: SystemBlockRole::Dynamic,
            cache_marker: None,
        }
    }

    /// Create the structural [`SystemBlockRole::Boundary`] marker.
    /// The block carries no wire-visible text; codecs drop it
    /// during encoding and apply their cache breakpoint to the
    /// preceding block.
    pub fn boundary() -> Self {
        Self {
            text: String::new(),
            role: SystemBlockRole::Boundary,
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
#[non_exhaustive]
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
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    /// Plain text (default).
    Text,
    /// JSON object output. The model may produce any valid JSON object.
    JsonObject,
    /// JSON output conforming to the supplied JSON Schema.
    JsonSchema(JsonSchemaSpec),
}

/// Specification for a JSON-Schema-constrained response format.
///
/// Carries the raw JSON schema plus two optional pieces of metadata
/// (`name`, `description`) that some providers surface on the wire and
/// others drop. The `strict` flag requests provider-side schema
/// validation when available.
///
/// # Wire support matrix
///
/// | Provider              | `schema` | `name` | `description` | `strict` |
/// | :-------------------- | :------: | :----: | :-----------: | :------: |
/// | OpenAI Chat           | ✅       | ✅     | ✅            | ✅       |
/// | OpenAI Responses      | ✅       | ✅     | ✅            | ✅       |
/// | Anthropic Messages    | ✅       | ❌ †   | ❌ †          | implicit |
/// | Gemini GenerateContent| ✅       | ❌ †   | ❌ †          | implicit |
/// | Bedrock Converse      | ✅       | ✅     | ✅            | implicit |
///
/// † Codecs emit a [`crate::ir::ModelWarning::LossyEncode`] when `name` or
/// `description` is set on a provider that has no wire field for it, so
/// the caller knows the value was dropped.
///
/// # Anthropic property ordering
///
/// Anthropic's grammar-constrained decoder emits required properties
/// first (in the order they appear in the schema), then optional
/// properties (in the order they appear). Callers that care about output
/// order should mark every property as required, or reorder after
/// parsing.
///
/// # Token cost
///
/// Providers that support native structured output inject an extra
/// system prompt explaining the expected shape, slightly increasing the
/// input token count compared to an unconstrained call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonSchemaSpec {
    /// Raw JSON Schema (pre-transformation). Codecs run their own
    /// [`crate::client::schema::SchemaPolicy`] on this value at encode
    /// time, so the spec carries the user's original intent rather than
    /// a provider-specific subset.
    pub schema: serde_json::Value,
    /// Human-readable schema name. See the wire support matrix above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Human-readable schema description. See the wire support matrix above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Request provider-side strict validation. Anthropic is always
    /// grammar-constrained regardless of this flag. OpenAI honours it as
    /// a legacy toggle; `false` relaxes constraint enforcement.
    ///
    /// **Default on deserialization is `true`** to match the constructor
    /// default (`JsonSchemaSpec::new` → `strict: true`). Without this
    /// explicit default, a spec deserialized from `{"schema": {...}}`
    /// (no `strict` field) would silently use `bool::default() == false`
    /// and produce different validation behaviour from a constructed spec.
    #[serde(default = "default_strict")]
    pub strict: bool,
}

#[inline]
fn default_strict() -> bool {
    true
}

impl JsonSchemaSpec {
    /// Construct a spec from a raw JSON Schema. `name` and `description`
    /// default to `None`; `strict` defaults to `true` (the common case).
    pub fn new(schema: serde_json::Value) -> Self {
        Self {
            schema,
            name: None,
            description: None,
            strict: true,
        }
    }

    /// Set the human-readable schema name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the human-readable schema description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the strict-validation flag. Defaults to `true`.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Derive a spec from a Rust type `T` that implements
    /// [`schemars::JsonSchema`]. The type's short name (without the
    /// module path) is used as `name`.
    ///
    /// # Example
    ///
    /// ```
    /// use branchforge::ir::JsonSchemaSpec;
    /// use schemars::JsonSchema;
    /// use serde::Deserialize;
    ///
    /// #[derive(JsonSchema, Deserialize)]
    /// struct Person {
    ///     name: String,
    ///     email: String,
    /// }
    ///
    /// let spec = JsonSchemaSpec::from_type::<Person>();
    /// assert_eq!(spec.name.as_deref(), Some("Person"));
    /// ```
    ///
    /// # Generic types
    ///
    /// For `Vec<T>`, `Box<T>`, and other generics, the name resolves to
    /// the outer type's short name (e.g. `"Vec"`), because
    /// `std::any::type_name` returns the fully-qualified form
    /// `"alloc::vec::Vec<crate::Person>"` and this function splits the
    /// generic arguments away before taking the last path segment. Using
    /// the outer type name avoids producing malformed identifiers like
    /// `"Person>"` that would otherwise bleed into the wire `name` field.
    pub fn from_type<T: schemars::JsonSchema>() -> Self {
        let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default();
        Self::new(schema).with_name(short_type_name::<T>())
    }
}

/// Extract the short name of `T` from `std::any::type_name`, handling
/// generic parameters by splitting on `<` before the `::` split. For
/// `crate::Person` → `"Person"`; for `Vec<Person>` → `"Vec"`.
fn short_type_name<T>() -> &'static str {
    let full = std::any::type_name::<T>();
    let non_generic = full.split('<').next().unwrap_or(full);
    non_generic.rsplit("::").next().unwrap_or("Schema")
}

/// Stateful continuation handle for providers that retain conversation
/// state on their side (currently only OpenAI Responses).
#[non_exhaustive]
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
                role: SystemBlockRole::Static,
                cache_marker: None,
            },
            SystemBlock {
                text: "second".into(),
                role: SystemBlockRole::Static,
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
            role: SystemBlockRole::Static,
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
    fn routing_model_id_returns_inference_profile_when_set() {
        use super::super::provider_options::BedrockOptions;
        let mut req = ModelRequest::new("anthropic.claude-sonnet-4-5", vec![]);
        assert_eq!(req.routing_model_id(), "anthropic.claude-sonnet-4-5");

        req.provider_options.bedrock = Some(BedrockOptions {
            inference_profile: Some("us.anthropic.claude-sonnet-4-5-v1:0".into()),
            ..Default::default()
        });
        assert_eq!(
            req.routing_model_id(),
            "us.anthropic.claude-sonnet-4-5-v1:0"
        );
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
            rate_limit: None,
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
    fn model_response_json_parses_happy_path() {
        use serde::Deserialize;

        #[derive(Deserialize, PartialEq, Debug)]
        struct Person {
            name: String,
            age: u32,
        }

        let response = ModelResponse::from_text(r#"{"name":"Alice","age":30}"#);
        let person: Person = response.json().unwrap();
        assert_eq!(
            person,
            Person {
                name: "Alice".into(),
                age: 30
            }
        );
    }

    #[test]
    fn model_response_json_returns_distinctive_error_on_refusal() {
        let mut response = ModelResponse::from_text("I cannot help with that.");
        response.finish_reason = FinishReason::ContentFilter;
        let err = response.json::<serde_json::Value>().unwrap_err();
        let message = err.to_string();
        assert!(message.contains("refused") || message.contains("filtered"));
    }

    #[test]
    fn model_response_json_returns_distinctive_error_on_length_truncation() {
        let mut response = ModelResponse::from_text(r#"{"name":"Al"#);
        response.finish_reason = FinishReason::Length;
        let err = response.json::<serde_json::Value>().unwrap_err();
        let message = err.to_string();
        assert!(message.contains("truncated") || message.contains("max_tokens"));
    }

    #[test]
    fn model_response_json_returns_parse_error_on_invalid_json() {
        let response = ModelResponse::from_text("not even close to json");
        let err = response.json::<serde_json::Value>().unwrap_err();
        assert!(err.to_string().contains("failed to parse"));
    }

    #[test]
    fn model_response_json_returns_error_on_empty_text() {
        let response = ModelResponse::from_text("");
        let err = response.json::<serde_json::Value>().unwrap_err();
        assert!(err.to_string().contains("no text content"));
    }

    #[test]
    fn response_format_json_schema_round_trips() {
        let rf = ResponseFormat::JsonSchema(
            JsonSchemaSpec::new(serde_json::json!({"type": "object"}))
                .with_name("Person")
                .with_description("A person record")
                .with_strict(true),
        );
        let j = serde_json::to_string(&rf).unwrap();
        let back: ResponseFormat = serde_json::from_str(&j).unwrap();
        assert_eq!(rf, back);
    }

    #[test]
    fn json_schema_spec_new_defaults_to_strict_and_no_metadata() {
        let spec = JsonSchemaSpec::new(serde_json::json!({"type": "object"}));
        assert!(spec.name.is_none());
        assert!(spec.description.is_none());
        assert!(spec.strict);
    }

    #[test]
    fn json_schema_spec_builder_chains() {
        let spec = JsonSchemaSpec::new(serde_json::json!({"type": "string"}))
            .with_name("Greeting")
            .with_description("A greeting string")
            .with_strict(false);
        assert_eq!(spec.name.as_deref(), Some("Greeting"));
        assert_eq!(spec.description.as_deref(), Some("A greeting string"));
        assert!(!spec.strict);
    }

    #[test]
    fn json_schema_spec_from_type_uses_short_type_name() {
        use schemars::JsonSchema;

        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct ContactInfo {
            name: String,
            email: String,
        }

        let spec = JsonSchemaSpec::from_type::<ContactInfo>();
        assert_eq!(spec.name.as_deref(), Some("ContactInfo"));
        assert!(spec.schema.is_object());
        assert!(spec.strict);
    }

    #[test]
    fn json_schema_spec_from_type_strips_generic_parameters_from_name() {
        use schemars::JsonSchema;

        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct Item {
            value: String,
        }

        // `Vec<Item>` → name should be `"Vec"`, not `"Item>"` or similar.
        // The pre-fix implementation used `rsplit("::")` which would produce
        // the buggy `"Item>"` tail because `::` doesn't split on `<`.
        let spec = JsonSchemaSpec::from_type::<Vec<Item>>();
        let name = spec.name.as_deref().unwrap();
        assert!(
            !name.contains('>') && !name.contains('<'),
            "name must not contain angle brackets, got: {name}"
        );
        assert_eq!(name, "Vec");
    }

    #[test]
    fn json_schema_spec_deserialize_defaults_strict_to_true() {
        // The constructor default is `strict: true`. The serde default
        // must match — otherwise a spec deserialized from JSON that
        // omits `strict` silently falls back to `false`.
        let json = serde_json::json!({
            "schema": {"type": "object"}
        });
        let spec: JsonSchemaSpec = serde_json::from_value(json).unwrap();
        assert!(
            spec.strict,
            "deserialized JsonSchemaSpec must default `strict` to true to match the constructor"
        );
    }
}
