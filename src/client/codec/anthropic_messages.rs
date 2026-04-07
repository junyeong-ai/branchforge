//! Anthropic Messages API codec.
//!
//! Translates between the neutral [`crate::ir`] IR and the Anthropic
//! Messages API wire format (`POST /v1/messages`). Used by Anthropic
//! Direct, Vertex (`publishers/anthropic`), and Foundry transports.
//!
//! Bedrock does **not** use this codec — it uses its own
//! `BedrockConverseCodec` (the legacy InvokeModel envelope is gone).
//!
//! See plan §1 for the codec/transport split rationale and plan §2 for the
//! IR shape.

use serde_json::{Value, json};

use super::{
    ApiVersionHint, EncodedRequest, EndpointShape, HeaderSource, HeaderSpec, InvocationMode,
    ModelCodec,
};
use crate::ir::{
    CacheGranularity, CacheSupport, ContentPart, FinishReason, MediaSource, Message, ModelRequest,
    ModelResponse, ModelStreamChunk, ModelWarning, ProviderCapabilities, ReasoningContent,
    ReasoningKind, ReasoningSignature, ReasoningSupport, Role, StreamDecodeState, Support,
    SystemPrompt, SystemPromptShape, ToolCallSupport, ToolDefinition, ToolIdSemantics, ToolOrigin,
    ToolResultContent, Usage, VisionSupport,
};
use crate::{Error, Result};

const CODEC_ID: &str = "anthropic-messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

const SHAPE: EndpointShape = EndpointShape {
    path_template: "v1/messages",
    verb_unary: "",
    verb_stream: "",
    stream_query: &[],
    required_headers: &[HeaderSpec {
        name: "anthropic-version",
        source: HeaderSource::Literal(ANTHROPIC_VERSION),
    }],
    api_version_hint: ApiVersionHint::Stable,
};

const CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
    codec_id: CODEC_ID,
    streaming: Support::Native,
    tool_calls: ToolCallSupport {
        mode: Support::Native,
        parallel: Support::Native,
        strict_schema: false,
        id_semantics: ToolIdSemantics::Provided,
    },
    structured_output: crate::ir::StructuredOutputSupport {
        json_object: Support::Emulated,
        json_schema: Support::Emulated,
        strict: false,
    },
    vision: VisionSupport {
        images: Support::Native,
        pdfs: Support::Native,
        video: Support::Unsupported,
        accepts_url: true,
    },
    prompt_caching: CacheSupport {
        mode: Support::Native,
        granularity: CacheGranularity::Block,
    },
    reasoning: ReasoningSupport {
        mode: Support::Native,
        exposes_text: true,
        exposes_tokens: true,
        requires_signature_passthrough: true,
    },
    system_prompt: SystemPromptShape::TopLevel,
    max_context_tokens: 200_000,
    count_tokens: Support::Native,
    batch: Support::Native,
};

/// Codec for the Anthropic Messages API.
#[derive(Clone, Copy, Debug, Default)]
pub struct AnthropicMessagesCodec;

impl AnthropicMessagesCodec {
    pub const fn new() -> Self {
        Self
    }
}

impl ModelCodec for AnthropicMessagesCodec {
    fn id(&self) -> &'static str {
        CODEC_ID
    }

    fn capabilities(&self) -> &'static ProviderCapabilities {
        &CAPABILITIES
    }

    fn endpoint_shape(&self) -> &'static EndpointShape {
        &SHAPE
    }

    fn encode_request(
        &self,
        request: &ModelRequest,
        mode: InvocationMode,
    ) -> Result<EncodedRequest> {
        let mut warnings = Vec::new();
        let mut body = json!({
            "model": request.model,
            "messages": encode_messages(&request.messages)?,
            "max_tokens": request.settings.max_output_tokens.unwrap_or(8192),
        });

        // System prompt → top-level field.
        if let Some(system) = &request.system {
            body["system"] = encode_system_prompt(system);
        }

        // Tools → top-level array, with input_schema rename.
        if !request.tools.is_empty() {
            body["tools"] = json!(
                request
                    .tools
                    .iter()
                    .map(encode_tool_definition)
                    .collect::<Vec<_>>()
            );
        }

        if let Some(choice) = &request.tool_choice {
            body["tool_choice"] = encode_tool_choice(choice);
        }

        // Portable settings.
        let s = &request.settings;
        if let Some(t) = s.temperature {
            body["temperature"] = json!(clamp_temperature(t, &mut warnings));
        }
        if let Some(t) = s.top_p {
            body["top_p"] = json!(t);
        }
        if let Some(t) = s.top_k {
            body["top_k"] = json!(t);
        }
        if !s.stop_sequences.is_empty() {
            body["stop_sequences"] = json!(s.stop_sequences);
        }
        if s.seed.is_some() {
            warnings.push(ModelWarning::unsupported("seed", CODEC_ID));
        }
        if s.presence_penalty.is_some() {
            warnings.push(ModelWarning::unsupported("presence_penalty", CODEC_ID));
        }
        if s.frequency_penalty.is_some() {
            warnings.push(ModelWarning::unsupported("frequency_penalty", CODEC_ID));
        }
        if let Some(reasoning) = &s.reasoning {
            if let Some(budget) = reasoning.budget_tokens {
                body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
            } else if reasoning.effort.is_some() {
                // Anthropic does not have an "effort" knob. Map the
                // discrete level to a default budget bracket.
                let budget = match reasoning.effort {
                    Some(crate::ir::ReasoningEffort::Minimal) => 1024,
                    Some(crate::ir::ReasoningEffort::Low) => 4096,
                    Some(crate::ir::ReasoningEffort::Medium) => 16_384,
                    Some(crate::ir::ReasoningEffort::High) => 32_768,
                    None => 0,
                };
                body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
            }
        }

        // Streaming flag.
        if matches!(mode, InvocationMode::Stream) {
            body["stream"] = json!(true);
        }

        // ProviderOptions: only the `anthropic` field is honoured.
        if let Some(opts) = &request.provider_options.anthropic {
            // beta_features → header is the transport's job; codec ignores.
            // service_tier → top-level field.
            if let Some(tier) = &opts.service_tier {
                body["service_tier"] = json!(tier);
            }
            // cache_control influences how messages and system are encoded;
            // applied via apply_cache_control on the already-built body.
            if let Some(cc) = &opts.cache_control {
                apply_cache_control(&mut body, cc);
            }
        }
        warn_dropped_provider_options(&request.provider_options, &mut warnings);

        // Continuation: Anthropic does not support stateful continuations.
        if request.continuation.is_some() {
            warnings.push(ModelWarning::lossy(
                "continuation",
                "anthropic-messages does not support stateful continuations",
            ));
        }

        Ok(EncodedRequest { body, warnings })
    }

    fn decode_response(
        &self,
        raw: serde_json::Value,
        _mode: InvocationMode,
    ) -> Result<ModelResponse> {
        let id = raw
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let model = raw
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let content = raw
            .get("content")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(decode_content_block)
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();

        let finish_reason = raw
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(decode_stop_reason)
            .unwrap_or(FinishReason::Stop);

        let usage = raw.get("usage").map(decode_usage).unwrap_or_default();

        Ok(ModelResponse {
            id,
            model,
            content,
            finish_reason,
            usage,
            continuation: None,
            warnings: Vec::new(),
            raw: Some(raw),
        })
    }

    fn decode_stream_chunk(
        &self,
        frame: &[u8],
        state: &mut StreamDecodeState,
    ) -> Result<Vec<ModelStreamChunk>> {
        state.bytes_seen += frame.len() as u64;
        state.frames_seen += 1;

        let s = std::str::from_utf8(frame)
            .map_err(|e| Error::Parse(format!("invalid utf-8 in anthropic stream frame: {e}")))?;

        // Anthropic SSE: each event is a JSON object on a single `data:` line.
        // The streaming framing layer (StreamParser) is expected to hand us
        // the raw JSON payload after stripping `event:` / `data:` prefixes,
        // but to be robust we accept either form.
        let json_str = strip_sse_data_prefix(s).trim();
        if json_str.is_empty() {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(json_str)
            .map_err(|e| Error::Parse(format!("anthropic stream chunk not json: {e}")))?;

        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let chunks = match event_type {
            "message_start" => decode_message_start(&value),
            "content_block_start" => decode_content_block_start(&value),
            "content_block_delta" => decode_content_block_delta(&value),
            "content_block_stop" => decode_content_block_stop(&value),
            "message_delta" => decode_message_delta(&value),
            "message_stop" => vec![ModelStreamChunk::Finish {
                reason: FinishReason::Stop,
                usage: Usage::default(),
            }],
            "ping" => vec![ModelStreamChunk::Heartbeat],
            "error" => decode_error_event(&value),
            _ => Vec::new(),
        };
        Ok(chunks)
    }
}

// =============================================================================
// Encoding helpers
// =============================================================================

fn encode_messages(messages: &[Message]) -> Result<Value> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let role = match m.role {
            Role::User | Role::Tool => "user",
            Role::Assistant => "assistant",
        };
        let blocks = m
            .content
            .iter()
            .map(encode_content_part)
            .collect::<Result<Vec<_>>>()?;
        out.push(json!({"role": role, "content": blocks}));
    }
    Ok(Value::Array(out))
}

fn encode_content_part(part: &ContentPart) -> Result<Value> {
    Ok(match part {
        ContentPart::Text { text } => json!({"type": "text", "text": text}),
        ContentPart::Image { source, mime } => {
            json!({"type": "image", "source": encode_media_source(source, mime)})
        }
        ContentPart::Document { source, mime } => {
            json!({"type": "document", "source": encode_media_source(source, mime)})
        }
        ContentPart::Source { url, title, .. } => {
            // Anthropic represents citations as text + citations metadata.
            // Without context we just pass through as a text block carrying
            // a markdown link.
            let label = title.clone().unwrap_or_else(|| url.clone());
            json!({"type": "text", "text": format!("[{label}]({url})")})
        }
        ContentPart::ToolCall {
            id,
            name,
            arguments,
            origin: _,
        } => json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": arguments,
        }),
        ContentPart::ToolResult {
            tool_call_id,
            content,
            is_error,
        } => {
            let mut obj = json!({
                "type": "tool_result",
                "tool_use_id": tool_call_id,
                "content": encode_tool_result_content(content)?,
            });
            if *is_error {
                obj["is_error"] = json!(true);
            }
            obj
        }
        ContentPart::Reasoning {
            content,
            kind: _,
            signature,
        } => {
            let mut obj = match content {
                ReasoningContent::Visible { text } => json!({"type": "thinking", "thinking": text}),
                ReasoningContent::Redacted { data } => {
                    json!({"type": "redacted_thinking", "data": data})
                }
            };
            if let Some(sig) = signature {
                obj["signature"] = json!(sig.as_str());
            }
            obj
        }
        ContentPart::Unknown {
            codec_id, payload, ..
        } => {
            // Only round-trip if it came from us.
            if codec_id == CODEC_ID {
                payload.clone()
            } else {
                return Err(Error::InvalidRequest(format!(
                    "cannot encode Unknown part from codec {codec_id} through anthropic-messages",
                )));
            }
        }
    })
}

fn encode_media_source(source: &MediaSource, mime: &str) -> Value {
    match source {
        MediaSource::Base64 { data } => json!({
            "type": "base64",
            "media_type": mime,
            "data": data,
        }),
        MediaSource::Url { url } => json!({
            "type": "url",
            "url": url,
        }),
        MediaSource::FileId { id } => json!({
            "type": "file",
            "file_id": id,
        }),
    }
}

fn encode_tool_result_content(content: &ToolResultContent) -> Result<Value> {
    Ok(match content {
        ToolResultContent::Text(s) => json!([{"type": "text", "text": s}]),
        ToolResultContent::Json(v) => json!([{"type": "text", "text": v.to_string()}]),
        ToolResultContent::MultiPart(parts) => {
            let blocks = parts
                .iter()
                .map(encode_content_part)
                .collect::<Result<Vec<_>>>()?;
            Value::Array(blocks)
        }
    })
}

fn encode_system_prompt(sp: &SystemPrompt) -> Value {
    match sp {
        SystemPrompt::Text(s) => json!(s),
        SystemPrompt::Blocks(blocks) => json!(
            blocks
                .iter()
                .map(|b| {
                    let mut obj = json!({"type": "text", "text": b.text});
                    if let Some(_cc) = &b.cache_control {
                        obj["cache_control"] = json!({"type": "ephemeral"});
                    }
                    obj
                })
                .collect::<Vec<_>>()
        ),
    }
}

fn encode_tool_definition(tool: &ToolDefinition) -> Value {
    let mut obj = json!({
        "name": tool.name,
        "input_schema": tool.parameters,
    });
    if let Some(desc) = &tool.description {
        obj["description"] = json!(desc);
    }
    obj
}

fn encode_tool_choice(choice: &crate::ir::ToolChoice) -> Value {
    match choice {
        crate::ir::ToolChoice::Auto => json!({"type": "auto"}),
        crate::ir::ToolChoice::Required => json!({"type": "any"}),
        crate::ir::ToolChoice::None => json!({"type": "none"}),
        crate::ir::ToolChoice::Tool { name } => json!({"type": "tool", "name": name}),
    }
}

fn apply_cache_control(body: &mut Value, cc: &crate::ir::CacheControl) {
    use crate::ir::CacheControlMode;
    let marker = json!({"type": "ephemeral"});
    let target_messages = matches!(cc.mode, CacheControlMode::SystemAndConversation);
    let target_system = !matches!(cc.mode, CacheControlMode::Tools);
    let target_tools = matches!(cc.mode, CacheControlMode::Tools);

    if target_system
        && let Some(system) = body.get_mut("system")
        && let Some(arr) = system.as_array_mut()
        && let Some(last) = arr.last_mut()
        && let Some(obj) = last.as_object_mut()
    {
        obj.insert("cache_control".into(), marker.clone());
    }
    if target_tools
        && let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut)
        && let Some(last) = tools.last_mut()
        && let Some(obj) = last.as_object_mut()
    {
        obj.insert("cache_control".into(), marker.clone());
    }
    if target_messages
        && let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut)
        && let Some(last) = messages.last_mut()
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
        && let Some(last_block) = content.last_mut()
        && let Some(obj) = last_block.as_object_mut()
    {
        obj.insert("cache_control".into(), marker);
    }
}

fn clamp_temperature(t: f32, warnings: &mut Vec<ModelWarning>) -> f32 {
    if !(0.0..=1.0).contains(&t) {
        let clamped = t.clamp(0.0, 1.0);
        warnings.push(ModelWarning::clamped("temperature", t, clamped));
        clamped
    } else {
        t
    }
}

fn warn_dropped_provider_options(
    opts: &crate::ir::ProviderOptions,
    warnings: &mut Vec<ModelWarning>,
) {
    if opts.openai.is_some() {
        warnings.push(ModelWarning::DroppedProviderOption {
            provider: "openai".into(),
            option: "*".into(),
        });
    }
    if opts.gemini.is_some() {
        warnings.push(ModelWarning::DroppedProviderOption {
            provider: "gemini".into(),
            option: "*".into(),
        });
    }
    if opts.bedrock.is_some() {
        warnings.push(ModelWarning::DroppedProviderOption {
            provider: "bedrock".into(),
            option: "*".into(),
        });
    }
    // vertex options apply at the transport layer, not codec — no warning.
}

// =============================================================================
// Decoding helpers
// =============================================================================

fn decode_content_block(v: &Value) -> Result<ContentPart> {
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
    Ok(match ty {
        "text" => ContentPart::Text {
            text: v
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        "tool_use" => ContentPart::ToolCall {
            id: v
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            name: v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            arguments: v.get("input").cloned().unwrap_or(Value::Null),
            origin: ToolOrigin::Local,
        },
        "thinking" => ContentPart::Reasoning {
            content: ReasoningContent::Visible {
                text: v
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
            kind: ReasoningKind::FullTrace,
            signature: v
                .get("signature")
                .and_then(Value::as_str)
                .map(ReasoningSignature::new),
        },
        "redacted_thinking" => ContentPart::Reasoning {
            content: ReasoningContent::Redacted {
                data: v
                    .get("data")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
            kind: ReasoningKind::FullTrace,
            signature: None,
        },
        // Anything else (server tool use, web search results, etc.) lands
        // in `Unknown` and round-trips through this codec only.
        _ => ContentPart::Unknown {
            codec_id: CODEC_ID.to_string(),
            schema_version: 1,
            payload: v.clone(),
        },
    })
}

fn decode_stop_reason(s: &str) -> FinishReason {
    match s {
        "end_turn" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "stop_sequence" => FinishReason::StopSequence,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::ContentFilter,
        "pause_turn" => FinishReason::PauseTurn,
        other => FinishReason::Other(other.to_string()),
    }
}

fn decode_usage(v: &Value) -> Usage {
    Usage {
        input_tokens: v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: v.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        cached_input_tokens: v.get("cache_read_input_tokens").and_then(Value::as_u64),
        cache_creation_tokens: v.get("cache_creation_input_tokens").and_then(Value::as_u64),
        reasoning_tokens: None,
        audio_input_tokens: None,
        audio_output_tokens: None,
        server_tool_invocations: None,
        raw: Some(v.clone()),
    }
}

// =============================================================================
// Streaming helpers
// =============================================================================

fn strip_sse_data_prefix(s: &str) -> &str {
    s.lines()
        .find_map(|line| {
            line.strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
        })
        .unwrap_or(s)
}

fn decode_message_start(v: &Value) -> Vec<ModelStreamChunk> {
    let msg = match v.get("message") {
        Some(m) => m,
        None => return Vec::new(),
    };
    let id = msg
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model = msg
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    vec![ModelStreamChunk::MessageStart {
        id,
        model,
        role: Role::Assistant,
    }]
}

fn decode_content_block_start(v: &Value) -> Vec<ModelStreamChunk> {
    let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let block = match v.get("content_block") {
        Some(b) => b,
        None => return Vec::new(),
    };
    let ty = block.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "tool_use" => {
            let id = block
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![ModelStreamChunk::ToolCallStart {
                index,
                id,
                name,
                origin: ToolOrigin::Local,
            }]
        }
        // text and thinking blocks generate deltas via content_block_delta.
        _ => Vec::new(),
    }
}

fn decode_content_block_delta(v: &Value) -> Vec<ModelStreamChunk> {
    let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let delta = match v.get("delta") {
        Some(d) => d,
        None => return Vec::new(),
    };
    let ty = delta.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "text_delta" => {
            let text = delta
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![ModelStreamChunk::TextDelta { index, text }]
        }
        "input_json_delta" => {
            let partial = delta
                .get("partial_json")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![ModelStreamChunk::ToolCallArgsDelta {
                index,
                partial_json: partial,
            }]
        }
        "thinking_delta" => {
            let text = delta
                .get("thinking")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![ModelStreamChunk::ReasoningDelta { index, text }]
        }
        "signature_delta" => {
            let signature = delta
                .get("signature")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![ModelStreamChunk::ReasoningSignature {
                index,
                signature: ReasoningSignature::new(signature),
            }]
        }
        _ => Vec::new(),
    }
}

fn decode_content_block_stop(v: &Value) -> Vec<ModelStreamChunk> {
    let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    vec![ModelStreamChunk::ToolCallEnd { index }]
}

fn decode_message_delta(v: &Value) -> Vec<ModelStreamChunk> {
    let mut out = Vec::new();
    if let Some(usage) = v.get("usage") {
        let pu = crate::ir::PartialUsage {
            output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
            input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
            cached_input_tokens: usage.get("cache_read_input_tokens").and_then(Value::as_u64),
            reasoning_tokens: None,
        };
        out.push(ModelStreamChunk::UsageDelta(pu));
    }
    if let Some(delta) = v.get("delta")
        && let Some(stop) = delta.get("stop_reason").and_then(Value::as_str)
    {
        let reason = decode_stop_reason(stop);
        let usage = v.get("usage").map(decode_usage).unwrap_or_default();
        out.push(ModelStreamChunk::Finish { reason, usage });
    }
    out
}

fn decode_error_event(v: &Value) -> Vec<ModelStreamChunk> {
    let err = v.get("error").cloned().unwrap_or(Value::Null);
    vec![ModelStreamChunk::Error {
        kind: err
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_string(),
        message: err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        AnthropicOptions, CacheControl, CacheControlMode, Continuation, ModelSettings,
        ProviderOptions, ReasoningSettings,
    };

    fn req(messages: Vec<Message>) -> ModelRequest {
        ModelRequest::new("claude-sonnet-4-5", messages)
    }

    #[test]
    fn id_and_capabilities() {
        let c = AnthropicMessagesCodec::new();
        assert_eq!(c.id(), "anthropic-messages");
        assert_eq!(c.capabilities().codec_id, "anthropic-messages");
        assert!(c.capabilities().streaming.is_available());
        assert_eq!(
            c.capabilities().tool_calls.id_semantics,
            ToolIdSemantics::Provided
        );
        assert_eq!(c.endpoint_shape().path_template, "v1/messages");
    }

    #[test]
    fn encode_basic_request() {
        let c = AnthropicMessagesCodec::new();
        let r = req(vec![Message::user("hello")]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["model"], "claude-sonnet-4-5");
        assert_eq!(enc.body["messages"][0]["role"], "user");
        assert_eq!(enc.body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(enc.body["messages"][0]["content"][0]["text"], "hello");
        assert_eq!(enc.body["max_tokens"], 8192);
        assert!(enc.body.get("stream").is_none());
        assert!(enc.warnings.is_empty());
    }

    #[test]
    fn encode_stream_mode_sets_stream_true() {
        let c = AnthropicMessagesCodec::new();
        let r = req(vec![Message::user("hi")]);
        let enc = c.encode_request(&r, InvocationMode::Stream).unwrap();
        assert_eq!(enc.body["stream"], true);
    }

    #[test]
    fn encode_system_text() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.system = Some(SystemPrompt::Text("be brief".into()));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["system"], "be brief");
    }

    #[test]
    fn encode_tool_definition_renames_parameters_to_input_schema() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.tools = vec![ToolDefinition::new(
            "calc",
            json!({"type": "object", "properties": {"a": {"type": "number"}}}),
        )];
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["tools"][0]["name"], "calc");
        assert!(enc.body["tools"][0]["input_schema"].is_object());
        assert!(enc.body["tools"][0].get("parameters").is_none());
    }

    #[test]
    fn encode_seed_emits_unsupported_warning() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings = ModelSettings {
            seed: Some(42),
            ..Default::default()
        };
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(enc.body.get("seed").is_none());
        assert!(matches!(
            enc.warnings.first(),
            Some(ModelWarning::UnsupportedSetting { setting, .. }) if setting == "seed"
        ));
    }

    #[test]
    fn encode_temperature_clamped_emits_warning() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings = ModelSettings {
            temperature: Some(2.0),
            ..Default::default()
        };
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["temperature"].as_f64().unwrap(), 1.0);
        assert!(matches!(
            enc.warnings.first(),
            Some(ModelWarning::SettingClamped { setting, .. }) if setting == "temperature"
        ));
    }

    #[test]
    fn encode_reasoning_budget_maps_to_thinking() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings.reasoning = Some(ReasoningSettings {
            budget_tokens: Some(5000),
            effort: None,
            include_thoughts: true,
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["thinking"]["type"], "enabled");
        assert_eq!(enc.body["thinking"]["budget_tokens"], 5000);
    }

    #[test]
    fn encode_drops_sibling_provider_options_with_warning() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.provider_options = ProviderOptions {
            openai: Some(crate::ir::OpenAiOptions::default()),
            gemini: Some(crate::ir::GeminiOptions::default()),
            ..Default::default()
        };
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let dropped: Vec<_> = enc
            .warnings
            .iter()
            .filter_map(|w| match w {
                ModelWarning::DroppedProviderOption { provider, .. } => Some(provider.as_str()),
                _ => None,
            })
            .collect();
        assert!(dropped.contains(&"openai"));
        assert!(dropped.contains(&"gemini"));
    }

    #[test]
    fn encode_continuation_emits_lossy_warning() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.continuation = Some(Continuation::OpenAiResponses {
            previous_response_id: "resp_x".into(),
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(enc.warnings.iter().any(
            |w| matches!(w, ModelWarning::LossyEncode { field, .. } if field == "continuation")
        ));
    }

    #[test]
    fn encode_anthropic_cache_control_marks_system_block() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.system = Some(SystemPrompt::Blocks(vec![crate::ir::SystemBlock {
            text: "sys".into(),
            cache_control: None,
        }]));
        r.provider_options.anthropic = Some(AnthropicOptions {
            cache_control: Some(CacheControl {
                mode: CacheControlMode::System,
                ttl: None,
            }),
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["system"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn encode_tool_call_round_trip() {
        let c = AnthropicMessagesCodec::new();
        let r = req(vec![Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall {
                id: "call_1".into(),
                name: "calc".into(),
                arguments: json!({"a": 1}),
                origin: ToolOrigin::Local,
            }],
        }]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let block = &enc.body["messages"][0]["content"][0];
        assert_eq!(block["type"], "tool_use");
        assert_eq!(block["id"], "call_1");
        assert_eq!(block["name"], "calc");
        assert_eq!(block["input"]["a"], 1);
    }

    #[test]
    fn encode_tool_result_uses_tool_use_id_field() {
        let c = AnthropicMessagesCodec::new();
        let r = req(vec![Message::tool_result("call_1", "result")]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let block = &enc.body["messages"][0]["content"][0];
        assert_eq!(block["type"], "tool_result");
        assert_eq!(block["tool_use_id"], "call_1");
    }

    #[test]
    fn decode_response_text_and_usage() {
        let c = AnthropicMessagesCodec::new();
        let raw = json!({
            "id": "msg_1",
            "model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": "hi there"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 3}
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.id, "msg_1");
        assert_eq!(resp.text(), "hi there");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.output_tokens, 5);
        assert_eq!(resp.usage.cached_input_tokens, Some(3));
    }

    #[test]
    fn decode_tool_use_response() {
        let c = AnthropicMessagesCodec::new();
        let raw = json!({
            "id": "msg_2",
            "model": "claude-sonnet-4-5",
            "content": [
                {"type": "text", "text": "let me calculate"},
                {"type": "tool_use", "id": "call_1", "name": "calc", "input": {"a": 2}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 8, "output_tokens": 12}
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        assert_eq!(resp.tool_calls().count(), 1);
    }

    #[test]
    fn decode_unknown_block_lands_in_unknown_variant() {
        let c = AnthropicMessagesCodec::new();
        let raw = json!({
            "id": "msg_3",
            "model": "claude-sonnet-4-5",
            "content": [{"type": "server_tool_use", "name": "web_search", "id": "stu_1"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert!(matches!(
            resp.content.first(),
            Some(ContentPart::Unknown { codec_id, .. }) if codec_id == "anthropic-messages"
        ));
    }

    #[test]
    fn decode_stream_text_delta() {
        let c = AnthropicMessagesCodec::new();
        let mut state = StreamDecodeState::new();
        let frame = br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#;
        let chunks = c.decode_stream_chunk(frame, &mut state).unwrap();
        assert_eq!(chunks.len(), 1);
        match &chunks[0] {
            ModelStreamChunk::TextDelta { index, text } => {
                assert_eq!(*index, 0);
                assert_eq!(text, "Hi");
            }
            _ => panic!("expected TextDelta"),
        }
        assert_eq!(state.frames_seen, 1);
    }

    #[test]
    fn decode_stream_tool_call_args_delta() {
        let c = AnthropicMessagesCodec::new();
        let mut state = StreamDecodeState::new();
        let frame = br#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#;
        let chunks = c.decode_stream_chunk(frame, &mut state).unwrap();
        assert!(matches!(
            chunks.first(),
            Some(ModelStreamChunk::ToolCallArgsDelta { index, .. }) if *index == 1
        ));
    }

    #[test]
    fn decode_stream_message_start() {
        let c = AnthropicMessagesCodec::new();
        let mut state = StreamDecodeState::new();
        let frame =
            br#"{"type":"message_start","message":{"id":"msg_42","model":"claude-sonnet-4-5"}}"#;
        let chunks = c.decode_stream_chunk(frame, &mut state).unwrap();
        match chunks.first() {
            Some(ModelStreamChunk::MessageStart { id, model, role }) => {
                assert_eq!(id, "msg_42");
                assert_eq!(model, "claude-sonnet-4-5");
                assert_eq!(*role, Role::Assistant);
            }
            _ => panic!("expected MessageStart"),
        }
    }

    #[test]
    fn decode_stream_signature_delta() {
        let c = AnthropicMessagesCodec::new();
        let mut state = StreamDecodeState::new();
        let frame = br#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_xyz"}}"#;
        let chunks = c.decode_stream_chunk(frame, &mut state).unwrap();
        assert!(matches!(
            chunks.first(),
            Some(ModelStreamChunk::ReasoningSignature { signature, .. }) if signature.as_str() == "sig_xyz"
        ));
    }

    #[test]
    fn decode_stream_with_data_prefix() {
        let c = AnthropicMessagesCodec::new();
        let mut state = StreamDecodeState::new();
        let frame =
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}";
        let chunks = c.decode_stream_chunk(frame, &mut state).unwrap();
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn decode_stream_message_delta_with_finish() {
        let c = AnthropicMessagesCodec::new();
        let mut state = StreamDecodeState::new();
        let frame = br#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":10,"output_tokens":5}}"#;
        let chunks = c.decode_stream_chunk(frame, &mut state).unwrap();
        // Expect a UsageDelta and a Finish.
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, ModelStreamChunk::UsageDelta(_)))
        );
        assert!(chunks.iter().any(|c| matches!(
            c,
            ModelStreamChunk::Finish {
                reason: FinishReason::Stop,
                ..
            }
        )));
    }

    #[test]
    fn decode_stream_ping_is_heartbeat() {
        let c = AnthropicMessagesCodec::new();
        let mut state = StreamDecodeState::new();
        let chunks = c
            .decode_stream_chunk(br#"{"type":"ping"}"#, &mut state)
            .unwrap();
        assert!(matches!(chunks.first(), Some(ModelStreamChunk::Heartbeat)));
    }

    #[test]
    fn decode_stop_reason_variants() {
        assert_eq!(decode_stop_reason("end_turn"), FinishReason::Stop);
        assert_eq!(decode_stop_reason("tool_use"), FinishReason::ToolCalls);
        assert_eq!(decode_stop_reason("max_tokens"), FinishReason::Length);
        assert_eq!(decode_stop_reason("refusal"), FinishReason::ContentFilter);
        assert_eq!(decode_stop_reason("pause_turn"), FinishReason::PauseTurn);
        assert_eq!(
            decode_stop_reason("weird"),
            FinishReason::Other("weird".into())
        );
    }
}
