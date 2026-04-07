//! Content parts — the neutral building blocks of a [`Message`](super::model::Message).
//!
//! [`ContentPart`] is the small, provider-neutral set of content variants
//! that any chat-style provider can express in some form. Anthropic-only,
//! OpenAI-only, or Gemini-only block types live behind
//! [`ContentPart::Unknown`] (with a schema version) so that they round-trip
//! through the originating codec but do not pollute the neutral surface.

use serde::{Deserialize, Serialize};

/// One semantic unit of content within a [`Message`](super::model::Message).
///
/// The variant set is deliberately small. Niche provider-specific block
/// types (Anthropic `redacted_thinking`, OpenAI `file_search_call`, …) are
/// surfaced through one of:
///
/// - [`ContentPart::ToolCall`] / [`ContentPart::ToolResult`] with
///   [`ToolOrigin::BuiltinServer`] for server-executed tools that share the
///   same call/response shape (web search, code execution, file search,
///   computer use).
/// - [`ContentPart::Reasoning`] with [`ReasoningContent::Redacted`] for
///   provider-encrypted reasoning passthrough (Anthropic
///   `redacted_thinking`, OpenAI Responses `encrypted_content`).
/// - [`ContentPart::Unknown`] as a last-resort escape hatch with a schema
///   version so the originating codec can verify "I wrote this, I can read
///   it back". Cross-codec sends drop the part with a warning rather than
///   silently succeeding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Plain text.
    Text { text: String },
    /// An image input or output.
    Image {
        source: MediaSource,
        /// MIME type, e.g. `"image/png"`.
        mime: String,
    },
    /// A document input (PDF, etc.).
    Document {
        source: MediaSource,
        /// MIME type, e.g. `"application/pdf"`.
        mime: String,
    },
    /// A citation or grounding source pointing at an external resource.
    /// Used for Anthropic citations, Gemini grounding metadata, and OpenAI
    /// Responses `web_search_call` results.
    Source {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snippet: Option<String>,
    },
    /// The assistant requests a tool / function call.
    ToolCall {
        /// Stable tool-call id. Codecs that don't carry an id on the wire
        /// (Gemini) synthesize one here; see
        /// [`ToolIdSemantics`](super::capabilities::ToolIdSemantics).
        id: String,
        /// Name of the tool / function being called.
        name: String,
        /// Arguments JSON object.
        arguments: serde_json::Value,
        /// Whether the tool is local to the agent runtime, a server-side
        /// builtin (web search, etc.), or an MCP server tool.
        #[serde(default)]
        origin: ToolOrigin,
    },
    /// The result of executing a tool, fed back to the model on the next turn.
    ToolResult {
        /// Must equal the `id` of a previous [`ContentPart::ToolCall`].
        tool_call_id: String,
        content: ToolResultContent,
        /// `true` if the tool execution failed; the model will see this as
        /// an error result and may retry.
        #[serde(default)]
        is_error: bool,
    },
    /// Model reasoning / extended thinking.
    Reasoning {
        content: ReasoningContent,
        kind: ReasoningKind,
        /// Opaque passthrough token. **Anthropic extended thinking with
        /// tool use REQUIRES this to be echoed back on the next turn**, or
        /// the tool-use sequence will fail validation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<ReasoningSignature>,
    },
    /// Last-resort escape hatch for content types this version of the IR
    /// does not model. Round-trips only through the originating codec.
    Unknown {
        /// Codec that produced the part. Used to gate re-encoding.
        codec_id: String,
        /// IR version of the originating codec at write time.
        schema_version: u32,
        payload: serde_json::Value,
    },
}

impl ContentPart {
    /// Convenience constructor for plain text.
    pub fn text(s: impl Into<String>) -> Self {
        ContentPart::Text { text: s.into() }
    }

    /// Convenience constructor for a tool result with text content.
    pub fn tool_result_text(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        ContentPart::ToolResult {
            tool_call_id: call_id.into(),
            content: ToolResultContent::Text(text.into()),
            is_error: false,
        }
    }

    /// Convenience constructor for a tool result that errored.
    pub fn tool_error(call_id: impl Into<String>, message: impl Into<String>) -> Self {
        ContentPart::ToolResult {
            tool_call_id: call_id.into(),
            content: ToolResultContent::Text(message.into()),
            is_error: true,
        }
    }

    /// Extract the text content if this is a `Text` part.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentPart::Text { text } => Some(text),
            _ => None,
        }
    }

    /// `true` if this is a reasoning/thinking part (visible or redacted).
    pub fn is_thinking(&self) -> bool {
        matches!(self, ContentPart::Reasoning { .. })
    }

    /// `true` if this is a tool call part.
    pub fn is_tool_call(&self) -> bool {
        matches!(self, ContentPart::ToolCall { .. })
    }

    /// `true` if this is a tool result part.
    pub fn is_tool_result(&self) -> bool {
        matches!(self, ContentPart::ToolResult { .. })
    }
}

/// Source of media content (image, document, …).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaSource {
    /// Inline base64-encoded data.
    Base64 { data: String },
    /// HTTP(S) URL. Only some providers accept URLs directly; see
    /// [`VisionSupport::accepts_url`](super::capabilities::VisionSupport::accepts_url).
    Url { url: String },
    /// A provider-side file id (OpenAI Files, Anthropic Files API, Gemini
    /// uploaded files).
    FileId { id: String },
}

/// Where the tool being called lives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOrigin {
    /// A locally registered tool dispatched by the agent runtime.
    #[default]
    Local,
    /// A server-side builtin tool (web search, code interpreter, file
    /// search, computer use, web fetch). The `namespace` identifies the
    /// builtin family.
    BuiltinServer { namespace: String },
    /// An MCP server tool. The `server_name` identifies the upstream MCP
    /// server.
    Mcp { server_name: String },
}

/// Content of a [`ContentPart::ToolResult`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// Plain text result.
    Text(String),
    /// Multi-part result (for tools that return text + images, etc.).
    /// Nested parts must not themselves be `ToolResult` to avoid
    /// pathological recursion.
    MultiPart(Vec<ContentPart>),
    /// Structured JSON result.
    Json(serde_json::Value),
}

/// Reasoning content visibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "visibility", rename_all = "snake_case")]
pub enum ReasoningContent {
    /// Reasoning text exposed by the provider.
    Visible { text: String },
    /// Reasoning that the provider returned in encrypted / opaque form
    /// (Anthropic `redacted_thinking.data`, OpenAI Responses
    /// `reasoning.encrypted_content`). The agent runtime should not attempt
    /// to display this; it must be passed back verbatim on the next turn.
    Redacted { data: String },
}

/// Whether the reasoning part is the model's full reasoning trace or only a
/// summary the provider chose to expose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningKind {
    /// Full chain-of-thought (Anthropic extended thinking, Gemini with
    /// `includeThoughts: true`).
    FullTrace,
    /// Provider-curated summary (OpenAI Responses reasoning items).
    Summary,
}

/// Newtype for opaque reasoning signatures. Wrapped in a struct to make the
/// "must round-trip verbatim" requirement obvious at call sites.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReasoningSignature(pub String);

impl ReasoningSignature {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_part_round_trips() {
        let p = ContentPart::text("hello");
        let j = serde_json::to_string(&p).unwrap();
        assert!(j.contains("\"type\":\"text\""));
        assert!(j.contains("\"text\":\"hello\""));
        let back: ContentPart = serde_json::from_str(&j).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn tool_call_default_origin_is_local() {
        let p = ContentPart::ToolCall {
            id: "call_1".into(),
            name: "calculator".into(),
            arguments: serde_json::json!({"a": 1, "b": 2}),
            origin: ToolOrigin::Local,
        };
        let j = serde_json::to_string(&p).unwrap();
        let back: ContentPart = serde_json::from_str(&j).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn tool_result_text_constructor() {
        let p = ContentPart::tool_result_text("call_1", "result");
        match p {
            ContentPart::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_call_id, "call_1");
                assert!(!is_error);
                match content {
                    ToolResultContent::Text(s) => assert_eq!(s, "result"),
                    _ => panic!("wrong content variant"),
                }
            }
            _ => panic!("wrong part variant"),
        }
    }

    #[test]
    fn reasoning_redacted_round_trips() {
        let p = ContentPart::Reasoning {
            content: ReasoningContent::Redacted {
                data: "OPAQUE_BLOB".into(),
            },
            kind: ReasoningKind::FullTrace,
            signature: Some(ReasoningSignature::new("sig_xyz")),
        };
        let j = serde_json::to_string(&p).unwrap();
        let back: ContentPart = serde_json::from_str(&j).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn unknown_part_carries_codec_and_version() {
        let p = ContentPart::Unknown {
            codec_id: "anthropic-messages".into(),
            schema_version: 1,
            payload: serde_json::json!({"foo": "bar"}),
        };
        let j = serde_json::to_string(&p).unwrap();
        assert!(j.contains("\"schema_version\":1"));
        let back: ContentPart = serde_json::from_str(&j).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn media_source_url_variant() {
        let s = MediaSource::Url {
            url: "https://example.com/x.png".into(),
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"type\":\"url\""));
    }

    #[test]
    fn tool_origin_default_is_local() {
        let o: ToolOrigin = Default::default();
        assert_eq!(o, ToolOrigin::Local);
    }
}
