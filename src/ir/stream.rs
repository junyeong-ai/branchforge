//! Streaming response chunks.

use serde::{Deserialize, Serialize};

use super::content::{ContentPart, ReasoningSignature, ToolOrigin};
use super::finish::FinishReason;
use super::model::Role;
use super::usage::Usage;
use super::warning::ModelWarning;

/// Wire-level framing of a streaming response body.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamFraming {
    /// Standard Server-Sent Events (`data: {json}\n\n`). Used by Anthropic
    /// Direct, OpenAI Chat & Responses, Gemini with `alt=sse`, Vertex
    /// Anthropic, Foundry.
    Sse,
    /// AWS binary EventStream framing
    /// (`application/vnd.amazon.eventstream`). Used by Bedrock Converse.
    AwsEventStream,
    /// A streaming JSON array (`[{…},{…},…]`). Used by Gemini
    /// `:streamGenerateContent` without `alt=sse`.
    JsonArray,
    /// Newline-delimited JSON. Reserved for future providers; not currently
    /// used by any built-in codec.
    NdJson,
}

/// One chunk of a streaming response, after framing decode and codec
/// translation into the neutral IR.
///
/// A single input frame may produce zero, one, or many [`ModelStreamChunk`]
/// values: snapshot-based codecs (Gemini SSE) diff successive snapshots and
/// emit multiple deltas at once, while Anthropic-style delta events map
/// 1:1 to chunks.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelStreamChunk {
    /// First chunk of a stream, carrying the response id and model. Always
    /// emitted exactly once at the start.
    MessageStart {
        id: String,
        model: String,
        role: Role,
    },
    /// A text-content delta on the content part at `index`.
    TextDelta { index: usize, text: String },
    /// A reasoning-text delta.
    ReasoningDelta { index: usize, text: String },
    /// An opaque reasoning signature delivered separately from the
    /// reasoning text (Anthropic `signature_delta`).
    ReasoningSignature {
        index: usize,
        signature: ReasoningSignature,
    },
    /// A tool call begins. Subsequent [`Self::ToolCallArgsDelta`] events
    /// carry partial JSON for the arguments object.
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
        #[serde(default)]
        origin: ToolOrigin,
    },
    /// A fragment of the tool-call arguments JSON. Codecs concatenate
    /// fragments and parse on `ToolCallEnd`.
    ToolCallArgsDelta { index: usize, partial_json: String },
    /// A tool call is complete; arguments are now well-formed JSON.
    ToolCallEnd { index: usize },
    /// A server-side builtin tool emitted a typed event (web search
    /// progress, code interpreter output, etc.).
    BuiltinToolEvent {
        index: usize,
        namespace: String,
        payload: serde_json::Value,
    },
    /// A citation / grounding source landed in the response.
    Source(ContentPart),
    /// Intermediate usage update. Anthropic emits this on `message_delta`;
    /// OpenAI emits a single one near `[DONE]` when
    /// `stream_options.include_usage` is set (the OpenAI codecs always set
    /// it).
    UsageDelta(PartialUsage),
    /// Final chunk: the model has stopped, with the final usage attached.
    Finish { reason: FinishReason, usage: Usage },
    /// A non-fatal degradation surfaced during stream decoding.
    Warning(ModelWarning),
    /// A provider error surfaced via the stream rather than the HTTP layer.
    Error { kind: String, message: String },
    /// Heartbeat / ping. Filtered out by default by the high-level stream
    /// adapter; codecs may emit it for diagnostic completeness.
    Heartbeat,
    /// Phase C-6: snapshot of provider rate-limit accounting parsed
    /// from response headers. Emitted by `ProviderClient::send_stream`
    /// as the first chunk (before `MessageStart`) when the transport's
    /// `parse_rate_limit` returned `Some`. Consumers use it to drive
    /// typed observability events ahead of data arriving.
    RateLimit(super::rate_limit::RateLimitSnapshot),
}

/// Codec-private state carried across `decode_stream_chunk` calls.
///
/// Snapshot-based codecs (Gemini) keep accumulated state here so they can
/// compute deltas. Delta-based codecs (Anthropic, OpenAI) typically leave
/// this empty. The variant is owned by each codec; the IR module only
/// exposes the wrapper so consumers can pass an opaque handle through the
/// streaming machinery.
#[derive(Clone, Debug, Default)]
pub struct StreamDecodeState {
    /// Codec-private opaque payload. Codecs cast/downcast as needed.
    pub inner: Option<serde_json::Value>,
    /// Number of bytes seen so far on this stream — useful for diagnostic
    /// logging across all codecs.
    pub bytes_seen: u64,
    /// Number of frames decoded so far on this stream.
    pub frames_seen: u64,
}

impl StreamDecodeState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Partial usage update emitted mid-stream.
///
/// Fields are `Option<u64>` because providers report usage incrementally
/// and may report only the deltas they have available at that point.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

impl PartialUsage {
    /// Apply this partial update to a running [`Usage`] accumulator.
    pub fn apply(&self, usage: &mut Usage) {
        if let Some(v) = self.input_tokens {
            usage.input_tokens = v;
        }
        if let Some(v) = self.output_tokens {
            usage.output_tokens = v;
        }
        if let Some(v) = self.cached_input_tokens {
            usage.cached_input_tokens = Some(v);
        }
        if let Some(v) = self.reasoning_tokens {
            usage.reasoning_tokens = Some(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_delta_round_trips() {
        let c = ModelStreamChunk::TextDelta {
            index: 0,
            text: "hello".into(),
        };
        let j = serde_json::to_string(&c).unwrap();
        assert!(j.contains("\"type\":\"text_delta\""));
        let back: ModelStreamChunk = serde_json::from_str(&j).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn finish_chunk_round_trips() {
        let c = ModelStreamChunk::Finish {
            reason: FinishReason::Stop,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
        };
        let j = serde_json::to_string(&c).unwrap();
        let back: ModelStreamChunk = serde_json::from_str(&j).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn partial_usage_apply_overrides_running_total() {
        let mut u = Usage {
            input_tokens: 5,
            output_tokens: 0,
            ..Default::default()
        };
        let p = PartialUsage {
            output_tokens: Some(7),
            ..Default::default()
        };
        p.apply(&mut u);
        assert_eq!(u.input_tokens, 5);
        assert_eq!(u.output_tokens, 7);
    }

    #[test]
    fn stream_framing_snake_case() {
        assert_eq!(
            serde_json::to_string(&StreamFraming::AwsEventStream).unwrap(),
            "\"aws_event_stream\""
        );
    }

    #[test]
    fn stream_decode_state_default_is_empty() {
        let s = StreamDecodeState::new();
        assert!(s.inner.is_none());
        assert_eq!(s.bytes_seen, 0);
        assert_eq!(s.frames_seen, 0);
    }
}
