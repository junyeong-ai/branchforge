//! Gemini `generateContent` / `streamGenerateContent` codec.
//!
//! Translates between the neutral [`crate::ir`] IR and the Gemini
//! `generateContent` wire format. Used by both `gemini` (direct,
//! `generativelanguage.googleapis.com`) and `vertex-gemini` (Vertex,
//! `{region}-aiplatform.googleapis.com/.../publishers/google/...`)
//! presets — the codec is identical, only the transport differs.
//!
//! ## Streaming model
//!
//! Gemini's `:streamGenerateContent?alt=sse` does not emit deltas. Each
//! SSE event is a **complete `GenerateContentResponse` snapshot**. To fit
//! the neutral [`ModelStreamChunk`] delta model, this codec keeps a
//! per-stream snapshot in [`StreamDecodeState::inner`] and diffs the
//! incoming snapshot against it on every frame.
//!
//! ## Tool call ids
//!
//! Gemini `functionCall` parts have **no id** on the wire — only a name.
//! Parallel calls with duplicate function names cannot be disambiguated by
//! the wire format. The codec synthesizes ids by **positional index** and
//! advertises [`ToolIdSemantics::SynthesizedByIndex`] so the agent runtime
//! preserves part ordering as the disambiguator.

use serde_json::{Value, json};

use super::{ApiVersionHint, EncodedRequest, EndpointShape, InvocationMode, ModelCodec};
#[cfg(test)]
use crate::ir::SystemPrompt;
use crate::ir::{
    CacheGranularity, CacheSupport, ContentPart, FinishReason, MediaSource, Message, ModelRequest,
    ModelResponse, ModelStreamChunk, ModelWarning, ProviderCapabilities, ReasoningContent,
    ReasoningKind, ReasoningSupport, ResponseFormat, Role, StreamDecodeState,
    StructuredOutputSupport, Support, SystemPromptShape, ToolCallSupport, ToolDefinition,
    ToolIdSemantics, ToolOrigin, ToolResultContent, Usage, VisionSupport,
};
use crate::{Error, Result};

const CODEC_ID: &str = "gemini-generate";

const SHAPE: EndpointShape = EndpointShape {
    codec_id: CODEC_ID,
    path_template: "v1beta/models/{model}:{verb}",
    verb_unary: "generateContent",
    verb_stream: "streamGenerateContent",
    stream_query: &[("alt", "sse")],
    required_headers: &[],
    // `Beta` is interpreted per-transport. DirectTransport uses the literal
    // `v1beta` baked into `path_template` above (Gemini direct API);
    // VertexTransport translates `Beta` → `v1beta1` (the Vertex publisher
    // endpoint version, which uses the `1` suffix even though the direct API
    // does not). Without this, vertex-gemini hits 404 in regional endpoints.
    api_version_hint: ApiVersionHint::Beta,
};

const CAPABILITIES: ProviderCapabilities = ProviderCapabilities {
    codec_id: CODEC_ID,
    streaming: Support::Native,
    tool_calls: ToolCallSupport {
        mode: Support::Native,
        parallel: Support::Native,
        strict_schema: false,
        // Gemini wire format has no tool-call id; we synthesize from index.
        id_semantics: ToolIdSemantics::SynthesizedByIndex,
    },
    structured_output: StructuredOutputSupport {
        json_object: Support::Native,
        json_schema: Support::Native,
        strict: false,
    },
    vision: VisionSupport {
        images: Support::Native,
        pdfs: Support::Native,
        video: Support::Native,
        accepts_url: false,
    },
    prompt_caching: CacheSupport {
        // Gemini's `cachedContent` API is a separate resource lifecycle:
        // clients POST to `/cachedContents` to mint a cache handle, then
        // reference it by name in `generateContent.cachedContent`. This is
        // fundamentally different from Anthropic/OpenAI inline
        // `cache_control` markers and cannot be represented as a per-message
        // IR annotation. Supporting it would require a dedicated
        // cache-management surface on `ProviderClient`, which is
        // intentionally out of scope for the current IR. The capability is
        // reported as `Unsupported` so callers do not assume parity with
        // inline-cache providers.
        mode: Support::Unsupported,
        granularity: CacheGranularity::None,
    },
    reasoning: ReasoningSupport {
        mode: Support::Native,
        exposes_text: true,
        exposes_tokens: true,
        requires_signature_passthrough: false,
    },
    system_prompt: SystemPromptShape::TopLevel,
    max_context_tokens: 1_048_576,
    count_tokens: Support::Native,
    batch: Support::Unsupported,
};

/// Codec for the Gemini `generateContent` API.
#[derive(Clone, Copy, Debug, Default)]
pub struct GeminiGenerateCodec;

impl GeminiGenerateCodec {
    pub const fn new() -> Self {
        Self
    }
}

impl ModelCodec for GeminiGenerateCodec {
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
        _mode: InvocationMode,
    ) -> Result<EncodedRequest> {
        let mut warnings = Vec::new();

        // Gemini's `functionResponse` requires the function name. Surface a
        // warning for any ToolResult that arrives without a `tool_name` —
        // we still encode (with an empty placeholder) so the call doesn't
        // fail outright, but the operator can spot the misuse.
        for msg in &request.messages {
            for part in &msg.content {
                if let ContentPart::ToolResult {
                    tool_name: None, ..
                } = part
                {
                    warnings.push(ModelWarning::lossy(
                        "tool_result.tool_name",
                        "gemini-generate functionResponse requires a name; \
                         agent layer should populate ToolResult.tool_name from \
                         the matching ToolCall — encoding with empty name as fallback",
                    ));
                }
            }
        }

        let contents = encode_messages(&request.messages)?;
        let mut body = json!({"contents": contents});

        // System prompt → systemInstruction.
        if let Some(sp) = &request.system {
            if sp.has_block_metadata() {
                warnings.push(ModelWarning::lossy(
                    "system.cache_marker",
                    "gemini-generate does not support per-block cache markers",
                ));
            }
            body["systemInstruction"] = json!({
                "parts": [{"text": sp.flatten()}],
            });
        }

        // Tools.
        if !request.tools.is_empty() {
            body["tools"] = json!([{
                "functionDeclarations": request
                    .tools
                    .iter()
                    .map(encode_tool_definition)
                    .collect::<Vec<_>>(),
            }]);
        }
        if let Some(choice) = &request.tool_choice {
            body["toolConfig"] = encode_tool_choice(choice);
        }

        // generationConfig from settings.
        let mut gc = serde_json::Map::new();
        let s = &request.settings;
        if let Some(n) = s.max_output_tokens {
            gc.insert("maxOutputTokens".into(), json!(n));
        }
        if let Some(t) = s.temperature {
            gc.insert("temperature".into(), json!(t));
        }
        if let Some(p) = s.top_p {
            gc.insert("topP".into(), json!(p));
        }
        if let Some(k) = s.top_k {
            gc.insert("topK".into(), json!(k));
        }
        if !s.stop_sequences.is_empty() {
            gc.insert("stopSequences".into(), json!(s.stop_sequences));
        }
        if let Some(seed) = s.seed {
            gc.insert("seed".into(), json!(seed));
        }

        // Structured output. Gemini exposes this through `responseMimeType`
        // + (optionally) `responseSchema` on `generationConfig`. The schema
        // is OpenAPI-flavoured rather than strict JSON Schema, but Gemini
        // accepts the standard JSON-Schema subset we get from `schemars`.
        if let Some(format) = &request.response_format {
            match format {
                ResponseFormat::Text => {
                    gc.insert("responseMimeType".into(), json!("text/plain"));
                }
                ResponseFormat::JsonObject => {
                    gc.insert("responseMimeType".into(), json!("application/json"));
                }
                ResponseFormat::JsonSchema { schema, .. } => {
                    gc.insert("responseMimeType".into(), json!("application/json"));
                    gc.insert("responseSchema".into(), schema.clone());
                }
            }
        }
        if s.presence_penalty.is_some() {
            warnings.push(ModelWarning::unsupported("presence_penalty", CODEC_ID));
        }
        if s.frequency_penalty.is_some() {
            warnings.push(ModelWarning::unsupported("frequency_penalty", CODEC_ID));
        }
        if let Some(reasoning) = &s.reasoning {
            let mut tc = serde_json::Map::new();
            if let Some(b) = reasoning.budget_tokens {
                tc.insert("thinkingBudget".into(), json!(b));
            } else if let Some(effort) = reasoning.effort {
                let budget = match effort {
                    crate::ir::ReasoningEffort::Minimal => 1024,
                    crate::ir::ReasoningEffort::Low => 4096,
                    crate::ir::ReasoningEffort::Medium => 16_384,
                    crate::ir::ReasoningEffort::High => 32_768,
                };
                tc.insert("thinkingBudget".into(), json!(budget));
            }
            if reasoning.include_thoughts {
                tc.insert("includeThoughts".into(), json!(true));
            }
            if !tc.is_empty() {
                gc.insert("thinkingConfig".into(), Value::Object(tc));
            }
        }

        // Provider options: gemini-typed.
        if let Some(opts) = &request.provider_options.gemini {
            if let Some(mime) = &opts.response_mime_type {
                gc.insert("responseMimeType".into(), json!(mime));
            }
            if let Some(b) = opts.thinking_budget {
                let entry = gc
                    .entry("thinkingConfig")
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("thinkingBudget".into(), json!(b));
                }
            }
            if let Some(cached) = &opts.cached_content {
                body["cachedContent"] = json!(cached);
            }
            if !opts.safety_settings.is_empty() {
                body["safetySettings"] = json!(
                    opts.safety_settings
                        .iter()
                        .map(|ss| json!({"category": ss.category, "threshold": ss.threshold}))
                        .collect::<Vec<_>>()
                );
            }
        }
        warn_dropped_provider_options(&request.provider_options, &mut warnings);

        if !gc.is_empty() {
            body["generationConfig"] = Value::Object(gc);
        }

        // Continuation: Gemini does not support stateful continuations.
        if request.continuation.is_some() {
            warnings.push(ModelWarning::lossy(
                "continuation",
                "gemini-generate does not support stateful continuations",
            ));
        }

        Ok(EncodedRequest { body, warnings })
    }

    fn decode_response(
        &self,
        raw: serde_json::Value,
        _mode: InvocationMode,
    ) -> Result<ModelResponse> {
        let model = raw
            .get("modelVersion")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = raw
            .get("responseId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let candidate = raw
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .ok_or_else(|| Error::Parse("gemini response missing candidates[0]".into()))?;

        let parts = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let (content, has_tool_calls) = decode_parts(&parts);

        let finish_reason_raw = candidate
            .get("finishReason")
            .and_then(Value::as_str)
            .unwrap_or("STOP");
        let finish_reason = decode_finish_reason(finish_reason_raw, has_tool_calls);

        let usage = raw
            .get("usageMetadata")
            .map(decode_usage)
            .unwrap_or_default();

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
            .map_err(|e| Error::Parse(format!("invalid utf-8 in gemini stream frame: {e}")))?;
        let json_str = strip_sse_data_prefix(s).trim();
        if json_str.is_empty() {
            return Ok(Vec::new());
        }
        let snapshot: Value = serde_json::from_str(json_str)
            .map_err(|e| Error::Parse(format!("gemini stream chunk not json: {e}")))?;

        let mut out = Vec::new();

        // First-frame: emit MessageStart.
        let is_first = state.inner.is_none();
        if is_first {
            let id = snapshot
                .get("responseId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let model = snapshot
                .get("modelVersion")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            out.push(ModelStreamChunk::MessageStart {
                id,
                model,
                role: Role::Assistant,
            });
        }

        // Diff text against accumulated snapshot.
        let new_text = extract_concatenated_text(&snapshot);
        let prev_text: String = state
            .inner
            .as_ref()
            .map(extract_concatenated_text)
            .unwrap_or_default();
        if new_text.len() > prev_text.len() && new_text.starts_with(&prev_text) {
            let delta = new_text[prev_text.len()..].to_string();
            if !delta.is_empty() {
                out.push(ModelStreamChunk::TextDelta {
                    index: 0,
                    text: delta,
                });
            }
        } else if new_text != prev_text && !new_text.is_empty() {
            // Snapshots disagree on prefix — best-effort: emit the new text wholesale.
            out.push(ModelStreamChunk::TextDelta {
                index: 0,
                text: new_text.clone(),
            });
        }

        // Tool calls — emit on first appearance only.
        let prev_tool_count = state.inner.as_ref().map(count_function_calls).unwrap_or(0);
        let new_tool_count = count_function_calls(&snapshot);
        if new_tool_count > prev_tool_count {
            for (idx, fc) in collect_function_calls(&snapshot)
                .into_iter()
                .enumerate()
                .skip(prev_tool_count)
            {
                let name = fc
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let args = fc.get("args").cloned().unwrap_or(Value::Null);
                let synth_id = format!("call_{idx}");
                out.push(ModelStreamChunk::ToolCallStart {
                    index: idx,
                    id: synth_id.clone(),
                    name,
                    origin: ToolOrigin::Local,
                });
                out.push(ModelStreamChunk::ToolCallArgsDelta {
                    index: idx,
                    partial_json: args.to_string(),
                });
                out.push(ModelStreamChunk::ToolCallEnd { index: idx });
            }
        }

        // Finish reason on this snapshot's candidate?
        if let Some(reason_raw) = snapshot
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|c| c.get("finishReason"))
            .and_then(Value::as_str)
        {
            let has_calls = new_tool_count > 0;
            let reason = decode_finish_reason(reason_raw, has_calls);
            let usage = snapshot
                .get("usageMetadata")
                .map(decode_usage)
                .unwrap_or_default();
            out.push(ModelStreamChunk::Finish { reason, usage });
        }

        state.inner = Some(snapshot);
        Ok(out)
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
            Role::Assistant => "model",
        };
        let parts = m
            .content
            .iter()
            .map(encode_content_part)
            .collect::<Result<Vec<_>>>()?;
        out.push(json!({"role": role, "parts": parts}));
    }
    Ok(out)
}

fn encode_content_part(part: &ContentPart) -> Result<Value> {
    Ok(match part {
        ContentPart::Text { text } => json!({"text": text}),
        ContentPart::Image { source, mime } => match source {
            MediaSource::Base64 { data } => {
                json!({"inlineData": {"mimeType": mime, "data": data}})
            }
            MediaSource::Url { url } => {
                json!({"fileData": {"mimeType": mime, "fileUri": url}})
            }
            MediaSource::FileId { id } => {
                json!({"fileData": {"mimeType": mime, "fileUri": id}})
            }
        },
        ContentPart::Document { source, mime } => match source {
            MediaSource::Base64 { data } => {
                json!({"inlineData": {"mimeType": mime, "data": data}})
            }
            MediaSource::Url { url } => json!({"fileData": {"mimeType": mime, "fileUri": url}}),
            MediaSource::FileId { id } => json!({"fileData": {"mimeType": mime, "fileUri": id}}),
        },
        ContentPart::Source { url, title, .. } => {
            let label = title.clone().unwrap_or_else(|| url.clone());
            json!({"text": format!("[{label}]({url})")})
        }
        ContentPart::ToolCall {
            name, arguments, ..
        } => {
            json!({"functionCall": {"name": name, "args": arguments}})
        }
        ContentPart::ToolResult {
            tool_call_id: _,
            tool_name,
            content,
            is_error,
        } => {
            // Gemini functionResponse requires the tool name, not the id.
            // The agent runtime should populate `tool_name` from the
            // matching ToolCall; if it's missing we emit an empty string
            // and a warning so the failure is observable.
            let name = tool_name.as_deref().unwrap_or("");
            let response_value = match content {
                ToolResultContent::Text(s) => json!({"result": s}),
                ToolResultContent::Json(v) => json!({"result": v}),
                ToolResultContent::MultiPart(_) => json!({"result": "<multipart>"}),
            };
            let mut response = response_value;
            if *is_error && let Some(obj) = response.as_object_mut() {
                obj.insert("error".into(), json!(true));
            }
            json!({"functionResponse": {"name": name, "response": response}})
        }
        ContentPart::Reasoning { content, .. } => match content {
            ReasoningContent::Visible { text } => json!({"text": text, "thought": true}),
            ReasoningContent::Redacted { data } => {
                // Gemini has no redacted reasoning channel; round-trip via
                // the unknown text field with the data preserved.
                json!({"text": format!("[redacted-reasoning:{data}]"), "thought": true})
            }
        },
        ContentPart::Unknown {
            codec_id, payload, ..
        } => {
            if codec_id == CODEC_ID {
                payload.clone()
            } else {
                return Err(Error::InvalidRequest(format!(
                    "cannot encode Unknown part from codec {codec_id} through gemini-generate",
                )));
            }
        }
    })
}

fn encode_tool_definition(tool: &ToolDefinition) -> Value {
    let mut obj = json!({
        "name": tool.name,
        "parameters": tool.parameters,
    });
    if let Some(desc) = &tool.description {
        obj["description"] = json!(desc);
    }
    obj
}

fn encode_tool_choice(choice: &crate::ir::ToolChoice) -> Value {
    match choice {
        crate::ir::ToolChoice::Auto => json!({"functionCallingConfig": {"mode": "AUTO"}}),
        crate::ir::ToolChoice::Required => json!({"functionCallingConfig": {"mode": "ANY"}}),
        crate::ir::ToolChoice::None => json!({"functionCallingConfig": {"mode": "NONE"}}),
        crate::ir::ToolChoice::Tool { name } => json!({
            "functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": [name]}
        }),
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
    if opts.openai.is_some() {
        warnings.push(ModelWarning::DroppedProviderOption {
            provider: "openai".into(),
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

fn decode_parts(parts: &[Value]) -> (Vec<ContentPart>, bool) {
    let mut out = Vec::with_capacity(parts.len());
    let mut tool_index: usize = 0;
    let mut has_tool_calls = false;
    for p in parts {
        if let Some(text) = p.get("text").and_then(Value::as_str) {
            let is_thought = p.get("thought").and_then(Value::as_bool).unwrap_or(false);
            if is_thought {
                out.push(ContentPart::Reasoning {
                    content: ReasoningContent::Visible { text: text.into() },
                    kind: ReasoningKind::FullTrace,
                    signature: None,
                });
            } else {
                out.push(ContentPart::Text { text: text.into() });
            }
            continue;
        }
        if let Some(fc) = p.get("functionCall") {
            has_tool_calls = true;
            let name = fc
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = fc.get("args").cloned().unwrap_or(Value::Null);
            out.push(ContentPart::ToolCall {
                id: format!("call_{tool_index}"),
                name,
                arguments,
                origin: ToolOrigin::Local,
            });
            tool_index += 1;
            continue;
        }
        if let Some(inline) = p.get("inlineData") {
            let mime = inline
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let data = inline
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            out.push(ContentPart::Image {
                source: MediaSource::Base64 { data },
                mime,
            });
            continue;
        }
        // Fallback — preserve the part verbatim.
        out.push(ContentPart::Unknown {
            codec_id: CODEC_ID.to_string(),
            schema_version: 1,
            payload: p.clone(),
        });
    }
    (out, has_tool_calls)
}

fn decode_finish_reason(raw: &str, has_tool_calls: bool) -> FinishReason {
    if has_tool_calls && (raw == "STOP" || raw.is_empty()) {
        return FinishReason::ToolCalls;
    }
    match raw {
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
            FinishReason::ContentFilter
        }
        "MALFORMED_FUNCTION_CALL" => FinishReason::Error,
        "" => FinishReason::Stop,
        other => FinishReason::Other(other.to_string()),
    }
}

fn decode_usage(v: &Value) -> Usage {
    Usage {
        input_tokens: v
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: v
            .get("candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: v.get("cachedContentTokenCount").and_then(Value::as_u64),
        cache_creation_tokens: None,
        reasoning_tokens: v.get("thoughtsTokenCount").and_then(Value::as_u64),
        audio_input_tokens: None,
        audio_output_tokens: None,
        server_tool_invocations: None,
        raw: Some(v.clone()),
    }
}

// =============================================================================
// Streaming snapshot helpers
// =============================================================================

fn strip_sse_data_prefix(s: &str) -> &str {
    s.lines()
        .find_map(|line| {
            line.strip_prefix("data: ")
                .or_else(|| line.strip_prefix("data:"))
        })
        .unwrap_or(s)
}

fn extract_concatenated_text(snapshot: &Value) -> String {
    let mut buf = String::new();
    if let Some(parts) = snapshot
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
    {
        for p in parts {
            if let Some(text) = p.get("text").and_then(Value::as_str) {
                let is_thought = p.get("thought").and_then(Value::as_bool).unwrap_or(false);
                if !is_thought {
                    buf.push_str(text);
                }
            }
        }
    }
    buf
}

fn count_function_calls(snapshot: &Value) -> usize {
    snapshot
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("functionCall").is_some())
                .count()
        })
        .unwrap_or(0)
}

fn collect_function_calls(snapshot: &Value) -> Vec<&Value> {
    snapshot
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .map(|parts| parts.iter().filter_map(|p| p.get("functionCall")).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{GeminiOptions, ModelSettings, ReasoningSettings, SafetySetting};

    fn req(messages: Vec<Message>) -> ModelRequest {
        ModelRequest::new("gemini-2.5-flash", messages)
    }

    #[test]
    fn id_and_capabilities() {
        let c = GeminiGenerateCodec::new();
        assert_eq!(c.id(), "gemini-generate");
        assert_eq!(
            c.capabilities().tool_calls.id_semantics,
            ToolIdSemantics::SynthesizedByIndex
        );
        assert_eq!(
            c.endpoint_shape().path_template,
            "v1beta/models/{model}:{verb}"
        );
        assert_eq!(c.endpoint_shape().verb_unary, "generateContent");
        assert_eq!(c.endpoint_shape().verb_stream, "streamGenerateContent");
        assert_eq!(c.endpoint_shape().stream_query, &[("alt", "sse")]);
    }

    #[test]
    fn encode_basic_request() {
        let c = GeminiGenerateCodec::new();
        let r = req(vec![Message::user("hello")]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["contents"][0]["role"], "user");
        assert_eq!(enc.body["contents"][0]["parts"][0]["text"], "hello");
        assert!(enc.warnings.is_empty());
    }

    #[test]
    fn encode_response_format_json_schema_into_generation_config() {
        let c = GeminiGenerateCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema {
            name: "Person".into(),
            schema: json!({"type": "object", "properties": {"name": {"type": "string"}}}),
            strict: true,
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(
            enc.body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(
            enc.body["generationConfig"]["responseSchema"]["type"],
            "object"
        );
    }

    #[test]
    fn encode_response_format_json_object_sets_mime_type_only() {
        let c = GeminiGenerateCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonObject);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(
            enc.body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert!(enc.body["generationConfig"].get("responseSchema").is_none());
    }

    #[test]
    fn encode_response_format_text_sets_text_mime_type() {
        let c = GeminiGenerateCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.response_format = Some(ResponseFormat::Text);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(
            enc.body["generationConfig"]["responseMimeType"],
            "text/plain"
        );
    }

    #[test]
    fn encode_assistant_role_renamed_to_model() {
        let c = GeminiGenerateCodec::new();
        let r = req(vec![Message::assistant("hi")]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["contents"][0]["role"], "model");
    }

    #[test]
    fn encode_system_prompt_to_system_instruction() {
        let c = GeminiGenerateCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.system = Some(SystemPrompt::Text("be brief".into()));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(
            enc.body["systemInstruction"]["parts"][0]["text"],
            "be brief"
        );
    }

    #[test]
    fn encode_system_block_metadata_emits_lossy_warning() {
        use crate::ir::{CacheMarker, SystemBlock};
        let c = GeminiGenerateCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.system = Some(SystemPrompt::Blocks(vec![SystemBlock {
            text: "x".into(),
            cache_marker: Some(CacheMarker::ephemeral()),
        }]));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(enc
            .warnings
            .iter()
            .any(|w| matches!(w, ModelWarning::LossyEncode { field, .. } if field == "system.cache_marker")));
    }

    #[test]
    fn encode_tool_definitions_under_function_declarations() {
        let c = GeminiGenerateCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.tools = vec![ToolDefinition::new(
            "calc",
            json!({"type": "object", "properties": {"a": {"type": "number"}}}),
        )];
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(
            enc.body["tools"][0]["functionDeclarations"][0]["name"],
            "calc"
        );
        assert!(enc.body["tools"][0]["functionDeclarations"][0]["parameters"].is_object());
    }

    #[test]
    fn encode_settings_to_generation_config() {
        let c = GeminiGenerateCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings = ModelSettings {
            max_output_tokens: Some(1024),
            temperature: Some(0.7),
            top_p: Some(0.9),
            top_k: Some(40),
            seed: Some(42),
            stop_sequences: vec!["END".into()],
            ..Default::default()
        };
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let gc = &enc.body["generationConfig"];
        assert_eq!(gc["maxOutputTokens"], 1024);
        assert!((gc["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-3);
        assert!((gc["topP"].as_f64().unwrap() - 0.9).abs() < 1e-3);
        assert_eq!(gc["topK"], 40);
        assert_eq!(gc["seed"], 42);
        assert_eq!(gc["stopSequences"][0], "END");
    }

    #[test]
    fn encode_reasoning_to_thinking_config() {
        let c = GeminiGenerateCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.settings.reasoning = Some(ReasoningSettings {
            budget_tokens: Some(8192),
            effort: None,
            include_thoughts: true,
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(
            enc.body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            8192
        );
        assert_eq!(
            enc.body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
    }

    #[test]
    fn encode_gemini_provider_options_safety_and_mime() {
        let c = GeminiGenerateCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.provider_options.gemini = Some(GeminiOptions {
            response_mime_type: Some("application/json".into()),
            safety_settings: vec![SafetySetting {
                category: "HARM_CATEGORY_HARASSMENT".into(),
                threshold: "BLOCK_MEDIUM_AND_ABOVE".into(),
            }],
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(
            enc.body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(
            enc.body["safetySettings"][0]["category"],
            "HARM_CATEGORY_HARASSMENT"
        );
    }

    #[test]
    fn encode_drops_sibling_provider_options_with_warning() {
        let c = GeminiGenerateCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.provider_options.anthropic = Some(crate::ir::AnthropicOptions::default());
        r.provider_options.openai = Some(crate::ir::OpenAiOptions::default());
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
        assert!(providers.contains(&"openai"));
    }

    #[test]
    fn encode_tool_call_to_function_call() {
        let c = GeminiGenerateCodec::new();
        let r = req(vec![Message {
            role: Role::Assistant,
            content: vec![ContentPart::ToolCall {
                id: "call_0".into(),
                name: "calc".into(),
                arguments: json!({"a": 1}),
                origin: ToolOrigin::Local,
            }],
        }]);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let part = &enc.body["contents"][0]["parts"][0];
        assert_eq!(part["functionCall"]["name"], "calc");
        assert_eq!(part["functionCall"]["args"]["a"], 1);
    }

    #[test]
    fn decode_response_basic() {
        let c = GeminiGenerateCodec::new();
        let raw = json!({
            "responseId": "r_1",
            "modelVersion": "gemini-2.5-flash",
            "candidates": [{
                "content": {"parts": [{"text": "pong"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 1,
                "totalTokenCount": 6
            }
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.id, "r_1");
        assert_eq!(resp.text(), "pong");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 1);
    }

    #[test]
    fn decode_response_with_tool_call_synthesizes_id_by_index() {
        let c = GeminiGenerateCodec::new();
        let raw = json!({
            "candidates": [{
                "content": {"parts": [
                    {"functionCall": {"name": "calc", "args": {"a": 1}}},
                    {"functionCall": {"name": "calc", "args": {"a": 2}}}
                ]},
                "finishReason": "STOP"
            }]
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        let ids: Vec<_> = resp
            .tool_calls()
            .filter_map(|p| match p {
                ContentPart::ToolCall { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["call_0", "call_1"]);
    }

    #[test]
    fn decode_response_safety_finish_reason() {
        let c = GeminiGenerateCodec::new();
        let raw = json!({
            "candidates": [{"content": {"parts": []}, "finishReason": "SAFETY"}]
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.finish_reason, FinishReason::ContentFilter);
    }

    #[test]
    fn decode_response_thought_part_becomes_reasoning() {
        let c = GeminiGenerateCodec::new();
        let raw = json!({
            "candidates": [{
                "content": {"parts": [
                    {"text": "thinking...", "thought": true},
                    {"text": "answer"}
                ]},
                "finishReason": "STOP"
            }]
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        let kinds: Vec<&str> = resp
            .content
            .iter()
            .map(|p| match p {
                ContentPart::Reasoning { .. } => "reasoning",
                ContentPart::Text { .. } => "text",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["reasoning", "text"]);
    }

    #[test]
    fn decode_usage_extracts_thoughts_tokens() {
        let c = GeminiGenerateCodec::new();
        let raw = json!({
            "candidates": [{"content": {"parts": [{"text": "x"}]}, "finishReason": "STOP"}],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "thoughtsTokenCount": 100,
                "cachedContentTokenCount": 8
            }
        });
        let resp = c.decode_response(raw, InvocationMode::Unary).unwrap();
        assert_eq!(resp.usage.reasoning_tokens, Some(100));
        assert_eq!(resp.usage.cached_input_tokens, Some(8));
    }

    #[test]
    fn decode_stream_first_frame_emits_message_start() {
        let c = GeminiGenerateCodec::new();
        let mut state = StreamDecodeState::new();
        let frame = br#"{"responseId":"r_1","modelVersion":"gemini-2.5-flash","candidates":[{"content":{"parts":[{"text":"He"}]}}]}"#;
        let chunks = c.decode_stream_chunk(frame, &mut state).unwrap();
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
    fn decode_stream_snapshot_diffs_text() {
        let c = GeminiGenerateCodec::new();
        let mut state = StreamDecodeState::new();
        let f1 = br#"{"candidates":[{"content":{"parts":[{"text":"Hel"}]}}]}"#;
        let f2 = br#"{"candidates":[{"content":{"parts":[{"text":"Hello world"}]}}]}"#;
        let _ = c.decode_stream_chunk(f1, &mut state).unwrap();
        let chunks2 = c.decode_stream_chunk(f2, &mut state).unwrap();
        let delta = chunks2.iter().find_map(|c| match c {
            ModelStreamChunk::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        });
        assert_eq!(delta, Some("lo world"));
    }

    #[test]
    fn decode_stream_finish_reason_emitted() {
        let c = GeminiGenerateCodec::new();
        let mut state = StreamDecodeState::new();
        let frame = br#"{"candidates":[{"content":{"parts":[{"text":"done"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1}}"#;
        let chunks = c.decode_stream_chunk(frame, &mut state).unwrap();
        assert!(chunks.iter().any(|c| matches!(
            c,
            ModelStreamChunk::Finish {
                reason: FinishReason::Stop,
                ..
            }
        )));
    }

    #[test]
    fn decode_stream_function_call_synthesizes_index_id() {
        let c = GeminiGenerateCodec::new();
        let mut state = StreamDecodeState::new();
        let frame = br#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"calc","args":{"a":1}}}]},"finishReason":"STOP"}]}"#;
        let chunks = c.decode_stream_chunk(frame, &mut state).unwrap();
        let start = chunks.iter().find_map(|c| match c {
            ModelStreamChunk::ToolCallStart {
                id, name, index, ..
            } => Some((id.clone(), name.clone(), *index)),
            _ => None,
        });
        assert_eq!(start, Some(("call_0".to_string(), "calc".to_string(), 0)));
        // Finish should be ToolCalls, not Stop, because there's a function call.
        assert!(chunks.iter().any(|c| matches!(
            c,
            ModelStreamChunk::Finish {
                reason: FinishReason::ToolCalls,
                ..
            }
        )));
    }

    #[test]
    fn decode_stream_handles_data_prefix() {
        let c = GeminiGenerateCodec::new();
        let mut state = StreamDecodeState::new();
        let frame = b"data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"x\"}]}}]}";
        let chunks = c.decode_stream_chunk(frame, &mut state).unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, ModelStreamChunk::TextDelta { text, .. } if text == "x"))
        );
    }
}
