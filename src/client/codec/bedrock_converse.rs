//! AWS Bedrock Converse API codec.
//!
//! Translates between the neutral [`crate::ir`] IR and the Bedrock
//! [Converse] wire format. Pinned to `BedrockTransport` because Converse
//! only ever travels over `bedrock-runtime.{region}.amazonaws.com` with
//! AWS SigV4 (or the new bearer token).
//!
//! [Converse]: https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Converse.html
//!
//! Streaming uses AWS EventStream binary framing
//! (`application/vnd.amazon.eventstream`). Each frame carries a
//! `:event-type` header that names the event kind
//! (`messageStart`, `contentBlockStart`, `contentBlockDelta`,
//! `contentBlockStop`, `messageStop`, `metadata`). The framing layer in
//! [`super::super::provider_client::ProviderClient::send_stream`] decodes
//! the binary frames and calls
//! [`super::ModelCodec::decode_eventstream_frame`] for each one, which
//! this codec dispatches on `event_type`.
//!
//! Bedrock hosts many model families (Anthropic, Amazon Nova, Meta Llama,
//! Mistral, Cohere, …) all through Converse. The codec is provider-neutral
//! across all of them; the few Anthropic-only knobs (`thinking`,
//! `cache_control`) are smuggled through `additionalModelRequestFields`
//! when the user supplies them via [`crate::ir::AnthropicOptions`].

use serde_json::{Value, json};

use super::{ApiVersionHint, EncodedRequest, EndpointShape, InvocationMode, ModelCodec};
use crate::ir::{
    CacheGranularity, CacheSupport, ContentPart, FinishReason, MediaSource, Message, ModelRequest,
    ModelResponse, ModelStreamChunk, ModelWarning, ProviderCapabilities, ReasoningSupport, Role,
    StreamDecodeState, StructuredOutputSupport, Support, SystemPromptShape, ToolCallSupport,
    ToolDefinition, ToolIdSemantics, ToolOrigin, ToolResultContent, Usage, VisionSupport,
};
use crate::{Error, Result};

const CODEC_ID: &str = "bedrock-converse";

const SHAPE: EndpointShape = EndpointShape {
    codec_id: CODEC_ID,
    // Bedrock URL is fully built by the transport because it includes the
    // model id in the path. The codec just declares the verb names so the
    // transport can switch between unary and streaming.
    path_template: "model/{model}/{verb}",
    verb_unary: "converse",
    verb_stream: "converse-stream",
    stream_query: &[],
    required_headers: &[],
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
    structured_output: StructuredOutputSupport {
        json_object: Support::Emulated,
        json_schema: Support::Emulated,
        strict: false,
    },
    vision: VisionSupport {
        images: Support::Native,
        pdfs: Support::Native,
        video: Support::Unsupported,
        accepts_url: false,
    },
    prompt_caching: CacheSupport {
        // Bedrock Converse supports prompt caching natively via inline
        // `cachePoint` blocks. The codec translates the IR's
        // `provider_options.anthropic.cache_control` flag set into
        // cachePoint blocks via `apply_cache_points`.
        mode: Support::Native,
        granularity: CacheGranularity::Conversation,
    },
    reasoning: ReasoningSupport {
        // Reasoning passthrough is model-specific (Anthropic Claude on
        // Bedrock supports thinking via additionalModelRequestFields).
        mode: Support::Emulated,
        exposes_text: true,
        exposes_tokens: true,
        requires_signature_passthrough: true,
    },
    system_prompt: SystemPromptShape::TopLevel,
    max_context_tokens: 200_000,
    count_tokens: Support::Unsupported,
    batch: Support::Unsupported,
};

/// Codec for the AWS Bedrock Converse API.
#[derive(Clone, Copy, Debug, Default)]
pub struct BedrockConverseCodec;

impl BedrockConverseCodec {
    pub const fn new() -> Self {
        Self
    }
}

impl ModelCodec for BedrockConverseCodec {
    fn id(&self) -> &'static str {
        CODEC_ID
    }

    fn capabilities(&self) -> &'static ProviderCapabilities {
        &CAPABILITIES
    }

    fn endpoint_shape(&self) -> &'static EndpointShape {
        &SHAPE
    }

    fn pinned_transport(&self) -> Option<&'static str> {
        Some("bedrock")
    }

    fn stream_framing(&self) -> crate::ir::StreamFraming {
        crate::ir::StreamFraming::AwsEventStream
    }

    fn encode_request(
        &self,
        request: &ModelRequest,
        _mode: InvocationMode,
    ) -> Result<EncodedRequest> {
        let mut warnings = Vec::new();
        let mut body = json!({
            "messages": encode_messages(&request.messages)?,
        });

        // System prompt → top-level array of `{text}` items.
        if let Some(sp) = &request.system {
            if sp.has_block_metadata() {
                warnings.push(ModelWarning::lossy(
                    "system.cache_marker",
                    "bedrock-converse does not preserve per-block cache markers",
                ));
            }
            body["system"] = json!([{"text": sp.flatten()}]);
        }

        // inferenceConfig.
        let mut ic = serde_json::Map::new();
        let s = &request.settings;
        if let Some(n) = s.max_output_tokens {
            ic.insert("maxTokens".into(), json!(n));
        }
        if let Some(t) = s.temperature {
            ic.insert("temperature".into(), json!(t));
        }
        if let Some(p) = s.top_p {
            ic.insert("topP".into(), json!(p));
        }
        if !s.stop_sequences.is_empty() {
            ic.insert("stopSequences".into(), json!(s.stop_sequences));
        }
        if !ic.is_empty() {
            body["inferenceConfig"] = Value::Object(ic);
        }
        if s.top_k.is_some() {
            warnings.push(ModelWarning::unsupported("top_k", CODEC_ID));
        }
        if s.presence_penalty.is_some() {
            warnings.push(ModelWarning::unsupported("presence_penalty", CODEC_ID));
        }
        if s.frequency_penalty.is_some() {
            warnings.push(ModelWarning::unsupported("frequency_penalty", CODEC_ID));
        }
        if s.seed.is_some() {
            warnings.push(ModelWarning::unsupported("seed", CODEC_ID));
        }

        // Tools.
        if !request.tools.is_empty() {
            body["toolConfig"] = json!({
                "tools": request
                    .tools
                    .iter()
                    .map(encode_tool_definition)
                    .collect::<Vec<_>>(),
            });
            if let Some(choice) = &request.tool_choice
                && let Some(tc_value) = encode_tool_choice(choice)
            {
                body["toolConfig"]["toolChoice"] = tc_value;
            }
        }

        // Structured output. Bedrock Converse delegates structured output
        // to the underlying model and does not expose a portable
        // `response_format` field — Anthropic models on Bedrock follow the
        // same tool-emulation pattern as the direct Anthropic API. Surface
        // a CapabilityEmulated warning so callers know the IR field is
        // honoured by emulation rather than a native parameter.
        if request.response_format.is_some() {
            warnings.push(ModelWarning::CapabilityEmulated {
                capability: "response_format".to_string(),
            });
        }

        // additionalModelRequestFields collects model-specific knobs that
        // Converse passes through to the underlying model.
        let mut additional = serde_json::Map::new();
        if let Some(reasoning) = &s.reasoning {
            // Anthropic Claude on Bedrock accepts `thinking` in
            // additionalModelRequestFields with the same shape as the
            // direct API.
            let budget = reasoning.budget_tokens.unwrap_or({
                match reasoning.effort {
                    Some(crate::ir::ReasoningEffort::Minimal) => 1024,
                    Some(crate::ir::ReasoningEffort::Low) => 4096,
                    Some(crate::ir::ReasoningEffort::Medium) => 16_384,
                    Some(crate::ir::ReasoningEffort::High) => 32_768,
                    None => 8192,
                }
            });
            additional.insert(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": budget}),
            );
        }
        if let Some(opts) = &request.provider_options.bedrock {
            if let Some(extra) = &opts.additional_model_request_fields
                && let Some(extra_obj) = extra.as_object()
            {
                for (k, v) in extra_obj {
                    additional.insert(k.clone(), v.clone());
                }
            }
            if let Some(g) = &opts.guardrail {
                let mut gc = json!({
                    "guardrailIdentifier": g.identifier,
                    "guardrailVersion": g.version,
                });
                if let Some(trace) = &g.trace {
                    gc["trace"] = json!(trace);
                }
                body["guardrailConfig"] = gc;
            }
            if let Some(latency) = &opts.latency {
                body["performanceConfig"] = json!({"latency": latency});
            }
        }
        if let Some(opts) = &request.provider_options.anthropic
            && !opts.beta_features.is_empty()
        {
            additional.insert("anthropic_beta".into(), json!(opts.beta_features));
        }
        if !additional.is_empty() {
            body["additionalModelRequestFields"] = Value::Object(additional);
        }

        // Bedrock Converse cachePoint translation. The Anthropic-style
        // `cache_control` flag set picks which positions get a cachePoint
        // block, mirroring the Anthropic Messages codec but using Converse's
        // inline-block syntax instead of `cache_control` annotations.
        if let Some(opts) = &request.provider_options.anthropic
            && let Some(cc) = &opts.cache_control
        {
            apply_cache_points(&mut body, cc);
        }

        warn_dropped_provider_options(&request.provider_options, &mut warnings);

        if request.continuation.is_some() {
            warnings.push(ModelWarning::lossy(
                "continuation",
                "bedrock-converse does not support stateful continuations",
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
            .get("ResponseMetadata")
            .and_then(|m| m.get("RequestId"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // Bedrock Converse does not echo the model in the body; the
        // transport could backfill it via the request URL but for now we
        // leave it empty.
        let model = String::new();

        let mut content = Vec::new();
        if let Some(blocks) = raw
            .get("output")
            .and_then(|o| o.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        {
            for block in blocks {
                if let Some(part) = decode_block(block) {
                    content.push(part);
                }
            }
        }

        let stop_raw = raw.get("stopReason").and_then(Value::as_str).unwrap_or("");
        let has_tool_calls = content
            .iter()
            .any(|p| matches!(p, ContentPart::ToolCall { .. }));
        let finish_reason = decode_stop_reason(stop_raw, has_tool_calls);
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
        _frame: &[u8],
        _state: &mut StreamDecodeState,
    ) -> Result<Vec<ModelStreamChunk>> {
        // Bedrock streaming uses AWS EventStream framing exclusively;
        // the SSE-style entry point should never be called.
        Err(Error::Parse(
            "bedrock-converse streams over AWS EventStream; use decode_eventstream_frame".into(),
        ))
    }

    fn decode_eventstream_frame(
        &self,
        event_type: &str,
        payload: &[u8],
        state: &mut StreamDecodeState,
    ) -> Result<Vec<ModelStreamChunk>> {
        state.bytes_seen += payload.len() as u64;
        state.frames_seen += 1;

        let value: Value = serde_json::from_slice(payload)
            .map_err(|e| Error::Parse(format!("bedrock eventstream payload not json: {e}")))?;

        let chunks = match event_type {
            "messageStart" => vec![ModelStreamChunk::MessageStart {
                id: String::new(),
                model: String::new(),
                role: Role::Assistant,
            }],
            "contentBlockStart" => decode_content_block_start_event(&value),
            "contentBlockDelta" => decode_content_block_delta_event(&value),
            "contentBlockStop" => {
                let index = value
                    .get("contentBlockIndex")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                vec![ModelStreamChunk::ToolCallEnd { index }]
            }
            "messageStop" => {
                let stop = value
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                vec![ModelStreamChunk::Finish {
                    reason: decode_stop_reason(stop, false),
                    usage: Usage::default(),
                }]
            }
            "metadata" => {
                if let Some(usage) = value.get("usage") {
                    vec![ModelStreamChunk::UsageDelta(crate::ir::PartialUsage {
                        input_tokens: usage.get("inputTokens").and_then(Value::as_u64),
                        output_tokens: usage.get("outputTokens").and_then(Value::as_u64),
                        cached_input_tokens: usage
                            .get("cacheReadInputTokens")
                            .and_then(Value::as_u64),
                        reasoning_tokens: None,
                    })]
                } else {
                    Vec::new()
                }
            }
            // ModelStreamErrorException, ValidationException, etc.
            "exception"
            | "modelStreamErrorException"
            | "internalServerException"
            | "throttlingException"
            | "validationException" => {
                let message = value
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                vec![ModelStreamChunk::Error {
                    kind: event_type.to_string(),
                    message,
                }]
            }
            _ => Vec::new(),
        };
        Ok(chunks)
    }
}

// =============================================================================
// Encoding helpers
// =============================================================================

fn encode_messages(messages: &[Message]) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let role = match m.role {
            Role::User | Role::Tool => "user",
            Role::Assistant => "assistant",
        };
        let content = m
            .content
            .iter()
            .filter_map(|p| encode_content_part(p).transpose())
            .collect::<Result<Vec<_>>>()?;
        if content.is_empty() {
            continue;
        }
        out.push(json!({"role": role, "content": content}));
    }
    Ok(out)
}

fn encode_content_part(part: &ContentPart) -> Result<Option<Value>> {
    Ok(match part {
        ContentPart::Text { text } => Some(json!({"text": text})),
        ContentPart::Image { source, mime } => {
            let format = mime_to_image_format(mime);
            match source {
                MediaSource::Base64 { data } => Some(json!({
                    "image": {
                        "format": format,
                        "source": {"bytes": data},
                    }
                })),
                MediaSource::Url { .. } | MediaSource::FileId { .. } => None,
            }
        }
        ContentPart::Document { source, mime } => {
            let format = mime_to_doc_format(mime);
            match source {
                MediaSource::Base64 { data } => Some(json!({
                    "document": {
                        "format": format,
                        "name": "document",
                        "source": {"bytes": data},
                    }
                })),
                MediaSource::Url { .. } | MediaSource::FileId { .. } => None,
            }
        }
        ContentPart::ToolCall {
            id,
            name,
            arguments,
            ..
        } => Some(json!({
            "toolUse": {
                "toolUseId": id,
                "name": name,
                "input": arguments,
            }
        })),
        ContentPart::ToolResult {
            tool_call_id,
            tool_name: _, // Bedrock Converse echoes only toolUseId
            content,
            is_error,
        } => {
            let content_arr = match content {
                ToolResultContent::Text(s) => json!([{"text": s}]),
                ToolResultContent::Json(v) => json!([{"json": v}]),
                ToolResultContent::MultiPart(parts) => {
                    let inner = parts
                        .iter()
                        .filter_map(|p| encode_content_part(p).ok().flatten())
                        .collect::<Vec<_>>();
                    Value::Array(inner)
                }
            };
            Some(json!({
                "toolResult": {
                    "toolUseId": tool_call_id,
                    "content": content_arr,
                    "status": if *is_error { "error" } else { "success" },
                }
            }))
        }
        ContentPart::Reasoning { .. }
        | ContentPart::Source { .. }
        | ContentPart::Unknown { .. } => None,
    })
}

fn mime_to_image_format(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    }
}

fn mime_to_doc_format(mime: &str) -> &'static str {
    match mime {
        "application/pdf" => "pdf",
        "text/csv" => "csv",
        "text/html" => "html",
        "text/markdown" => "md",
        "text/plain" => "txt",
        _ => "pdf",
    }
}

fn encode_tool_definition(tool: &ToolDefinition) -> Value {
    let mut spec = json!({
        "name": tool.name,
        "inputSchema": {"json": tool.parameters},
    });
    if let Some(desc) = &tool.description {
        spec["description"] = json!(desc);
    }
    json!({"toolSpec": spec})
}

fn encode_tool_choice(choice: &crate::ir::ToolChoice) -> Option<Value> {
    match choice {
        crate::ir::ToolChoice::Auto => Some(json!({"auto": {}})),
        crate::ir::ToolChoice::Required => Some(json!({"any": {}})),
        crate::ir::ToolChoice::Tool { name } => Some(json!({"tool": {"name": name}})),
        // Bedrock Converse has no explicit "none"; the caller can omit
        // the toolConfig instead.
        crate::ir::ToolChoice::None => None,
    }
}

/// Inject Bedrock Converse `cachePoint` blocks into the encoded body
/// based on the IR [`crate::ir::CacheControl`] flag set.
///
/// Mirrors the role of `apply_cache_control` in the Anthropic Messages
/// codec, but uses Converse's inline-block syntax (`{"cachePoint": {"type":
/// "default"}}`) inserted as a sibling of `text`/`image`/`toolResult`
/// content blocks instead of an annotation on a single block.
fn apply_cache_points(body: &mut Value, cc: &crate::ir::CacheControl) {
    let cp = json!({"cachePoint": {"type": "default"}});

    // System cache point — appended to the system array.
    if cc.system
        && let Some(system) = body.get_mut("system").and_then(Value::as_array_mut)
    {
        system.push(cp.clone());
    }

    // Tool catalogue cache point — appended to toolConfig.tools.
    if cc.tools
        && let Some(tools) = body
            .get_mut("toolConfig")
            .and_then(|tc| tc.get_mut("tools"))
            .and_then(Value::as_array_mut)
    {
        tools.push(cp.clone());
    }

    // Conversation cache point — appended to the last message's content
    // array. Bedrock interprets this as "cache the prefix up to here".
    if cc.conversation
        && let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut)
        && let Some(last) = messages.last_mut()
        && let Some(content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        content.push(cp);
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
}

// =============================================================================
// Decoding helpers
// =============================================================================

fn decode_block(block: &Value) -> Option<ContentPart> {
    if let Some(text) = block.get("text").and_then(Value::as_str) {
        return Some(ContentPart::Text { text: text.into() });
    }
    if let Some(tu) = block.get("toolUse") {
        let id = tu
            .get("toolUseId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = tu
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let arguments = tu.get("input").cloned().unwrap_or(Value::Null);
        return Some(ContentPart::ToolCall {
            id,
            name,
            arguments,
            origin: ToolOrigin::Local,
        });
    }
    if let Some(rc) = block.get("reasoningContent")
        && let Some(rt) = rc.get("reasoningText")
    {
        let text = rt
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let signature = rt
            .get("signature")
            .and_then(Value::as_str)
            .map(crate::ir::ReasoningSignature::new);
        return Some(ContentPart::Reasoning {
            content: crate::ir::ReasoningContent::Visible { text },
            kind: crate::ir::ReasoningKind::FullTrace,
            signature,
        });
    }
    None
}

fn decode_stop_reason(raw: &str, has_tool_calls: bool) -> FinishReason {
    match raw {
        "end_turn" => {
            if has_tool_calls {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            }
        }
        "tool_use" => FinishReason::ToolCalls,
        "max_tokens" => FinishReason::Length,
        "stop_sequence" => FinishReason::StopSequence,
        "content_filtered" => FinishReason::ContentFilter,
        "guardrail_intervened" => FinishReason::ContentFilter,
        "" => {
            if has_tool_calls {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            }
        }
        other => FinishReason::Other(other.to_string()),
    }
}

fn decode_usage(v: &Value) -> Usage {
    Usage {
        input_tokens: v.get("inputTokens").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: v.get("outputTokens").and_then(Value::as_u64).unwrap_or(0),
        cached_input_tokens: v.get("cacheReadInputTokens").and_then(Value::as_u64),
        cache_creation_tokens: v.get("cacheWriteInputTokens").and_then(Value::as_u64),
        reasoning_tokens: None,
        audio_input_tokens: None,
        audio_output_tokens: None,
        server_tool_invocations: None,
        raw: Some(v.clone()),
    }
}

fn decode_content_block_start_event(value: &Value) -> Vec<ModelStreamChunk> {
    let index = value
        .get("contentBlockIndex")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    if let Some(start) = value.get("start")
        && let Some(tu) = start.get("toolUse")
    {
        let id = tu
            .get("toolUseId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = tu
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return vec![ModelStreamChunk::ToolCallStart {
            index,
            id,
            name,
            origin: ToolOrigin::Local,
        }];
    }
    Vec::new()
}

fn decode_content_block_delta_event(value: &Value) -> Vec<ModelStreamChunk> {
    let index = value
        .get("contentBlockIndex")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let delta = match value.get("delta") {
        Some(d) => d,
        None => return Vec::new(),
    };
    if let Some(text) = delta.get("text").and_then(Value::as_str) {
        return vec![ModelStreamChunk::TextDelta {
            index,
            text: text.into(),
        }];
    }
    if let Some(tu) = delta.get("toolUse")
        && let Some(input) = tu.get("input").and_then(Value::as_str)
    {
        return vec![ModelStreamChunk::ToolCallArgsDelta {
            index,
            partial_json: input.into(),
        }];
    }
    if let Some(rc) = delta.get("reasoningContent") {
        if let Some(text) = rc.get("text").and_then(Value::as_str) {
            return vec![ModelStreamChunk::ReasoningDelta {
                index,
                text: text.into(),
            }];
        }
        if let Some(sig) = rc.get("signature").and_then(Value::as_str) {
            return vec![ModelStreamChunk::ReasoningSignature {
                index,
                signature: crate::ir::ReasoningSignature::new(sig),
            }];
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        AnthropicOptions, BedrockGuardrail, BedrockOptions, ModelSettings, ReasoningSettings,
    };

    fn req(messages: Vec<Message>) -> ModelRequest {
        ModelRequest::new("anthropic.claude-sonnet-4-5", messages)
    }

    #[test]
    fn id_capabilities_pin() {
        let c = BedrockConverseCodec::new();
        assert_eq!(c.id(), "bedrock-converse");
        assert_eq!(c.pinned_transport(), Some("bedrock"));
        assert_eq!(c.endpoint_shape().verb_unary, "converse");
        assert_eq!(c.endpoint_shape().verb_stream, "converse-stream");
        assert_eq!(c.stream_framing(), crate::ir::StreamFraming::AwsEventStream);
    }

    #[test]
    fn encode_basic_request() {
        let c = BedrockConverseCodec::new();
        let r = req(vec![Message::user("hello")]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["messages"][0]["role"], "user");
        assert_eq!(enc.body["messages"][0]["content"][0]["text"], "hello");
    }

    #[test]
    fn encode_system_top_level_array() {
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.system = Some("be brief".into());
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["system"][0]["text"], "be brief");
    }

    #[test]
    fn encode_inference_config_max_tokens() {
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings = ModelSettings::default()
            .with_max_output_tokens(512)
            .with_temperature(0.5);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["inferenceConfig"]["maxTokens"], 512);
        assert!((enc.body["inferenceConfig"]["temperature"].as_f64().unwrap() - 0.5).abs() < 1e-3);
    }

    #[test]
    fn encode_unsupported_settings_emit_warnings() {
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings.top_k = Some(40);
        r.settings.seed = Some(1);
        r.settings.frequency_penalty = Some(0.5);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let unsupported: Vec<_> = enc
            .warnings
            .iter()
            .filter_map(|w| match w {
                ModelWarning::UnsupportedSetting { setting, .. } => Some(setting.as_str()),
                _ => None,
            })
            .collect();
        assert!(unsupported.contains(&"top_k"));
        assert!(unsupported.contains(&"seed"));
        assert!(unsupported.contains(&"frequency_penalty"));
    }

    #[test]
    fn encode_tools_under_tool_config_with_input_schema_json() {
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.tools = vec![ToolDefinition::new("calc", json!({"type": "object"}))];
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let spec = &enc.body["toolConfig"]["tools"][0]["toolSpec"];
        assert_eq!(spec["name"], "calc");
        assert!(spec["inputSchema"]["json"].is_object());
    }

    #[test]
    fn encode_reasoning_into_additional_thinking() {
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings.reasoning = Some(ReasoningSettings {
            budget_tokens: Some(8192),
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(
            enc.body["additionalModelRequestFields"]["thinking"]["type"],
            "enabled"
        );
        assert_eq!(
            enc.body["additionalModelRequestFields"]["thinking"]["budget_tokens"],
            8192
        );
    }

    #[test]
    fn encode_bedrock_guardrail_and_latency() {
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.provider_options.bedrock = Some(BedrockOptions {
            guardrail: Some(BedrockGuardrail {
                identifier: "gid".into(),
                version: "1".into(),
                trace: Some("enabled".into()),
            }),
            latency: Some("optimized".into()),
            additional_model_request_fields: Some(json!({"foo": "bar"})),
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["guardrailConfig"]["guardrailIdentifier"], "gid");
        assert_eq!(enc.body["guardrailConfig"]["guardrailVersion"], "1");
        assert_eq!(enc.body["guardrailConfig"]["trace"], "enabled");
        assert_eq!(enc.body["performanceConfig"]["latency"], "optimized");
        assert_eq!(enc.body["additionalModelRequestFields"]["foo"], "bar");
    }

    #[test]
    fn encode_anthropic_beta_features_into_additional() {
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.provider_options.anthropic = Some(AnthropicOptions {
            beta_features: vec!["context-1m-2025-08-07".into()],
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(
            enc.body["additionalModelRequestFields"]["anthropic_beta"][0],
            "context-1m-2025-08-07"
        );
    }

    #[test]
    fn cache_control_only_system_emits_cache_point_in_system_only() {
        use crate::ir::{AnthropicOptions, CacheControl, SystemPrompt};
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hello")]);
        r.system = Some(SystemPrompt::from("you are helpful"));
        r.tools = vec![ToolDefinition::new("calc", json!({"type": "object"}))];
        r.provider_options.anthropic = Some(AnthropicOptions {
            cache_control: Some(CacheControl {
                system: true,
                tools: false,
                conversation: false,
                ttl: None,
            }),
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let system = enc.body["system"].as_array().unwrap();
        assert!(system.iter().any(|i| i.get("cachePoint").is_some()));
        let tools = enc.body["toolConfig"]["tools"].as_array().unwrap();
        assert!(!tools.iter().any(|i| i.get("cachePoint").is_some()));
        let last = enc.body["messages"].as_array().unwrap().last().unwrap();
        let content = last["content"].as_array().unwrap();
        assert!(!content.iter().any(|i| i.get("cachePoint").is_some()));
    }

    #[test]
    fn cache_control_only_tools_emits_cache_point_in_tools_only() {
        use crate::ir::{AnthropicOptions, CacheControl, SystemPrompt};
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hello")]);
        r.system = Some(SystemPrompt::from("you are helpful"));
        r.tools = vec![ToolDefinition::new("calc", json!({"type": "object"}))];
        r.provider_options.anthropic = Some(AnthropicOptions {
            cache_control: Some(CacheControl {
                system: false,
                tools: true,
                conversation: false,
                ttl: None,
            }),
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let system = enc.body["system"].as_array().unwrap();
        assert!(!system.iter().any(|i| i.get("cachePoint").is_some()));
        let tools = enc.body["toolConfig"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|i| i.get("cachePoint").is_some()));
    }

    #[test]
    fn cache_control_only_conversation_emits_cache_point_in_last_message_only() {
        use crate::ir::{AnthropicOptions, CacheControl, SystemPrompt};
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hello")]);
        r.system = Some(SystemPrompt::from("you are helpful"));
        r.tools = vec![ToolDefinition::new("calc", json!({"type": "object"}))];
        r.provider_options.anthropic = Some(AnthropicOptions {
            cache_control: Some(CacheControl {
                system: false,
                tools: false,
                conversation: true,
                ttl: None,
            }),
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let system = enc.body["system"].as_array().unwrap();
        assert!(!system.iter().any(|i| i.get("cachePoint").is_some()));
        let tools = enc.body["toolConfig"]["tools"].as_array().unwrap();
        assert!(!tools.iter().any(|i| i.get("cachePoint").is_some()));
        let last = enc.body["messages"].as_array().unwrap().last().unwrap();
        let content = last["content"].as_array().unwrap();
        assert!(content.iter().any(|i| i.get("cachePoint").is_some()));
    }

    #[test]
    fn cache_control_all_false_emits_no_cache_points() {
        use crate::ir::{AnthropicOptions, CacheControl, SystemPrompt};
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hello")]);
        r.system = Some(SystemPrompt::from("you are helpful"));
        r.tools = vec![ToolDefinition::new("calc", json!({"type": "object"}))];
        r.provider_options.anthropic = Some(AnthropicOptions {
            cache_control: Some(CacheControl::default()),
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        for path in [&enc.body["system"], &enc.body["toolConfig"]["tools"]] {
            if let Some(arr) = path.as_array() {
                assert!(!arr.iter().any(|i| i.get("cachePoint").is_some()));
            }
        }
    }

    #[test]
    fn cache_control_translates_to_inline_cache_points() {
        use crate::ir::{AnthropicOptions, CacheControl, SystemPrompt};
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hello")]);
        r.system = Some(SystemPrompt::from("you are helpful"));
        r.tools = vec![ToolDefinition::new("calc", json!({"type": "object"}))];
        r.provider_options.anthropic = Some(AnthropicOptions {
            cache_control: Some(CacheControl {
                system: true,
                tools: true,
                conversation: true,
                ttl: None,
            }),
            ..Default::default()
        });

        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();

        // System cache point appended to the system array.
        let system = enc.body["system"].as_array().unwrap();
        assert!(
            system
                .iter()
                .any(|item| item.get("cachePoint").is_some()),
            "system array missing cachePoint"
        );

        // Tools cache point appended to toolConfig.tools.
        let tools = enc.body["toolConfig"]["tools"].as_array().unwrap();
        assert!(
            tools.iter().any(|item| item.get("cachePoint").is_some()),
            "toolConfig.tools missing cachePoint"
        );

        // Conversation cache point appended to the last message's content.
        let last_msg = enc.body["messages"].as_array().unwrap().last().unwrap();
        let content = last_msg["content"].as_array().unwrap();
        assert!(
            content.iter().any(|item| item.get("cachePoint").is_some()),
            "last message content missing cachePoint"
        );
    }

    #[test]
    fn encode_drops_sibling_provider_options() {
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.provider_options.openai = Some(crate::ir::OpenAiOptions::default());
        r.provider_options.gemini = Some(crate::ir::GeminiOptions::default());
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let providers: Vec<_> = enc
            .warnings
            .iter()
            .filter_map(|w| match w {
                ModelWarning::DroppedProviderOption { provider, .. } => Some(provider.as_str()),
                _ => None,
            })
            .collect();
        assert!(providers.contains(&"openai"));
        assert!(providers.contains(&"gemini"));
    }

    #[test]
    fn response_format_emits_capability_emulated_warning() {
        // Bedrock Converse delegates structured output to the underlying
        // model. The codec surfaces the gap as a CapabilityEmulated warning
        // so the caller knows the request was honoured by emulation rather
        // than a native parameter.
        let c = BedrockConverseCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(crate::ir::ResponseFormat::JsonObject);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(
            enc.warnings.iter().any(|w| matches!(
                w,
                ModelWarning::CapabilityEmulated { capability } if capability == "response_format"
            )),
            "expected CapabilityEmulated warning, got {:?}",
            enc.warnings
        );
    }

    #[test]
    fn encode_tool_call_and_tool_result_round_trip() {
        let c = BedrockConverseCodec::new();
        let r = req(vec![
            Message::user("calc"),
            Message {
                role: Role::Assistant,
                content: vec![ContentPart::ToolCall {
                    id: "call_1".into(),
                    name: "calc".into(),
                    arguments: json!({"a": 1}),
                    origin: ToolOrigin::Local,
                }],
            },
            Message::tool_result("call_1", "2"),
        ]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let msgs = enc.body["messages"].as_array().unwrap();
        // Bedrock collapses tool result into a `user` role with toolResult content.
        let asst = msgs.iter().find(|m| m["role"] == "assistant").unwrap();
        assert_eq!(asst["content"][0]["toolUse"]["toolUseId"], "call_1");
        assert_eq!(asst["content"][0]["toolUse"]["name"], "calc");
        let last = msgs.last().unwrap();
        assert_eq!(last["role"], "user");
        assert_eq!(last["content"][0]["toolResult"]["toolUseId"], "call_1");
        assert_eq!(last["content"][0]["toolResult"]["status"], "success");
    }

    #[test]
    fn decode_response_text_and_usage() {
        let c = BedrockConverseCodec::new();
        let raw = json!({
            "output": {"message": {"role": "assistant", "content": [{"text": "hi"}]}},
            "stopReason": "end_turn",
            "usage": {"inputTokens": 5, "outputTokens": 1, "cacheReadInputTokens": 2}
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.text(), "hi");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.cached_input_tokens, Some(2));
    }

    #[test]
    fn decode_response_tool_use_finish_reason() {
        let c = BedrockConverseCodec::new();
        let raw = json!({
            "output": {"message": {"role": "assistant", "content": [
                {"toolUse": {"toolUseId": "call_1", "name": "calc", "input": {"a": 1}}}
            ]}},
            "stopReason": "tool_use",
            "usage": {"inputTokens": 5, "outputTokens": 5}
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        let tc = resp.tool_calls().next().unwrap();
        if let ContentPart::ToolCall { id, name, .. } = tc {
            assert_eq!(id, "call_1");
            assert_eq!(name, "calc");
        } else {
            panic!("expected ToolCall");
        }
    }

    #[test]
    fn decode_response_guardrail_finish_reason() {
        let c = BedrockConverseCodec::new();
        let raw = json!({
            "output": {"message": {"role": "assistant", "content": []}},
            "stopReason": "guardrail_intervened",
            "usage": {"inputTokens": 1, "outputTokens": 0}
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.finish_reason, FinishReason::ContentFilter);
    }

    #[test]
    fn decode_eventstream_message_start() {
        let c = BedrockConverseCodec::new();
        let mut s = StreamDecodeState::new();
        let chunks = c
            .decode_eventstream_frame("messageStart", b"{}", &mut s)
            .unwrap();
        assert!(matches!(
            chunks.first(),
            Some(ModelStreamChunk::MessageStart { .. })
        ));
    }

    #[test]
    fn decode_eventstream_content_block_delta_text() {
        let c = BedrockConverseCodec::new();
        let mut s = StreamDecodeState::new();
        let payload = br#"{"contentBlockIndex":0,"delta":{"text":"hi"}}"#;
        let chunks = c
            .decode_eventstream_frame("contentBlockDelta", payload, &mut s)
            .unwrap();
        match chunks.first() {
            Some(ModelStreamChunk::TextDelta { text, .. }) => assert_eq!(text, "hi"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn decode_eventstream_content_block_start_tool_use() {
        let c = BedrockConverseCodec::new();
        let mut s = StreamDecodeState::new();
        let payload =
            br#"{"contentBlockIndex":1,"start":{"toolUse":{"toolUseId":"call_1","name":"calc"}}}"#;
        let chunks = c
            .decode_eventstream_frame("contentBlockStart", payload, &mut s)
            .unwrap();
        assert!(matches!(
            chunks.first(),
            Some(ModelStreamChunk::ToolCallStart { id, name, .. }) if id == "call_1" && name == "calc"
        ));
    }

    #[test]
    fn decode_eventstream_content_block_delta_tool_use_args() {
        let c = BedrockConverseCodec::new();
        let mut s = StreamDecodeState::new();
        let payload = br#"{"contentBlockIndex":1,"delta":{"toolUse":{"input":"{\"a\":1}"}}}"#;
        let chunks = c
            .decode_eventstream_frame("contentBlockDelta", payload, &mut s)
            .unwrap();
        assert!(matches!(
            chunks.first(),
            Some(ModelStreamChunk::ToolCallArgsDelta { partial_json, .. }) if partial_json.contains("\"a\":1")
        ));
    }

    #[test]
    fn decode_eventstream_message_stop_emits_finish() {
        let c = BedrockConverseCodec::new();
        let mut s = StreamDecodeState::new();
        let chunks = c
            .decode_eventstream_frame("messageStop", br#"{"stopReason":"end_turn"}"#, &mut s)
            .unwrap();
        assert!(matches!(
            chunks.first(),
            Some(ModelStreamChunk::Finish {
                reason: FinishReason::Stop,
                ..
            })
        ));
    }

    #[test]
    fn decode_eventstream_metadata_usage_delta() {
        let c = BedrockConverseCodec::new();
        let mut s = StreamDecodeState::new();
        let payload = br#"{"usage":{"inputTokens":10,"outputTokens":5,"cacheReadInputTokens":2}}"#;
        let chunks = c
            .decode_eventstream_frame("metadata", payload, &mut s)
            .unwrap();
        let usage = chunks.iter().find_map(|c| match c {
            ModelStreamChunk::UsageDelta(u) => Some(u),
            _ => None,
        });
        let u = usage.expect("expected UsageDelta");
        assert_eq!(u.input_tokens, Some(10));
        assert_eq!(u.output_tokens, Some(5));
        assert_eq!(u.cached_input_tokens, Some(2));
    }

    #[test]
    fn decode_eventstream_exception_emits_error() {
        let c = BedrockConverseCodec::new();
        let mut s = StreamDecodeState::new();
        let payload = br#"{"message":"throttled"}"#;
        let chunks = c
            .decode_eventstream_frame("throttlingException", payload, &mut s)
            .unwrap();
        assert!(matches!(
            chunks.first(),
            Some(ModelStreamChunk::Error { kind, message }) if kind == "throttlingException" && message == "throttled"
        ));
    }
}
