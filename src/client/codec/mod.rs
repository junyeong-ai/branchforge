//! `ModelCodec` — wire-format encoders/decoders.
//!
//! A [`ModelCodec`] is a **pure** translator between the neutral
//! [`crate::ir`] IR and one provider's wire format. It owns no HTTP client,
//! no credentials, and no endpoint URLs — those concerns belong to
//! [`crate::client::transport::ModelTransport`]. The split is what makes
//! Vertex-Gemini fall out as a free composition: the same
//! [`anthropic_messages::AnthropicMessagesCodec`] body builder pairs with
//! `DirectTransport`, `BedrockTransport`, `VertexTransport(publisher=anthropic)`,
//! and `FoundryTransport`; while [`gemini_generate::GeminiGenerateCodec`]
//! pairs with `DirectTransport` *and* `VertexTransport(publisher=google)`.
//!
//! ## Endpoint shape
//!
//! Codecs do not know about URLs. Instead, they expose an [`EndpointShape`]
//! — a static descriptor of the URL pattern, verb names, query parameters,
//! and required headers their wire format expects. Transports consume the
//! shape and produce a concrete [`crate::client::transport::Endpoint`].
//! This is the third axis (alongside *codec* and *transport*) that keeps
//! the abstraction orthogonal.
//!
//! See `/Users/mac/.claude/plans/snug-strolling-crane.md` §1.

pub mod anthropic_messages;
pub mod bedrock_converse;
pub mod gemini_generate;
pub mod openai_chat;
pub mod openai_responses;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::ir::{
    ModelRequest, ModelResponse, ModelStreamChunk, ProviderCapabilities, StreamDecodeState,
    StreamFraming,
};

pub use anthropic_messages::AnthropicMessagesCodec;
pub use bedrock_converse::BedrockConverseCodec;
pub use gemini_generate::GeminiGenerateCodec;
pub use openai_chat::OpenAiChatCodec;
pub use openai_responses::OpenAiResponsesCodec;

/// Pure encoder/decoder for one provider wire format.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// threads inside `Arc<dyn ModelCodec>`. They should be cheap to clone or
/// (better) zero-state — all per-call state lives on the
/// [`StreamDecodeState`] passed into [`Self::decode_stream_chunk`].
pub trait ModelCodec: Send + Sync + std::fmt::Debug {
    /// Stable identifier for this codec, e.g. `"anthropic-messages"`,
    /// `"openai-responses"`, `"gemini-generate"`. Used by transports to
    /// dispatch and by tests to identify scenarios.
    fn id(&self) -> &'static str;

    /// Capability declaration. Should be a `const` value where possible.
    fn capabilities(&self) -> &'static ProviderCapabilities;

    /// URL/header pattern this codec needs from a transport.
    fn endpoint_shape(&self) -> &'static EndpointShape;

    /// Whether this codec supports the given invocation mode. Defaults to
    /// `Unary` and `Stream`; codecs that also support batches override.
    fn supports_mode(&self, mode: InvocationMode) -> bool {
        matches!(mode, InvocationMode::Unary | InvocationMode::Stream)
    }

    /// `Some(transport_id)` if this codec is intrinsically pinned to a
    /// single transport (e.g. `BedrockConverseCodec` to `"bedrock"`). The
    /// `Client` builder rejects compositions that violate the pin.
    fn pinned_transport(&self) -> Option<&'static str> {
        None
    }

    /// Encode a [`ModelRequest`] into the on-the-wire request body for the
    /// given invocation mode.
    fn encode_request(
        &self,
        request: &ModelRequest,
        mode: InvocationMode,
    ) -> Result<EncodedRequest>;

    /// Decode an on-the-wire response payload into a [`ModelResponse`].
    fn decode_response(
        &self,
        raw: serde_json::Value,
        mode: InvocationMode,
    ) -> Result<ModelResponse>;

    /// Decode one wire-format frame into zero or more [`ModelStreamChunk`]s.
    ///
    /// `state` is codec-private mutable state carried across calls; it is
    /// what allows snapshot-based decoders (Gemini SSE) to compute deltas.
    /// Delta-based decoders typically ignore it.
    fn decode_stream_chunk(
        &self,
        frame: &[u8],
        state: &mut StreamDecodeState,
    ) -> Result<Vec<ModelStreamChunk>>;

    /// Decode one AWS EventStream frame (binary framing) into chunks.
    ///
    /// Default implementation forwards `payload` to
    /// [`Self::decode_stream_chunk`] and ignores `event_type`. Codecs whose
    /// wire format multiplexes event kinds via the AWS `:event-type`
    /// header (Bedrock Converse) override this to dispatch on `event_type`.
    fn decode_eventstream_frame(
        &self,
        _event_type: &str,
        payload: &[u8],
        state: &mut StreamDecodeState,
    ) -> Result<Vec<ModelStreamChunk>> {
        self.decode_stream_chunk(payload, state)
    }

    /// Wire-level framing of the streaming response body.
    fn stream_framing(&self) -> StreamFraming {
        StreamFraming::Sse
    }
}

/// What kind of invocation we are encoding for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationMode {
    /// Single request → single response.
    Unary,
    /// Single request → streaming response.
    Stream,
    /// Batched submission for asynchronous processing.
    Batch,
}

/// One header that the wire format requires the transport to send on every
/// request. The transport substitutes [`HeaderSource::ContextValue`]
/// placeholders from its own context (project id, region, etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderSpec {
    /// HTTP header name.
    pub name: &'static str,
    /// Where the value comes from.
    pub source: HeaderSource,
}

/// Where the value of a [`HeaderSpec`] comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderSource {
    /// A literal constant value the codec hard-codes (e.g.
    /// `anthropic-version: 2023-06-01`).
    Literal(&'static str),
    /// A context value the transport substitutes at request time. The
    /// `&'static str` is a key the transport recognises (e.g.
    /// `"quota_project"` for GCP `x-goog-user-project`).
    ContextValue(&'static str),
}

/// Hint for which API version the codec targets, used by transports that
/// route between stable and beta endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiVersionHint {
    /// Whatever stable version the transport considers default.
    Stable,
    /// The transport's beta endpoint, if any.
    Beta,
    /// A specific version literal pinned by the codec.
    PinnedTo(&'static str),
}

/// URL/header pattern that a codec needs from a transport.
///
/// `EndpointShape` is the bridge between the *codec* axis (wire format) and
/// the *transport* axis (auth + URL). It is a `const`-friendly descriptor
/// that the transport reads to compute a concrete
/// [`crate::client::transport::Endpoint`].
///
/// # Path templates
///
/// `path_template` is a transport-relative URL template with two
/// placeholders:
///
/// - `{model}` — substituted with the request's model id.
/// - `{verb}` — substituted with `verb_unary` for unary calls and
///   `verb_stream` for streaming calls.
///
/// Examples:
///
/// - Anthropic Messages: `path_template = "v1/messages"`,
///   `verb_unary = ""`, `verb_stream = ""`.
/// - OpenAI Chat Completions: `"v1/chat/completions"`, no verbs.
/// - Gemini GenerateContent:
///   `path_template = "v1beta/models/{model}:{verb}"`,
///   `verb_unary = "generateContent"`,
///   `verb_stream = "streamGenerateContent"`,
///   `stream_query = &[("alt", "sse")]`.
/// - Vertex Anthropic:
///   `path_template = "v1/projects/{project}/locations/{location}/publishers/anthropic/models/{model}:{verb}"`,
///   `verb_unary = "rawPredict"`,
///   `verb_stream = "streamRawPredict"`. Transport substitutes
///   `{project}`/`{location}` from its own context.
#[derive(Clone, Copy, Debug)]
pub struct EndpointShape {
    /// Transport-relative URL template (no scheme/host).
    pub path_template: &'static str,
    /// Verb substituted for `{verb}` in unary mode.
    pub verb_unary: &'static str,
    /// Verb substituted for `{verb}` in streaming mode.
    pub verb_stream: &'static str,
    /// Query parameters added in streaming mode (e.g. `[("alt", "sse")]`).
    pub stream_query: &'static [(&'static str, &'static str)],
    /// Headers the codec requires the transport to send on every request.
    pub required_headers: &'static [HeaderSpec],
    /// Hint for which API version the transport should route to.
    pub api_version_hint: ApiVersionHint,
}

/// Output of [`ModelCodec::encode_request`] — the request body plus any
/// codec-emitted warnings (e.g. dropped sibling provider options).
#[derive(Clone, Debug)]
pub struct EncodedRequest {
    /// JSON body to send as the HTTP request payload.
    pub body: serde_json::Value,
    /// Non-fatal degradations the codec encountered while encoding. These
    /// flow into the final `ModelResponse::warnings`.
    pub warnings: Vec<crate::ir::ModelWarning>,
}

impl EncodedRequest {
    /// Construct an `EncodedRequest` with no warnings.
    pub fn new(body: serde_json::Value) -> Self {
        Self {
            body,
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invocation_mode_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&InvocationMode::Unary).unwrap(),
            "\"unary\""
        );
        assert_eq!(
            serde_json::to_string(&InvocationMode::Stream).unwrap(),
            "\"stream\""
        );
    }

    #[test]
    fn endpoint_shape_can_be_const() {
        const SHAPE: EndpointShape = EndpointShape {
            path_template: "v1/messages",
            verb_unary: "",
            verb_stream: "",
            stream_query: &[],
            required_headers: &[HeaderSpec {
                name: "anthropic-version",
                source: HeaderSource::Literal("2023-06-01"),
            }],
            api_version_hint: ApiVersionHint::Stable,
        };
        assert_eq!(SHAPE.path_template, "v1/messages");
        assert_eq!(SHAPE.required_headers.len(), 1);
    }

    #[test]
    fn encoded_request_new_has_no_warnings() {
        let r = EncodedRequest::new(serde_json::json!({"x": 1}));
        assert!(r.warnings.is_empty());
    }
}
