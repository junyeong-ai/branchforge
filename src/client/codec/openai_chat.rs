//! OpenAI `/v1/chat/completions` codec.
//!
//! Translates between the neutral [`crate::ir`] IR and the OpenAI Chat
//! Completions wire format. Used by the `openai-chat` preset (legacy
//! OpenAI path) and any OpenAI-compatible third-party server (Grok,
//! DeepSeek, Mistral, OpenRouter, Together, Fireworks, …) by pointing a
//! `DirectTransport` at the alternate `base_url`.
//!
//! For the modern OpenAI primary endpoint (Responses API) see
//! [`super::openai_responses`].

#![allow(missing_docs)]

use serde_json::{Value, json};

use super::{ApiVersionHint, EncodedRequest, EndpointShape, InvocationMode, ModelCodec};
use crate::client::schema::{PreparedSchema, SchemaPolicy, prepare_schema, prepare_tool_schema};
use crate::ir::{
    CacheGranularity, CacheSupport, ContentPart, FinishReason, JsonSchemaSpec, MediaSource,
    Message, ModelRequest, ModelResponse, ModelStreamChunk, ModelWarning, ProviderCapabilities,
    ReasoningSupport, ResponseFormat, Role, StreamDecodeState, StructuredOutputSupport, Support,
    SystemPromptShape, ToolCallSupport, ToolDefinition, ToolIdSemantics, ToolOrigin,
    ToolResultContent, Usage, VisionSupport,
};
use crate::{Error, Result};

const SCHEMA_POLICY: SchemaPolicy = SchemaPolicy::openai_strict();

const CODEC_ID: &str = "openai-chat";

const SHAPE: EndpointShape = EndpointShape {
    codec_id: CODEC_ID,
    path_template: "v1/chat/completions",
    verb_unary: "",
    verb_stream: "",
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
        strict_schema: true,
        id_semantics: ToolIdSemantics::Provided,
    },
    structured_output: StructuredOutputSupport {
        json_object: Support::Native,
        json_schema: Support::Native,
        strict: true,
    },
    vision: VisionSupport {
        images: Support::Native,
        pdfs: Support::Unsupported,
        video: Support::Unsupported,
        accepts_url: true,
    },
    prompt_caching: CacheSupport {
        // OpenAI Chat Completions automatic prompt caching is transparent —
        // we get cached_tokens in usage but cannot control it. Mark Native
        // for the read side and let the cache_read_tokens decode populate.
        mode: Support::Native,
        granularity: CacheGranularity::Conversation,
    },
    reasoning: ReasoningSupport {
        // The Chat Completions API exposes a native `reasoning_effort`
        // wire field for the o-series (encoded directly at line ~212).
        // The response side cannot return reasoning text — only token
        // counts in `usage.completion_tokens_details.reasoning_tokens` —
        // which is captured separately by `exposes_text: false`. The
        // `mode` axis describes whether the wire path is native or
        // emulated, not whether responses round-trip the reasoning text.
        mode: Support::Native,
        exposes_text: false,
        exposes_tokens: true,
        requires_signature_passthrough: false,
    },
    system_prompt: SystemPromptShape::RoleMessage,
    max_context_tokens: 128_000,
    count_tokens: Support::Unsupported,
    batch: Support::Native,
};

/// Codec for the OpenAI Chat Completions API.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenAiChatCodec;

impl OpenAiChatCodec {
    pub const fn new() -> Self {
        Self
    }
}

impl ModelCodec for OpenAiChatCodec {
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

        // Build the messages array, prepending system as a role-message.
        let mut messages = Vec::new();
        if let Some(sp) = &request.system {
            if sp.has_block_metadata() {
                warnings.push(ModelWarning::lossy(
                    "system.cache_control",
                    "openai-chat does not support per-block cache_control",
                ));
            }
            messages.push(json!({"role": "system", "content": sp.flatten()}));
        }
        for m in &request.messages {
            for built in encode_message(m)? {
                messages.push(built);
            }
        }

        let mut body = json!({
            "model": request.model,
            "messages": messages,
        });

        if !request.tools.is_empty() {
            let mut tool_defs = Vec::with_capacity(request.tools.len());
            for tool in &request.tools {
                tool_defs.push(encode_tool_definition(tool, &mut warnings));
            }
            body["tools"] = json!(tool_defs);
        }
        if let Some(choice) = &request.tool_choice {
            body["tool_choice"] = encode_tool_choice(choice);
        }

        // Structured output. Chat Completions uses the legacy
        // `response_format` envelope. JSON-schema mode runs the schema
        // through the shared OpenAI strict-mode preparation so
        // unsupported keywords are stripped with lossy-encode warnings
        // instead of the silent drop the old codec performed.
        if let Some(format) = &request.response_format {
            encode_response_format(format, &mut body, &mut warnings);
        }

        let s = &request.settings;
        if let Some(n) = s.max_output_tokens {
            // Newer OpenAI spec uses max_completion_tokens; max_tokens is
            // deprecated but still works for compatibility servers.
            body["max_completion_tokens"] = json!(n);
        }
        if let Some(t) = s.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = s.top_p {
            body["top_p"] = json!(p);
        }
        if s.top_k.is_some() {
            warnings.push(ModelWarning::unsupported("top_k", CODEC_ID));
        }
        if let Some(pp) = s.presence_penalty {
            body["presence_penalty"] = json!(pp);
        }
        if let Some(fp) = s.frequency_penalty {
            body["frequency_penalty"] = json!(fp);
        }
        if !s.stop_sequences.is_empty() {
            body["stop"] = json!(s.stop_sequences);
        }
        if let Some(seed) = s.seed {
            body["seed"] = json!(seed);
        }
        if s.reasoning.is_some() {
            // Chat Completions o-series accepts reasoning_effort.
            if let Some(r) = &s.reasoning {
                let effort = r.effort.unwrap_or(crate::ir::ReasoningEffort::Medium);
                let s_effort = match effort {
                    crate::ir::ReasoningEffort::Minimal => "minimal",
                    crate::ir::ReasoningEffort::Low => "low",
                    crate::ir::ReasoningEffort::Medium => "medium",
                    crate::ir::ReasoningEffort::High => "high",
                };
                body["reasoning_effort"] = json!(s_effort);
            }
        }

        // Provider options.
        if let Some(opts) = &request.provider_options.openai {
            if let Some(parallel) = opts.parallel_tool_calls {
                body["parallel_tool_calls"] = json!(parallel);
            }
            if !opts.logit_bias.is_empty() {
                body["logit_bias"] = json!(opts.logit_bias);
            }
            if let Some(store) = opts.store {
                body["store"] = json!(store);
            }
            if !opts.metadata.is_empty() {
                body["metadata"] = json!(opts.metadata);
            }
            if let Some(effort) = opts.reasoning_effort {
                let s_effort = match effort {
                    crate::ir::ReasoningEffort::Minimal => "minimal",
                    crate::ir::ReasoningEffort::Low => "low",
                    crate::ir::ReasoningEffort::Medium => "medium",
                    crate::ir::ReasoningEffort::High => "high",
                };
                body["reasoning_effort"] = json!(s_effort);
            }
            if let Some(tier) = &opts.service_tier {
                body["service_tier"] = json!(tier);
            }
        }
        warn_dropped_provider_options(&request.provider_options, &mut warnings);

        // Streaming flag + auto-inject usage in stream chunks.
        if matches!(mode, InvocationMode::Stream) {
            body["stream"] = json!(true);
            body["stream_options"] = json!({"include_usage": true});
        }

        if request.continuation.is_some() {
            warnings.push(ModelWarning::lossy(
                "continuation",
                "openai-chat does not support stateful continuations",
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

        let choice = raw
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .ok_or_else(|| Error::Parse("openai response missing choices[0]".into()))?;
        let message = choice
            .get("message")
            .ok_or_else(|| Error::Parse("openai choice missing message".into()))?;

        let mut content = Vec::new();
        if let Some(text) = message.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            content.push(ContentPart::Text { text: text.into() });
        }
        if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
            for tc in tool_calls {
                let id = tc
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let func = tc.get("function");
                let name = func
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let arguments_str = func
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let arguments: Value = serde_json::from_str(arguments_str).unwrap_or(Value::Null);
                content.push(ContentPart::ToolCall {
                    id,
                    name,
                    arguments,
                    origin: ToolOrigin::Local,
                });
            }
        }

        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(decode_finish_reason)
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
            rate_limit: None,
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
            .map_err(|e| Error::Parse(format!("invalid utf-8 in openai stream frame: {e}")))?;
        let json_str = strip_sse_data_prefix(s).trim();
        if json_str.is_empty() || json_str == "[DONE]" {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(json_str)
            .map_err(|e| Error::Parse(format!("openai stream chunk not json: {e}")))?;

        let mut out = Vec::new();

        // First chunk: emit MessageStart with id + model.
        let is_first = state.frames_seen == 1;
        if is_first {
            let id = value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let model = value
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            out.push(ModelStreamChunk::MessageStart {
                id,
                model,
                role: Role::Assistant,
            });
        }

        // Process the choice delta.
        if let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        {
            if let Some(delta) = choice.get("delta") {
                if let Some(text) = delta.get("content").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    out.push(ModelStreamChunk::TextDelta {
                        index: 0,
                        text: text.into(),
                    });
                }
                if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for tc in tool_calls {
                        let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        // First fragment carries id + name; subsequent
                        // fragments carry only `function.arguments` chunks.
                        if let Some(id) = tc.get("id").and_then(Value::as_str) {
                            let name = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            out.push(ModelStreamChunk::ToolCallStart {
                                index,
                                id: id.to_string(),
                                name,
                                origin: ToolOrigin::Local,
                            });
                        }
                        if let Some(args) = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(Value::as_str)
                            && !args.is_empty()
                        {
                            out.push(ModelStreamChunk::ToolCallArgsDelta {
                                index,
                                partial_json: args.into(),
                            });
                        }
                    }
                }
            }

            if let Some(reason_raw) = choice.get("finish_reason").and_then(Value::as_str) {
                let reason = decode_finish_reason(reason_raw);
                let usage = value.get("usage").map(decode_usage).unwrap_or_default();
                out.push(ModelStreamChunk::Finish { reason, usage });
            }
        }

        // OpenAI emits a final standalone chunk with `usage` only when
        // `stream_options.include_usage = true`.
        if value
            .get("choices")
            .and_then(Value::as_array)
            .is_none_or(|a| a.is_empty())
            && let Some(usage_v) = value.get("usage")
        {
            let pu = crate::ir::PartialUsage {
                input_tokens: usage_v.get("prompt_tokens").and_then(Value::as_u64),
                output_tokens: usage_v.get("completion_tokens").and_then(Value::as_u64),
                cached_input_tokens: usage_v
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(Value::as_u64),
                reasoning_tokens: usage_v
                    .get("completion_tokens_details")
                    .and_then(|d| d.get("reasoning_tokens"))
                    .and_then(Value::as_u64),
            };
            out.push(ModelStreamChunk::UsageDelta(pu));
        }

        Ok(out)
    }
}

// =============================================================================
// Encoding helpers
// =============================================================================

fn encode_message(m: &Message) -> Result<Vec<Value>> {
    // OpenAI splits a single IR Message into:
    // - one assistant/user message with text content + tool_calls array
    // - one or more `role: "tool"` messages for tool results
    let mut out = Vec::new();
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => {
            // Each tool result becomes its own role:tool message.
            for part in &m.content {
                if let ContentPart::ToolResult {
                    tool_call_id,
                    content,
                    ..
                } = part
                {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": tool_result_text(content),
                    }));
                }
            }
            return Ok(out);
        }
    };

    let mut text_parts = Vec::new();
    let mut content_parts = Vec::new();
    let mut tool_calls = Vec::new();
    let mut has_media = false;
    for part in &m.content {
        match part {
            ContentPart::Text { text } => {
                text_parts.push(text.clone());
                content_parts.push(json!({"type": "text", "text": text}));
            }
            ContentPart::Image { source, mime: _ } => {
                has_media = true;
                let url = match source {
                    MediaSource::Base64 { data } => format!("data:image/png;base64,{data}"),
                    MediaSource::Url { url } => url.clone(),
                    MediaSource::FileId { id } => id.clone(),
                };
                content_parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
            }
            ContentPart::Document { .. } => {
                // OpenAI Chat does not natively accept PDFs in this shape;
                // emit as a placeholder text reference.
                content_parts.push(json!({"type": "text", "text": "[document attachment]"}));
            }
            ContentPart::Source { url, title, .. } => {
                let label = title.clone().unwrap_or_else(|| url.clone());
                let s = format!("[{label}]({url})");
                text_parts.push(s.clone());
                content_parts.push(json!({"type": "text", "text": s}));
            }
            ContentPart::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()},
                }));
            }
            ContentPart::ToolResult { .. } => {
                // Should not appear on a non-Tool role; skip.
            }
            ContentPart::Reasoning { .. } => {
                // OpenAI Chat does not accept reasoning content as input.
            }
            ContentPart::Unknown {
                codec_id, payload, ..
            } => {
                if codec_id == CODEC_ID
                    && let Some(arr) = payload.as_array()
                {
                    content_parts.extend(arr.iter().cloned());
                }
            }
        }
    }

    let mut msg = json!({"role": role});
    if has_media {
        msg["content"] = json!(content_parts);
    } else if !text_parts.is_empty() {
        msg["content"] = json!(text_parts.join("\n"));
    } else {
        msg["content"] = Value::Null;
    }
    if !tool_calls.is_empty() {
        msg["tool_calls"] = json!(tool_calls);
    }
    out.push(msg);
    Ok(out)
}

fn tool_result_text(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::Text(s) => s.clone(),
        ToolResultContent::Json(v) => v.to_string(),
        ToolResultContent::MultiPart(_) => "<multipart>".to_string(),
    }
}

fn encode_tool_definition(tool: &ToolDefinition, warnings: &mut Vec<ModelWarning>) -> Value {
    let prepared = prepare_tool_schema(
        tool.parameters.clone(),
        &SCHEMA_POLICY,
        tool.strict,
        &tool.name,
    );
    warnings.extend(prepared.warnings);

    let mut function = json!({
        "name": tool.name,
        "parameters": prepared.value,
    });
    if let Some(desc) = &tool.description {
        function["description"] = json!(desc);
    }
    if tool.strict {
        function["strict"] = json!(true);
    }
    json!({"type": "function", "function": function})
}

fn encode_tool_choice(choice: &crate::ir::ToolChoice) -> Value {
    match choice {
        crate::ir::ToolChoice::Auto => json!("auto"),
        crate::ir::ToolChoice::Required => json!("required"),
        crate::ir::ToolChoice::None => json!("none"),
        crate::ir::ToolChoice::Tool { name } => {
            json!({"type": "function", "function": {"name": name}})
        }
    }
}

fn warn_dropped_provider_options(
    opts: &crate::ir::ProviderOptions,
    warnings: &mut Vec<ModelWarning>,
) {
    if opts.anthropic.is_some() {
        warnings.push(ModelWarning::DroppedProviderOption {
            provider: "anthropic".into(),
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
}

fn encode_response_format(
    format: &ResponseFormat,
    body: &mut Value,
    warnings: &mut Vec<ModelWarning>,
) {
    match format {
        ResponseFormat::Text => {
            body["response_format"] = json!({"type": "text"});
        }
        ResponseFormat::JsonObject => {
            body["response_format"] = json!({"type": "json_object"});
        }
        ResponseFormat::JsonSchema(spec) => {
            body["response_format"] = encode_json_schema_envelope(spec, warnings);
        }
    }
}

fn encode_json_schema_envelope(spec: &JsonSchemaSpec, warnings: &mut Vec<ModelWarning>) -> Value {
    let prepared = if spec.strict {
        prepare_schema(
            spec.schema.clone(),
            &SCHEMA_POLICY,
            "response_format.schema",
        )
    } else {
        PreparedSchema::passthrough(spec.schema.clone())
    };
    warnings.extend(prepared.warnings);

    let mut json_schema = serde_json::Map::new();
    if let Some(name) = &spec.name {
        json_schema.insert("name".into(), json!(name));
    }
    if let Some(desc) = &spec.description {
        json_schema.insert("description".into(), json!(desc));
    }
    json_schema.insert("schema".into(), prepared.value);
    json_schema.insert("strict".into(), json!(spec.strict));
    json!({"type": "json_schema", "json_schema": Value::Object(json_schema)})
}

// =============================================================================
// Decoding helpers
// =============================================================================

fn decode_finish_reason(raw: &str) -> FinishReason {
    match raw {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.to_string()),
    }
}

fn decode_usage(v: &Value) -> Usage {
    Usage {
        input_tokens: v.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: v
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: v
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64),
        cache_creation_tokens: None,
        reasoning_tokens: v
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64),
        audio_input_tokens: v
            .get("prompt_tokens_details")
            .and_then(|d| d.get("audio_tokens"))
            .and_then(Value::as_u64),
        audio_output_tokens: v
            .get("completion_tokens_details")
            .and_then(|d| d.get("audio_tokens"))
            .and_then(Value::as_u64),
        server_tool_invocations: None,
        raw: Some(v.clone()),
    }
}

fn strip_sse_data_prefix(s: &str) -> &str {
    s.lines()
        .find_map(|line| {
            line.strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
        })
        .unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        JsonSchemaSpec, ModelSettings, OpenAiOptions, ReasoningEffort, ReasoningSettings,
    };

    fn req(messages: Vec<Message>) -> ModelRequest {
        ModelRequest::new("gpt-4o-mini", messages)
    }

    #[test]
    fn id_and_capabilities() {
        let c = OpenAiChatCodec::new();
        assert_eq!(c.id(), "openai-chat");
        assert_eq!(c.endpoint_shape().path_template, "v1/chat/completions");
        assert_eq!(
            c.capabilities().tool_calls.id_semantics,
            ToolIdSemantics::Provided
        );
        assert_eq!(
            c.capabilities().system_prompt,
            SystemPromptShape::RoleMessage
        );
    }

    #[test]
    fn encode_basic_request() {
        let c = OpenAiChatCodec::new();
        let r = req(vec![Message::user("hello")]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["model"], "gpt-4o-mini");
        assert_eq!(enc.body["messages"][0]["role"], "user");
        assert_eq!(enc.body["messages"][0]["content"], "hello");
        assert!(enc.body.get("stream").is_none());
    }

    #[test]
    fn encode_response_format_json_schema_uses_legacy_envelope() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema(
            JsonSchemaSpec::new(json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                }
            }))
            .with_name("Person")
            .with_strict(true),
        ));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["response_format"]["type"], "json_schema");
        assert_eq!(enc.body["response_format"]["json_schema"]["name"], "Person");
        assert_eq!(enc.body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(
            enc.body["response_format"]["json_schema"]["schema"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn encode_response_format_json_schema_emits_description_on_wire() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema(
            JsonSchemaSpec::new(json!({"type": "object"}))
                .with_name("Person")
                .with_description("A person record extracted from text"),
        ));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(
            enc.body["response_format"]["json_schema"]["description"],
            "A person record extracted from text"
        );
    }

    #[test]
    fn encode_response_format_json_schema_propagates_strip_warnings() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema(
            JsonSchemaSpec::new(json!({
                "type": "object",
                "properties": {
                    "age": {"type": "integer", "minimum": 0, "maximum": 120}
                }
            }))
            .with_name("Person"),
        ));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        // `minimum` and `maximum` are not allowed in OpenAI strict mode
        // and were previously silently stripped. Now they emit
        // LossyEncode warnings.
        let stripped: Vec<&str> = enc
            .warnings
            .iter()
            .filter_map(|w| match w {
                ModelWarning::LossyEncode { field, .. } => Some(field.as_str()),
                _ => None,
            })
            .collect();
        assert!(stripped.iter().any(|f| f.contains("minimum")));
        assert!(stripped.iter().any(|f| f.contains("maximum")));
    }

    #[test]
    fn encode_response_format_json_object() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonObject);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["response_format"]["type"], "json_object");
    }

    #[test]
    fn encode_system_prompt_as_role_message() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.system = Some("be brief".into());
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["messages"][0]["role"], "system");
        assert_eq!(enc.body["messages"][0]["content"], "be brief");
        assert_eq!(enc.body["messages"][1]["role"], "user");
    }

    #[test]
    fn encode_stream_includes_usage_options() {
        let c = OpenAiChatCodec::new();
        let r = req(vec![Message::user("hi")]);
        let enc = c.encode_request(&r, InvocationMode::Stream).unwrap();
        assert_eq!(enc.body["stream"], true);
        assert_eq!(enc.body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn encode_max_uses_max_completion_tokens() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings = ModelSettings::default().with_max_output_tokens(512);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["max_completion_tokens"], 512);
        assert!(enc.body.get("max_tokens").is_none());
    }

    #[test]
    fn encode_top_k_emits_unsupported_warning() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings.top_k = Some(40);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(enc.warnings.iter().any(
            |w| matches!(w, ModelWarning::UnsupportedSetting { setting, .. } if setting == "top_k")
        ));
    }

    #[test]
    fn encode_reasoning_effort_via_settings() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings.reasoning = Some(ReasoningSettings {
            effort: Some(ReasoningEffort::High),
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["reasoning_effort"], "high");
    }

    #[test]
    fn encode_provider_options_openai_overrides() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.provider_options.openai = Some(OpenAiOptions {
            parallel_tool_calls: Some(false),
            store: Some(true),
            reasoning_effort: Some(ReasoningEffort::Low),
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["parallel_tool_calls"], false);
        assert_eq!(enc.body["store"], true);
        assert_eq!(enc.body["reasoning_effort"], "low");
    }

    #[test]
    fn encode_drops_sibling_provider_options() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.provider_options.anthropic = Some(crate::ir::AnthropicOptions::default());
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
        assert!(providers.contains(&"anthropic"));
        assert!(providers.contains(&"gemini"));
    }

    #[test]
    fn encode_assistant_tool_call_then_tool_result() {
        let c = OpenAiChatCodec::new();
        let r = req(vec![
            Message::user("calc"),
            Message {
                role: Role::Assistant,
                content: vec![ContentPart::ToolCall {
                    id: "call_1".into(),
                    name: "calculator".into(),
                    arguments: json!({"a": 1}),
                    origin: ToolOrigin::Local,
                }],
            },
            Message::tool_result("call_1", "result=1"),
        ]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let msgs = enc.body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(msgs[1]["tool_calls"][0]["function"]["name"], "calculator");
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "call_1");
        assert_eq!(msgs[2]["content"], "result=1");
    }

    #[test]
    fn encode_image_part_as_image_url() {
        let c = OpenAiChatCodec::new();
        let r = req(vec![Message {
            role: Role::User,
            content: vec![
                ContentPart::Text {
                    text: "describe".into(),
                },
                ContentPart::Image {
                    source: MediaSource::Url {
                        url: "https://example.com/x.png".into(),
                    },
                    mime: "image/png".into(),
                },
            ],
        }]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let content = &enc.body["messages"][0]["content"];
        assert!(content.is_array());
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "https://example.com/x.png");
    }

    #[test]
    fn encode_tool_definition_strict_flag() {
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        let mut tool = ToolDefinition::new("calc", json!({"type": "object"}));
        tool.strict = true;
        r.tools = vec![tool];
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let func = &enc.body["tools"][0]["function"];
        assert_eq!(func["name"], "calc");
        assert_eq!(func["strict"], true);
    }

    #[test]
    fn encode_tool_definition_strict_applies_strict_policy_to_parameters() {
        // OpenAI strict tool use requires the strict JSON Schema subset
        // on tool parameters — same as response_format.json_schema.
        // schemars-generated schemas (with minimum: 0 etc.) must be
        // prepared by the shared pipeline.
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        let mut tool = ToolDefinition::new(
            "calc",
            json!({
                "type": "object",
                "properties": {
                    "a": {"type": "integer", "minimum": 0}
                }
            }),
        );
        tool.strict = true;
        r.tools = vec![tool];
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let params = &enc.body["tools"][0]["function"]["parameters"];
        // `minimum` is stripped because strict mode applied openai_strict policy.
        assert!(params["properties"]["a"].get("minimum").is_none());
        // additionalProperties: false was added.
        assert_eq!(params["additionalProperties"], false);
        // `a` was added to required (all-props auto-fill).
        assert!(
            params["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "a")
        );
    }

    #[test]
    fn encode_tool_definition_non_strict_preserves_user_constraints() {
        // Non-strict tools use lenient policy — user constraints survive.
        let c = OpenAiChatCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.tools = vec![ToolDefinition::new(
            "calc",
            json!({
                "type": "object",
                "properties": {
                    "a": {"type": "integer", "minimum": 0}
                }
            }),
        )];
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let params = &enc.body["tools"][0]["function"]["parameters"];
        assert_eq!(params["properties"]["a"]["minimum"], 0);
    }

    #[test]
    fn decode_response_with_text_and_usage() {
        let c = OpenAiChatCodec::new();
        let raw = json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi there"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 2,
                "prompt_tokens_details": {"cached_tokens": 3},
                "completion_tokens_details": {"reasoning_tokens": 100}
            }
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.id, "chatcmpl-1");
        assert_eq!(resp.text(), "hi there");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 2);
        assert_eq!(resp.usage.cached_input_tokens, Some(3));
        assert_eq!(resp.usage.reasoning_tokens, Some(100));
    }

    #[test]
    fn decode_response_with_tool_calls_parses_arguments_string() {
        let c = OpenAiChatCodec::new();
        let raw = json!({
            "id": "x",
            "model": "gpt-4o-mini",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "calc", "arguments": "{\"a\":1}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        let tc = resp.tool_calls().next().unwrap();
        if let ContentPart::ToolCall {
            id,
            name,
            arguments,
            ..
        } = tc
        {
            assert_eq!(id, "call_1");
            assert_eq!(name, "calc");
            assert_eq!(arguments["a"], 1);
        } else {
            panic!("expected ToolCall");
        }
    }

    #[test]
    fn decode_finish_reason_variants() {
        assert_eq!(decode_finish_reason("stop"), FinishReason::Stop);
        assert_eq!(decode_finish_reason("length"), FinishReason::Length);
        assert_eq!(decode_finish_reason("tool_calls"), FinishReason::ToolCalls);
        assert_eq!(
            decode_finish_reason("function_call"),
            FinishReason::ToolCalls
        );
        assert_eq!(
            decode_finish_reason("content_filter"),
            FinishReason::ContentFilter
        );
        assert_eq!(
            decode_finish_reason("weird"),
            FinishReason::Other("weird".into())
        );
    }

    #[test]
    fn decode_stream_text_delta() {
        let c = OpenAiChatCodec::new();
        let mut s = StreamDecodeState::new();
        let f = br#"{"id":"x","model":"gpt-4o","choices":[{"delta":{"content":"He"}}]}"#;
        let chunks = c.decode_stream_chunk(f, &mut s).unwrap();
        // First chunk emits MessageStart + TextDelta.
        assert!(matches!(
            chunks.first(),
            Some(ModelStreamChunk::MessageStart { .. })
        ));
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, ModelStreamChunk::TextDelta { text, .. } if text == "He"))
        );
    }

    #[test]
    fn decode_stream_tool_call_fragments() {
        let c = OpenAiChatCodec::new();
        let mut s = StreamDecodeState::new();
        // First fragment: tool_call start with id + name.
        let f1 = br#"{"id":"x","model":"gpt-4o","choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"calc","arguments":""}}]}}]}"#;
        let chunks1 = c.decode_stream_chunk(f1, &mut s).unwrap();
        assert!(chunks1.iter().any(|c| matches!(
            c,
            ModelStreamChunk::ToolCallStart { id, name, .. } if id == "call_1" && name == "calc"
        )));
        // Second fragment: more args.
        let f2 = br#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\":"}}]}}]}"#;
        let chunks2 = c.decode_stream_chunk(f2, &mut s).unwrap();
        assert!(chunks2.iter().any(|c| matches!(
            c,
            ModelStreamChunk::ToolCallArgsDelta { partial_json, .. } if partial_json == "{\"a\":"
        )));
        // Third fragment: end with finish_reason.
        let f3 = br#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
        let chunks3 = c.decode_stream_chunk(f3, &mut s).unwrap();
        assert!(chunks3.iter().any(|c| matches!(
            c,
            ModelStreamChunk::Finish {
                reason: FinishReason::ToolCalls,
                ..
            }
        )));
    }

    #[test]
    fn decode_stream_done_marker_skipped() {
        let c = OpenAiChatCodec::new();
        let mut s = StreamDecodeState::new();
        let chunks = c.decode_stream_chunk(b"[DONE]", &mut s).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn decode_stream_final_usage_chunk() {
        let c = OpenAiChatCodec::new();
        let mut s = StreamDecodeState::new();
        // Pretend a previous frame already advanced state.
        s.frames_seen = 5;
        let f = br#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":5},"completion_tokens_details":{"reasoning_tokens":50}}}"#;
        let chunks = c.decode_stream_chunk(f, &mut s).unwrap();
        let usage = chunks.iter().find_map(|c| match c {
            ModelStreamChunk::UsageDelta(u) => Some(u),
            _ => None,
        });
        let u = usage.expect("expected UsageDelta");
        assert_eq!(u.input_tokens, Some(10));
        assert_eq!(u.output_tokens, Some(20));
        assert_eq!(u.cached_input_tokens, Some(5));
        assert_eq!(u.reasoning_tokens, Some(50));
    }
}
