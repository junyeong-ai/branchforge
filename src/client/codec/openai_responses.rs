//! OpenAI `/v1/responses` codec.
//!
//! Translates between the neutral [`crate::ir`] IR and the OpenAI
//! Responses API wire format. This is the modern OpenAI primary endpoint
//! (GA 2025) and is the default for the `openai` preset; the legacy
//! `/v1/chat/completions` codec lives in [`super::openai_chat`] under the
//! `openai-chat` preset.
//!
//! Differences from Chat Completions that matter for the IR:
//!
//! - **Stateful**: `previous_response_id` lets the server keep prior
//!   conversation state. When set, the request only sends the delta turn.
//!   We surface this through [`crate::ir::Continuation::OpenAiResponses`].
//! - **Typed output items**: responses contain a list of `output[]` items
//!   of typed kinds: `message`, `reasoning`, `function_call`,
//!   `web_search_call`, `file_search_call`, `code_interpreter_call`,
//!   `computer_call`, `mcp_call`. We translate each to a `ContentPart`.
//! - **Encrypted reasoning passthrough**: `reasoning.encrypted_content`
//!   is opaque and **must round-trip back** in stateless mode. We surface
//!   this through [`ContentPart::Reasoning`] with [`ReasoningContent::Redacted`].
//! - **`instructions` field** for system prompt instead of role-message.
//! - **Different streaming taxonomy** with explicit
//!   `response.output_text.delta`, `response.function_call_arguments.delta`,
//!   `response.reasoning.delta`, etc. event names.

use serde_json::{Value, json};

use super::{ApiVersionHint, EncodedRequest, EndpointShape, InvocationMode, ModelCodec};
use crate::client::schema::transform_for_strict;
use crate::ir::{
    CacheGranularity, CacheSupport, ContentPart, Continuation, FinishReason, MediaSource, Message,
    ModelRequest, ModelResponse, ModelStreamChunk, ModelWarning, ProviderCapabilities,
    ReasoningContent, ReasoningKind, ReasoningSignature, ReasoningSupport, ResponseFormat, Role,
    StreamDecodeState, StructuredOutputSupport, Support, SystemPromptShape, ToolCallSupport,
    ToolDefinition, ToolIdSemantics, ToolOrigin, ToolResultContent, Usage, VisionSupport,
};
use crate::{Error, Result};

const CODEC_ID: &str = "openai-responses";

const SHAPE: EndpointShape = EndpointShape {
    codec_id: CODEC_ID,
    path_template: "v1/responses",
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
        pdfs: Support::Native,
        video: Support::Unsupported,
        accepts_url: true,
    },
    prompt_caching: CacheSupport {
        mode: Support::Native,
        granularity: CacheGranularity::Conversation,
    },
    reasoning: ReasoningSupport {
        mode: Support::Native,
        exposes_text: true,
        exposes_tokens: true,
        // Responses API uses encrypted_content for stateless reasoning
        // round-trip; the IR carries it as ReasoningContent::Redacted.
        requires_signature_passthrough: true,
    },
    system_prompt: SystemPromptShape::TopLevel,
    max_context_tokens: 200_000,
    count_tokens: Support::Unsupported,
    batch: Support::Native,
};

/// Codec for the OpenAI Responses API.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenAiResponsesCodec;

impl OpenAiResponsesCodec {
    pub const fn new() -> Self {
        Self
    }
}

impl ModelCodec for OpenAiResponsesCodec {
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
            "input": encode_input(&request.messages)?,
        });

        // System prompt → top-level `instructions`.
        if let Some(sp) = &request.system {
            if sp.has_block_metadata() {
                warnings.push(ModelWarning::lossy(
                    "system.cache_control",
                    "openai-responses does not support per-block cache_control",
                ));
            }
            body["instructions"] = json!(sp.flatten());
        }

        // Tools.
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

        // Structured output. The Responses API takes the schema under
        // `text.format` (not the legacy `response_format`). For
        // `JsonSchema` we run `transform_for_strict` so the schema satisfies
        // OpenAI's strict-mode validator (no missing `additionalProperties`
        // gates, all properties listed in `required`).
        if let Some(format) = &request.response_format {
            match format {
                ResponseFormat::Text => {
                    body["text"] = json!({"format": {"type": "text"}});
                }
                ResponseFormat::JsonObject => {
                    body["text"] = json!({"format": {"type": "json_object"}});
                }
                ResponseFormat::JsonSchema {
                    name,
                    schema,
                    strict,
                } => {
                    let prepared = if *strict {
                        transform_for_strict(schema.clone())
                    } else {
                        schema.clone()
                    };
                    body["text"] = json!({
                        "format": {
                            "type": "json_schema",
                            "name": name,
                            "schema": prepared,
                            "strict": strict,
                        }
                    });
                }
            }
        }

        // Settings.
        let s = &request.settings;
        if let Some(n) = s.max_output_tokens {
            body["max_output_tokens"] = json!(n);
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
        if s.presence_penalty.is_some() {
            warnings.push(ModelWarning::unsupported("presence_penalty", CODEC_ID));
        }
        if s.frequency_penalty.is_some() {
            warnings.push(ModelWarning::unsupported("frequency_penalty", CODEC_ID));
        }
        if !s.stop_sequences.is_empty() {
            warnings.push(ModelWarning::unsupported("stop_sequences", CODEC_ID));
        }
        if let Some(seed) = s.seed {
            body["seed"] = json!(seed);
        }
        if let Some(reasoning) = &s.reasoning {
            let mut robj = serde_json::Map::new();
            if let Some(effort) = reasoning.effort {
                let s_effort = match effort {
                    crate::ir::ReasoningEffort::Minimal => "minimal",
                    crate::ir::ReasoningEffort::Low => "low",
                    crate::ir::ReasoningEffort::Medium => "medium",
                    crate::ir::ReasoningEffort::High => "high",
                };
                robj.insert("effort".into(), json!(s_effort));
            }
            // Responses API does not accept budget_tokens directly; we map
            // budget_tokens to a coarse effort bracket if effort is unset.
            if reasoning.effort.is_none()
                && let Some(b) = reasoning.budget_tokens
            {
                let level = if b >= 16_384 {
                    "high"
                } else if b >= 4096 {
                    "medium"
                } else if b >= 1024 {
                    "low"
                } else {
                    "minimal"
                };
                robj.insert("effort".into(), json!(level));
            }
            if !robj.is_empty() {
                body["reasoning"] = Value::Object(robj);
            }
        }

        // Provider options.
        if let Some(opts) = &request.provider_options.openai {
            if let Some(parallel) = opts.parallel_tool_calls {
                body["parallel_tool_calls"] = json!(parallel);
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
                body.as_object_mut()
                    .and_then(|o| {
                        o.entry("reasoning")
                            .or_insert_with(|| json!({}))
                            .as_object_mut()
                    })
                    .map(|r| r.insert("effort".into(), json!(s_effort)));
            }
            if let Some(tier) = &opts.service_tier {
                body["service_tier"] = json!(tier);
            }
            if let Some(trunc) = &opts.truncation {
                body["truncation"] = json!(trunc);
            }
            if !opts.logit_bias.is_empty() {
                warnings.push(ModelWarning::unsupported("logit_bias", CODEC_ID));
            }
        }
        warn_dropped_provider_options(&request.provider_options, &mut warnings);

        // Stateful continuation.
        if let Some(Continuation::OpenAiResponses {
            previous_response_id,
        }) = &request.continuation
        {
            body["previous_response_id"] = json!(previous_response_id);
        }

        // Streaming flag.
        if matches!(mode, InvocationMode::Stream) {
            body["stream"] = json!(true);
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

        let output = raw
            .get("output")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut content = Vec::new();
        for item in &output {
            decode_output_item(item, &mut content);
        }

        // Responses API status / incomplete.reason mapping.
        let status = raw.get("status").and_then(Value::as_str).unwrap_or("");
        let incomplete_reason = raw
            .get("incomplete_details")
            .and_then(|d| d.get("reason"))
            .and_then(Value::as_str);
        let has_tool_calls = content
            .iter()
            .any(|p| matches!(p, ContentPart::ToolCall { .. }));
        let finish_reason = decode_finish(status, incomplete_reason, has_tool_calls);

        let usage = raw.get("usage").map(decode_usage).unwrap_or_default();
        let continuation = if !id.is_empty() {
            Some(Continuation::OpenAiResponses {
                previous_response_id: id.clone(),
            })
        } else {
            None
        };

        Ok(ModelResponse {
            id,
            model,
            content,
            finish_reason,
            usage,
            continuation,
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
            .map_err(|e| Error::Parse(format!("invalid utf-8 in openai stream frame: {e}")))?;
        let json_str = strip_sse_data_prefix(s).trim();
        if json_str.is_empty() || json_str == "[DONE]" {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(json_str)
            .map_err(|e| Error::Parse(format!("openai stream chunk not json: {e}")))?;

        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let mut out = Vec::new();
        match event_type {
            "response.created" => {
                if let Some(resp) = value.get("response") {
                    let id = resp
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let model = resp
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
            }
            "response.output_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    let index = value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize;
                    out.push(ModelStreamChunk::TextDelta {
                        index,
                        text: delta.into(),
                    });
                }
            }
            "response.reasoning.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    let index = value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize;
                    out.push(ModelStreamChunk::ReasoningDelta {
                        index,
                        text: delta.into(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    let index = value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize;
                    out.push(ModelStreamChunk::ToolCallArgsDelta {
                        index,
                        partial_json: delta.into(),
                    });
                }
            }
            "response.output_item.added" => {
                if let Some(item) = value.get("item") {
                    let index = value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize;
                    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                    if item_type == "function_call" {
                        let id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        out.push(ModelStreamChunk::ToolCallStart {
                            index,
                            id,
                            name,
                            origin: ToolOrigin::Local,
                        });
                    }
                }
            }
            "response.output_item.done" => {
                let index = value
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                if let Some(item) = value.get("item")
                    && item.get("type").and_then(Value::as_str) == Some("function_call")
                {
                    out.push(ModelStreamChunk::ToolCallEnd { index });
                }
            }
            "response.completed" => {
                if let Some(resp) = value.get("response") {
                    let usage = resp.get("usage").map(decode_usage).unwrap_or_default();
                    let status = resp.get("status").and_then(Value::as_str).unwrap_or("");
                    let incomplete = resp
                        .get("incomplete_details")
                        .and_then(|d| d.get("reason"))
                        .and_then(Value::as_str);
                    let has_tool_calls = resp
                        .get("output")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter().any(|i| {
                                i.get("type").and_then(Value::as_str) == Some("function_call")
                            })
                        })
                        .unwrap_or(false);
                    out.push(ModelStreamChunk::Finish {
                        reason: decode_finish(status, incomplete, has_tool_calls),
                        usage,
                    });
                }
            }
            "response.failed" | "response.error" | "error" => {
                let message = value
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .or_else(|| value.get("message").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string();
                let kind = value
                    .get("error")
                    .and_then(|e| e.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string();
                out.push(ModelStreamChunk::Error { kind, message });
            }
            _ => {}
        }
        Ok(out)
    }
}

// =============================================================================
// Encoding helpers
// =============================================================================

fn encode_input(messages: &[Message]) -> Result<Value> {
    // Responses API `input` accepts an array of typed items. We model each
    // IR Message as one or more input items.
    let mut out = Vec::new();
    for m in messages {
        match m.role {
            Role::User => {
                out.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": encode_input_content(&m.content, "input_text")?,
                }));
            }
            Role::Assistant => {
                // Split assistant content into text + tool calls + reasoning.
                let mut text_chunks = Vec::new();
                let mut function_calls = Vec::new();
                let mut reasoning_items = Vec::new();
                for part in &m.content {
                    match part {
                        ContentPart::Text { text } => text_chunks.push(text.clone()),
                        ContentPart::ToolCall {
                            id,
                            name,
                            arguments,
                            ..
                        } => {
                            function_calls.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": arguments.to_string(),
                            }));
                        }
                        ContentPart::Reasoning {
                            content, signature, ..
                        } => {
                            let mut item = json!({"type": "reasoning"});
                            match content {
                                ReasoningContent::Visible { text } => {
                                    item["summary"] =
                                        json!([{"type": "summary_text", "text": text}]);
                                }
                                ReasoningContent::Redacted { data } => {
                                    item["encrypted_content"] = json!(data);
                                }
                            }
                            if let Some(sig) = signature {
                                item["id"] = json!(sig.as_str());
                            }
                            reasoning_items.push(item);
                        }
                        _ => {}
                    }
                }
                out.extend(reasoning_items);
                if !text_chunks.is_empty() {
                    out.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text_chunks.join("\n")}],
                    }));
                }
                out.extend(function_calls);
            }
            Role::Tool => {
                for part in &m.content {
                    if let ContentPart::ToolResult {
                        tool_call_id,
                        content,
                        ..
                    } = part
                    {
                        out.push(json!({
                            "type": "function_call_output",
                            "call_id": tool_call_id,
                            "output": tool_result_text(content),
                        }));
                    }
                }
            }
        }
    }
    Ok(Value::Array(out))
}

fn encode_input_content(parts: &[ContentPart], text_type: &str) -> Result<Value> {
    let mut out = Vec::new();
    for p in parts {
        match p {
            ContentPart::Text { text } => {
                out.push(json!({"type": text_type, "text": text}));
            }
            ContentPart::Image { source, mime: _ } => {
                let url = match source {
                    MediaSource::Base64 { data } => format!("data:image/png;base64,{data}"),
                    MediaSource::Url { url } => url.clone(),
                    MediaSource::FileId { id } => id.clone(),
                };
                out.push(json!({"type": "input_image", "image_url": url}));
            }
            ContentPart::Document { source, .. } => {
                let url = match source {
                    MediaSource::Base64 { data } => format!("data:application/pdf;base64,{data}"),
                    MediaSource::Url { url } => url.clone(),
                    MediaSource::FileId { id } => id.clone(),
                };
                out.push(json!({"type": "input_file", "file_url": url}));
            }
            _ => {}
        }
    }
    Ok(Value::Array(out))
}

fn tool_result_text(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::Text(s) => s.clone(),
        ToolResultContent::Json(v) => v.to_string(),
        ToolResultContent::MultiPart(_) => "<multipart>".to_string(),
    }
}

fn encode_tool_definition(tool: &ToolDefinition) -> Value {
    let mut obj = json!({
        "type": "function",
        "name": tool.name,
        "parameters": tool.parameters,
    });
    if let Some(desc) = &tool.description {
        obj["description"] = json!(desc);
    }
    if tool.strict {
        obj["strict"] = json!(true);
    }
    obj
}

fn encode_tool_choice(choice: &crate::ir::ToolChoice) -> Value {
    match choice {
        crate::ir::ToolChoice::Auto => json!("auto"),
        crate::ir::ToolChoice::Required => json!("required"),
        crate::ir::ToolChoice::None => json!("none"),
        crate::ir::ToolChoice::Tool { name } => {
            json!({"type": "function", "name": name})
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

// =============================================================================
// Decoding helpers
// =============================================================================

fn decode_output_item(item: &Value, content: &mut Vec<ContentPart>) {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    match item_type {
        "message" => {
            if let Some(parts) = item.get("content").and_then(Value::as_array) {
                for p in parts {
                    let p_type = p.get("type").and_then(Value::as_str).unwrap_or("");
                    if (p_type == "output_text" || p_type == "text")
                        && let Some(text) = p.get("text").and_then(Value::as_str)
                    {
                        content.push(ContentPart::Text { text: text.into() });
                    }
                }
            }
        }
        "reasoning" => {
            // Visible summary text + optional encrypted_content for
            // stateless passthrough.
            if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                let mut buf = String::new();
                for s in summary {
                    if let Some(t) = s.get("text").and_then(Value::as_str) {
                        if !buf.is_empty() {
                            buf.push('\n');
                        }
                        buf.push_str(t);
                    }
                }
                if !buf.is_empty() {
                    content.push(ContentPart::Reasoning {
                        content: ReasoningContent::Visible { text: buf },
                        kind: ReasoningKind::Summary,
                        signature: item
                            .get("id")
                            .and_then(Value::as_str)
                            .map(ReasoningSignature::new),
                    });
                }
            }
            if let Some(enc) = item.get("encrypted_content").and_then(Value::as_str) {
                content.push(ContentPart::Reasoning {
                    content: ReasoningContent::Redacted { data: enc.into() },
                    kind: ReasoningKind::Summary,
                    signature: item
                        .get("id")
                        .and_then(Value::as_str)
                        .map(ReasoningSignature::new),
                });
            }
        }
        "function_call" => {
            let id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments_str = item
                .get("arguments")
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
        "web_search_call"
        | "file_search_call"
        | "code_interpreter_call"
        | "computer_call"
        | "mcp_call" => {
            // Builtin server-side tool invocations. Surface as
            // `BuiltinServer` tool calls so the agent runtime can route
            // them uniformly.
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let namespace = match item_type {
                "web_search_call" => "web_search",
                "file_search_call" => "file_search",
                "code_interpreter_call" => "code_interpreter",
                "computer_call" => "computer_use",
                "mcp_call" => "mcp",
                _ => "builtin",
            }
            .to_string();
            content.push(ContentPart::ToolCall {
                id,
                name: namespace.clone(),
                arguments: item.clone(),
                origin: ToolOrigin::BuiltinServer { namespace },
            });
        }
        _ => {
            content.push(ContentPart::Unknown {
                codec_id: CODEC_ID.to_string(),
                schema_version: 1,
                payload: item.clone(),
            });
        }
    }
}

fn decode_finish(status: &str, incomplete: Option<&str>, has_tool_calls: bool) -> FinishReason {
    if status == "completed" {
        if has_tool_calls {
            return FinishReason::ToolCalls;
        }
        return FinishReason::Stop;
    }
    if status == "incomplete" {
        return match incomplete {
            Some("max_output_tokens") => FinishReason::Length,
            Some("content_filter") => FinishReason::ContentFilter,
            Some("tool_call_loop") => FinishReason::PauseTurn,
            Some(other) => FinishReason::Other(other.to_string()),
            None => FinishReason::Length,
        };
    }
    if status == "failed" {
        return FinishReason::Error;
    }
    if status.is_empty() {
        if has_tool_calls {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        }
    } else {
        FinishReason::Other(status.to_string())
    }
}

fn decode_usage(v: &Value) -> Usage {
    Usage {
        input_tokens: v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: v.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        cached_input_tokens: v
            .get("input_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64),
        cache_creation_tokens: None,
        reasoning_tokens: v
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64),
        audio_input_tokens: None,
        audio_output_tokens: None,
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
    use crate::ir::{ModelSettings, OpenAiOptions, ReasoningEffort, ReasoningSettings};

    fn req(messages: Vec<Message>) -> ModelRequest {
        ModelRequest::new("gpt-4o-mini", messages)
    }

    #[test]
    fn id_and_capabilities() {
        let c = OpenAiResponsesCodec::new();
        assert_eq!(c.id(), "openai-responses");
        assert_eq!(c.endpoint_shape().path_template, "v1/responses");
        assert_eq!(c.capabilities().system_prompt, SystemPromptShape::TopLevel);
        assert!(c.capabilities().reasoning.requires_signature_passthrough);
    }

    #[test]
    fn encode_basic_request_uses_input_array() {
        let c = OpenAiResponsesCodec::new();
        let r = req(vec![Message::user("hello")]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["model"], "gpt-4o-mini");
        let item = &enc.body["input"][0];
        assert_eq!(item["type"], "message");
        assert_eq!(item["role"], "user");
        assert_eq!(item["content"][0]["type"], "input_text");
        assert_eq!(item["content"][0]["text"], "hello");
    }

    #[test]
    fn encode_json_schema_response_format_uses_text_format_envelope() {
        let c = OpenAiResponsesCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                }
            }),
            strict: true,
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["text"]["format"]["type"], "json_schema");
        assert_eq!(enc.body["text"]["format"]["name"], "Person");
        assert_eq!(enc.body["text"]["format"]["strict"], true);
        // The strict transform must populate `additionalProperties: false`
        // and auto-generate the `required` array because the input did not
        // declare one.
        assert_eq!(
            enc.body["text"]["format"]["schema"]["additionalProperties"],
            false
        );
        let req_arr = enc.body["text"]["format"]["schema"]["required"]
            .as_array()
            .unwrap();
        assert!(req_arr.iter().any(|v| v == "name"));
    }

    #[test]
    fn encode_json_object_response_format() {
        let c = OpenAiResponsesCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonObject);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["text"]["format"]["type"], "json_object");
    }

    #[test]
    fn encode_system_to_instructions() {
        let c = OpenAiResponsesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.system = Some("be brief".into());
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["instructions"], "be brief");
    }

    #[test]
    fn encode_max_output_tokens_field_name() {
        let c = OpenAiResponsesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings = ModelSettings::default().with_max_output_tokens(1024);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["max_output_tokens"], 1024);
    }

    #[test]
    fn encode_continuation_sets_previous_response_id() {
        let c = OpenAiResponsesCodec::new();
        let mut r = req(vec![Message::user("turn 2")]);
        r.continuation = Some(Continuation::OpenAiResponses {
            previous_response_id: "resp_123".into(),
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["previous_response_id"], "resp_123");
    }

    #[test]
    fn encode_reasoning_effort_top_level_object() {
        let c = OpenAiResponsesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings.reasoning = Some(ReasoningSettings {
            effort: Some(ReasoningEffort::High),
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["reasoning"]["effort"], "high");
    }

    #[test]
    fn encode_reasoning_budget_maps_to_effort_bracket() {
        let c = OpenAiResponsesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings.reasoning = Some(ReasoningSettings {
            budget_tokens: Some(20_000),
            effort: None,
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["reasoning"]["effort"], "high");
    }

    #[test]
    fn encode_assistant_tool_call_to_function_call_item() {
        let c = OpenAiResponsesCodec::new();
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
            Message::tool_result("call_1", "2"),
        ]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let items = enc.body["input"].as_array().unwrap();
        // Should have: user message, function_call item, function_call_output item.
        assert_eq!(items[0]["type"], "message");
        assert_eq!(items[0]["role"], "user");
        let fc = items.iter().find(|i| i["type"] == "function_call").unwrap();
        assert_eq!(fc["call_id"], "call_1");
        assert_eq!(fc["name"], "calculator");
        let fco = items
            .iter()
            .find(|i| i["type"] == "function_call_output")
            .unwrap();
        assert_eq!(fco["call_id"], "call_1");
        assert_eq!(fco["output"], "2");
    }

    #[test]
    fn encode_reasoning_passthrough_includes_encrypted_content() {
        let c = OpenAiResponsesCodec::new();
        let r = req(vec![Message {
            role: Role::Assistant,
            content: vec![ContentPart::Reasoning {
                content: ReasoningContent::Redacted {
                    data: "OPAQUE_BLOB".into(),
                },
                kind: ReasoningKind::Summary,
                signature: Some(ReasoningSignature::new("rs_42")),
            }],
        }]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let items = enc.body["input"].as_array().unwrap();
        let reasoning = items.iter().find(|i| i["type"] == "reasoning").unwrap();
        assert_eq!(reasoning["encrypted_content"], "OPAQUE_BLOB");
        assert_eq!(reasoning["id"], "rs_42");
    }

    #[test]
    fn encode_unsupported_settings_emit_warnings() {
        let c = OpenAiResponsesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings.top_k = Some(40);
        r.settings.frequency_penalty = Some(0.5);
        r.settings.stop_sequences = vec!["END".into()];
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
        assert!(unsupported.contains(&"frequency_penalty"));
        assert!(unsupported.contains(&"stop_sequences"));
    }

    #[test]
    fn encode_provider_options_openai_store_metadata() {
        let c = OpenAiResponsesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        let mut md = std::collections::BTreeMap::new();
        md.insert("session_id".to_string(), "abc".to_string());
        r.provider_options.openai = Some(OpenAiOptions {
            store: Some(true),
            metadata: md,
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["store"], true);
        assert_eq!(enc.body["metadata"]["session_id"], "abc");
    }

    #[test]
    fn decode_response_with_text_output_item() {
        let c = OpenAiResponsesCodec::new();
        let raw = json!({
            "id": "resp_1",
            "model": "gpt-4o-mini",
            "status": "completed",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hi"}]
            }],
            "usage": {
                "input_tokens": 5,
                "output_tokens": 1,
                "input_tokens_details": {"cached_tokens": 2},
                "output_tokens_details": {"reasoning_tokens": 0}
            }
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.id, "resp_1");
        assert_eq!(resp.text(), "hi");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.cached_input_tokens, Some(2));
        // Continuation populated from response id.
        assert!(matches!(
            resp.continuation,
            Some(Continuation::OpenAiResponses { previous_response_id }) if previous_response_id == "resp_1"
        ));
    }

    #[test]
    fn decode_response_function_call_item_becomes_tool_call() {
        let c = OpenAiResponsesCodec::new();
        let raw = json!({
            "id": "resp_2",
            "model": "gpt-4o-mini",
            "status": "completed",
            "output": [{
                "type": "function_call",
                "call_id": "call_1",
                "name": "calc",
                "arguments": "{\"a\":2}"
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
            assert_eq!(arguments["a"], 2);
        } else {
            panic!("expected ToolCall");
        }
    }

    #[test]
    fn decode_response_reasoning_with_encrypted_content() {
        let c = OpenAiResponsesCodec::new();
        let raw = json!({
            "id": "resp_3",
            "model": "o3-mini",
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [{"type": "summary_text", "text": "thought summary"}],
                    "encrypted_content": "ENCRYPTED"
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "answer"}]
                }
            ]
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        // Should produce a Visible reasoning + a Redacted reasoning + a Text.
        let kinds: Vec<_> = resp
            .content
            .iter()
            .map(|p| match p {
                ContentPart::Reasoning {
                    content: ReasoningContent::Visible { .. },
                    ..
                } => "visible",
                ContentPart::Reasoning {
                    content: ReasoningContent::Redacted { .. },
                    ..
                } => "redacted",
                ContentPart::Text { .. } => "text",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["visible", "redacted", "text"]);
    }

    #[test]
    fn decode_response_builtin_tool_calls_routed_through_tool_origin() {
        let c = OpenAiResponsesCodec::new();
        let raw = json!({
            "id": "resp_4",
            "model": "gpt-4o-mini",
            "status": "completed",
            "output": [{
                "type": "web_search_call",
                "id": "ws_1",
                "query": "rust"
            }]
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        let tc = resp.tool_calls().next().unwrap();
        if let ContentPart::ToolCall { origin, name, .. } = tc {
            assert_eq!(name, "web_search");
            assert!(matches!(
                origin,
                ToolOrigin::BuiltinServer { namespace } if namespace == "web_search"
            ));
        } else {
            panic!("expected ToolCall");
        }
    }

    #[test]
    fn decode_finish_incomplete_max_tokens() {
        assert_eq!(
            decode_finish("incomplete", Some("max_output_tokens"), false),
            FinishReason::Length
        );
        assert_eq!(
            decode_finish("incomplete", Some("content_filter"), false),
            FinishReason::ContentFilter
        );
        assert_eq!(
            decode_finish("incomplete", Some("tool_call_loop"), false),
            FinishReason::PauseTurn
        );
        assert_eq!(decode_finish("failed", None, false), FinishReason::Error);
    }

    #[test]
    fn decode_stream_response_created_emits_message_start() {
        let c = OpenAiResponsesCodec::new();
        let mut s = StreamDecodeState::new();
        let frame =
            br#"{"type":"response.created","response":{"id":"resp_42","model":"gpt-4o-mini"}}"#;
        let chunks = c.decode_stream_chunk(frame, &mut s).unwrap();
        match chunks.first() {
            Some(ModelStreamChunk::MessageStart { id, model, .. }) => {
                assert_eq!(id, "resp_42");
                assert_eq!(model, "gpt-4o-mini");
            }
            _ => panic!("expected MessageStart"),
        }
    }

    #[test]
    fn decode_stream_text_delta() {
        let c = OpenAiResponsesCodec::new();
        let mut s = StreamDecodeState::new();
        let frame = br#"{"type":"response.output_text.delta","output_index":0,"delta":"He"}"#;
        let chunks = c.decode_stream_chunk(frame, &mut s).unwrap();
        assert!(chunks.iter().any(|c| matches!(
            c,
            ModelStreamChunk::TextDelta { text, .. } if text == "He"
        )));
    }

    #[test]
    fn decode_stream_function_call_args_delta() {
        let c = OpenAiResponsesCodec::new();
        let mut s = StreamDecodeState::new();
        let frame = br#"{"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"a\":"}"#;
        let chunks = c.decode_stream_chunk(frame, &mut s).unwrap();
        assert!(chunks.iter().any(|c| matches!(
            c,
            ModelStreamChunk::ToolCallArgsDelta { partial_json, index } if partial_json == "{\"a\":" && *index == 1
        )));
    }

    #[test]
    fn decode_stream_output_item_added_function_call() {
        let c = OpenAiResponsesCodec::new();
        let mut s = StreamDecodeState::new();
        let frame = br#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"calc","arguments":""}}"#;
        let chunks = c.decode_stream_chunk(frame, &mut s).unwrap();
        assert!(chunks.iter().any(|c| matches!(
            c,
            ModelStreamChunk::ToolCallStart { id, name, .. } if id == "call_1" && name == "calc"
        )));
    }

    #[test]
    fn decode_stream_response_completed_emits_finish() {
        let c = OpenAiResponsesCodec::new();
        let mut s = StreamDecodeState::new();
        let frame = br#"{"type":"response.completed","response":{"status":"completed","output":[{"type":"message"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let chunks = c.decode_stream_chunk(frame, &mut s).unwrap();
        let finish = chunks.iter().find_map(|c| match c {
            ModelStreamChunk::Finish { reason, usage } => Some((reason.clone(), usage.clone())),
            _ => None,
        });
        let (reason, usage) = finish.expect("expected Finish");
        assert_eq!(reason, FinishReason::Stop);
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
    }

    #[test]
    fn decode_stream_reasoning_delta() {
        let c = OpenAiResponsesCodec::new();
        let mut s = StreamDecodeState::new();
        let frame =
            br#"{"type":"response.reasoning.delta","output_index":0,"delta":"thinking..."}"#;
        let chunks = c.decode_stream_chunk(frame, &mut s).unwrap();
        assert!(chunks.iter().any(|c| matches!(
            c,
            ModelStreamChunk::ReasoningDelta { text, .. } if text == "thinking..."
        )));
    }
}
