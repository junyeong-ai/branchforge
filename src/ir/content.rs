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
        /// Name of the tool whose result this is. Optional because not every
        /// codec carries the name on the wire — Anthropic and OpenAI echo
        /// only the id, but **Gemini's `functionResponse` requires the
        /// name** to be present. The agent runtime should populate this
        /// from the matching [`ContentPart::ToolCall::name`] whenever
        /// possible so that round-trips through Gemini do not lose data.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
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
    ///
    /// `tool_name` defaults to `None`. Use [`Self::with_tool_name`] to
    /// attach a name when the agent layer knows it (Gemini requires it).
    pub fn tool_result_text(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        ContentPart::ToolResult {
            tool_call_id: call_id.into(),
            tool_name: None,
            content: ToolResultContent::Text(text.into()),
            is_error: false,
        }
    }

    /// Convenience constructor for a tool result that errored.
    pub fn tool_error(call_id: impl Into<String>, message: impl Into<String>) -> Self {
        ContentPart::ToolResult {
            tool_call_id: call_id.into(),
            tool_name: None,
            content: ToolResultContent::Text(message.into()),
            is_error: true,
        }
    }

    /// Build an IR `ToolResult` content part from a tool execution result.
    ///
    /// This is the canonical conversion point used by the agent layer.
    /// Multi-block results (text + images + search results) are preserved
    /// via [`ToolResultContent::MultiPart`]; pure text/error/empty results
    /// use [`ToolResultContent::Text`].
    ///
    /// `tool_name` defaults to `None`. Chain [`Self::with_tool_name`] when
    /// the agent layer has the name (it almost always does because the
    /// matching [`ContentPart::ToolCall`] carries it).
    pub fn from_tool_result(call_id: impl Into<String>, result: &crate::types::ToolResult) -> Self {
        use crate::types::{ToolOutput, ToolOutputBlock};
        let call_id = call_id.into();
        match &result.output {
            ToolOutput::Success(text) => ContentPart::ToolResult {
                tool_call_id: call_id,
                tool_name: None,
                content: ToolResultContent::Text(text.clone()),
                is_error: false,
            },
            ToolOutput::SuccessBlocks(blocks) => {
                // Preserve text/image blocks as a MultiPart so downstream
                // consumers and the codec layer can render them faithfully.
                let parts: Vec<ContentPart> = blocks
                    .iter()
                    .map(|b| match b {
                        ToolOutputBlock::Text { text } => ContentPart::text(text.clone()),
                        ToolOutputBlock::Image { data, media_type } => ContentPart::Image {
                            source: MediaSource::Base64 { data: data.clone() },
                            mime: media_type.clone(),
                        },
                    })
                    .collect();
                ContentPart::ToolResult {
                    tool_call_id: call_id,
                    tool_name: None,
                    content: ToolResultContent::MultiPart(parts),
                    is_error: false,
                }
            }
            ToolOutput::Error(e) => ContentPart::ToolResult {
                tool_call_id: call_id,
                tool_name: None,
                content: ToolResultContent::Text(e.to_string()),
                is_error: true,
            },
            ToolOutput::Empty => ContentPart::ToolResult {
                tool_call_id: call_id,
                tool_name: None,
                content: ToolResultContent::Text(String::new()),
                is_error: false,
            },
        }
    }

    /// Builder: attach a tool name to a `ContentPart::ToolResult`. No-op
    /// for any other variant. The name is required by Gemini and ignored
    /// by codecs that echo only the id.
    pub fn with_tool_name(mut self, name: impl Into<String>) -> Self {
        if let ContentPart::ToolResult {
            ref mut tool_name, ..
        } = self
        {
            *tool_name = Some(name.into());
        }
        self
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
                tool_name,
                content,
                is_error,
            } => {
                assert_eq!(tool_call_id, "call_1");
                assert!(tool_name.is_none());
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
    fn with_tool_name_attaches_name_to_tool_result() {
        let p = ContentPart::tool_result_text("call_1", "result").with_tool_name("calculator");
        match p {
            ContentPart::ToolResult {
                tool_name: Some(name),
                ..
            } => assert_eq!(name, "calculator"),
            _ => panic!("expected ToolResult with tool_name set"),
        }
    }

    #[test]
    fn with_tool_name_is_noop_on_non_tool_result() {
        let p = ContentPart::text("hello").with_tool_name("calculator");
        assert!(matches!(p, ContentPart::Text { .. }));
    }

    #[test]
    fn tool_name_round_trips_through_serde() {
        let p = ContentPart::tool_result_text("call_1", "ok").with_tool_name("calc");
        let j = serde_json::to_string(&p).unwrap();
        assert!(j.contains("\"tool_name\":\"calc\""));
        let back: ContentPart = serde_json::from_str(&j).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn missing_tool_name_serializes_as_absent_field() {
        let p = ContentPart::tool_result_text("call_1", "ok");
        let j = serde_json::to_string(&p).unwrap();
        assert!(!j.contains("tool_name"));
        let back: ContentPart = serde_json::from_str(&j).unwrap();
        assert_eq!(p, back);
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

    #[test]
    fn tool_result_content_text_serde() {
        // Scenario 1: Deserialize plain string "hello" → should be ToolResultContent::Text
        let json_str = r#""hello""#;
        let result: Result<ToolResultContent, _> = serde_json::from_str(json_str);
        assert!(
            result.is_ok(),
            "Plain string should deserialize as ToolResultContent::Text"
        );
        if let Ok(ToolResultContent::Text(s)) = result {
            assert_eq!(s, "hello");
        } else {
            panic!("Expected ToolResultContent::Text, got {:?}", result);
        }
    }

    #[test]
    fn tool_result_content_json_number_serde() {
        // Scenario 2: Deserialize number 42 → should be ToolResultContent::Json
        let json_str = r#"42"#;
        let result: Result<ToolResultContent, _> = serde_json::from_str(json_str);
        assert!(
            result.is_ok(),
            "Number should deserialize as ToolResultContent::Json"
        );
        if let Ok(ToolResultContent::Json(val)) = result {
            assert_eq!(val.as_i64(), Some(42));
        } else {
            panic!("Expected ToolResultContent::Json, got {:?}", result);
        }
    }

    #[test]
    fn tool_result_content_json_null_serde() {
        // Scenario 3: Deserialize null → should be ToolResultContent::Json(null)
        let json_str = "null";
        let result: Result<ToolResultContent, _> = serde_json::from_str(json_str);
        assert!(
            result.is_ok(),
            "null should deserialize as ToolResultContent::Json"
        );
        if let Ok(ToolResultContent::Json(val)) = result {
            assert!(val.is_null());
        } else {
            panic!("Expected ToolResultContent::Json, got {:?}", result);
        }
    }

    #[test]
    fn tool_result_content_multipart_array_serde() {
        // Scenario 4: Deserialize array ["hello"] → ambiguous: could be MultiPart or Json
        // With #[serde(untagged)], arrays should deserialize as MultiPart first
        let json_str = r#"[{"type":"text","text":"hello"}]"#;
        let result: Result<ToolResultContent, _> = serde_json::from_str(json_str);
        // This should succeed as MultiPart because the first untagged variant (Text) fails,
        // then MultiPart succeeds because it's Vec<ContentPart>
        assert!(
            result.is_ok(),
            "Array of ContentPart should deserialize as MultiPart"
        );
        match result {
            Ok(ToolResultContent::MultiPart(parts)) => {
                assert_eq!(parts.len(), 1);
            }
            Ok(ToolResultContent::Json(_)) => {
                // This is acceptable—if serde tries Json first and succeeds, ok.
            }
            _ => panic!("Unexpected deserialization result: {:?}", result),
        }
    }

    #[test]
    fn tool_result_content_json_object_serde() {
        // Scenario 5: Deserialize object {"key":"value"} → should be ToolResultContent::Json
        let json_str = r#"{"key":"value"}"#;
        let result: Result<ToolResultContent, _> = serde_json::from_str(json_str);
        assert!(
            result.is_ok(),
            "Object should deserialize as ToolResultContent::Json"
        );
        if let Ok(ToolResultContent::Json(val)) = result {
            assert_eq!(val.get("key").and_then(|v| v.as_str()), Some("value"));
        } else {
            panic!("Expected ToolResultContent::Json, got {:?}", result);
        }
    }
}
