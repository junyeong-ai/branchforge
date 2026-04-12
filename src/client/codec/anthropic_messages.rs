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

#![allow(missing_docs)]

use serde_json::{Value, json};

use super::{
    ApiVersionHint, EncodedRequest, EndpointShape, HeaderSource, HeaderSpec, InvocationMode,
    ModelCodec,
};
use crate::client::schema::{
    SchemaPolicy, prepare_schema, prepare_tool_schema, warn_dropped_metadata,
};
use crate::ir::{
    CacheGranularity, CacheSupport, ContentPart, FinishReason, MediaSource, Message, ModelRequest,
    ModelResponse, ModelStreamChunk, ModelWarning, ProviderCapabilities, ReasoningContent,
    ReasoningKind, ReasoningSignature, ReasoningSupport, ResponseFormat, Role, StreamDecodeState,
    Support, SystemPrompt, SystemPromptShape, ToolCallSupport, ToolDefinition, ToolIdSemantics,
    ToolOrigin, ToolResultContent, Usage, VisionSupport,
};
use crate::{Error, Result};

const CODEC_ID: &str = "anthropic-messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const SCHEMA_POLICY: SchemaPolicy = SchemaPolicy::anthropic();

const SHAPE: EndpointShape = EndpointShape {
    codec_id: "anthropic-messages",
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
        // Anthropic GA structured outputs ship strict tool use via the
        // same grammar compiler: `strict: true` on a tool definition
        // guarantees schema-constrained inputs. The codec emits the
        // wire flag and runs the tool's input_schema through the
        // shared `prepare_tool_schema` pipeline.
        strict_schema: true,
        id_semantics: ToolIdSemantics::Provided,
    },
    structured_output: crate::ir::StructuredOutputSupport {
        // JsonObject has no portable mapping on Anthropic structured
        // outputs — the API requires an explicit schema. The codec
        // surfaces a `response_format.json_object` CapabilityEmulated
        // warning when callers set `ResponseFormat::JsonObject`.
        json_object: Support::Emulated,
        // JsonSchema is native via `output_config.format` (GA on the
        // Claude API and Amazon Bedrock as of 2026-04). The codec runs
        // the schema through `SchemaPolicy::anthropic()` to strip
        // unsupported keywords before wire submission.
        json_schema: Support::Native,
        // Anthropic structured outputs are always grammar-constrained —
        // the provider validates the schema strictly server-side. The
        // `JsonSchemaSpec::strict` flag is therefore a no-op here.
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
        granularity: CacheGranularity::Block,
    },
    reasoning: ReasoningSupport {
        mode: Support::Native,
        exposes_text: true,
        // The Anthropic Messages API bills extended-thinking tokens as
        // part of `output_tokens` — there is no separate
        // `reasoning_tokens` field on the response. `decode_usage`
        // therefore sets `usage.reasoning_tokens = None`, and this
        // capability flag must report the same.
        exposes_tokens: false,
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
            let mut tool_defs = Vec::with_capacity(request.tools.len());
            for tool in &request.tools {
                tool_defs.push(encode_tool_definition(tool, &mut warnings));
            }
            body["tools"] = json!(tool_defs);
        }

        if let Some(choice) = &request.tool_choice {
            body["tool_choice"] = encode_tool_choice(choice);
        }

        // Structured output. Anthropic GA ships a native
        // `output_config.format` parameter on the Messages API. The
        // precondition `ensure_not_prefilling` preempts the
        // structured-output × message-prefilling incompatibility so
        // callers see a local error instead of a wire-level 400;
        // `encode_response_format` then emits the native envelope.
        if let Some(format) = &request.response_format {
            ensure_not_prefilling(&request.messages)?;
            encode_response_format(format, &mut body, &mut warnings);
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

        // Cache_control post-processing chokepoint. Two API constraints
        // are enforced here at the codec level so the agent runtime
        // doesn't have to know about Anthropic-specific limits:
        //
        // 1. **TTL ordering normalisation** runs FIRST. Anthropic rejects
        //    requests where a longer TTL marker comes after a shorter
        //    TTL marker in the `tools → system → messages` processing
        //    order. The normalizer auto-upgrades earlier markers to
        //    match the longest later TTL — this preserves all markers
        //    (no cache breakpoints lost) while making the stream
        //    monotonically non-increasing. Upgrading is semantically
        //    safe for prompt caching because longer TTL just extends
        //    cache lifetime; cache hits only match identical prefixes,
        //    so there is no stale-data risk and Anthropic does not bill
        //    for cache storage time.
        //
        // 2. **Marker cap** (MAX_CACHE_BREAKPOINTS = 4). Multiple sources
        //    can contribute markers (per-block `cache_marker` on system
        //    blocks, `apply_cache_control` on system/tools/conversation),
        //    so the runtime can easily exceed the cap. Drop the EARLIEST
        //    markers in wire stream order since later markers cache more
        //    content (Anthropic incremental caching includes everything
        //    before the marker).
        let upgraded = normalize_cache_ttl_ordering(&mut body);
        if upgraded > 0 {
            warnings.push(ModelWarning::lossy(
                "cache_control_ttl",
                "anthropic-messages requires cache_control TTLs to be \
                 monotonically non-increasing in `tools → system → messages` \
                 order; auto-upgraded earlier markers to match a later \
                 marker's longer TTL",
            ));
        }
        let removed = enforce_cache_breakpoint_cap(&mut body);
        if removed > 0 {
            warnings.push(ModelWarning::lossy(
                "cache_control",
                "anthropic-messages caps cache_control at 4 markers per request; \
                 dropped earliest markers",
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
            tool_name: _, // Anthropic echoes only the id
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
        SystemPrompt::Blocks(blocks) => {
            // The block immediately preceding the structural
            // [`SystemBlockRole::Boundary`] marker is the last
            // cacheable static block; promote it to carry an
            // ephemeral cache_control so the static prefix is
            // cached even when the dynamic suffix changes between
            // calls. The boundary block itself is dropped — codecs
            // never serialise it to the wire.
            let boundary_index = sp.boundary_index();

            let serialized: Vec<Value> = blocks
                .iter()
                .enumerate()
                .filter_map(|(i, b)| {
                    if b.role.is_boundary() {
                        return None;
                    }
                    let mut obj = json!({"type": "text", "text": b.text});
                    let promote_cache =
                        boundary_index.is_some_and(|bi| i + 1 == bi) && b.cache_marker.is_none();
                    if let Some(marker) = &b.cache_marker {
                        let mut cc = json!({"type": "ephemeral"});
                        if let Some(ttl) = &marker.ttl {
                            cc["ttl"] = json!(ttl);
                        }
                        obj["cache_control"] = cc;
                    } else if promote_cache {
                        obj["cache_control"] = json!({"type": "ephemeral"});
                    }
                    Some(obj)
                })
                .collect();
            json!(serialized)
        }
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

    let mut obj = json!({
        "name": tool.name,
        "input_schema": prepared.value,
    });
    if let Some(desc) = &tool.description {
        obj["description"] = json!(desc);
    }
    // Anthropic's GA strict tool use reuses the same `output_config`
    // grammar compiler — emit `strict: true` on the wire when the IR
    // asks for it.
    if tool.strict {
        obj["strict"] = json!(true);
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

/// Anthropic Messages API caps prompt-caching at this many `cache_control`
/// markers per request. Exceeding the limit returns
/// `400 invalid_request_error: A maximum of 4 blocks with cache_control may
/// be provided.`.
const MAX_CACHE_BREAKPOINTS: usize = 4;

/// Visit each `cache_control` marker in the request body in **wire stream
/// order**. Anthropic's API documentation (and the live error message we
/// observed during R10 — `"blocks are processed in the following order:
/// tools, system, messages"`) define this as the canonical order, NOT the
/// order in which fields appear in the JSON object.
///
/// `f` is invoked once per marker with a closure that, when called,
/// removes that marker from the body. Returning `false` from the visitor
/// stops further iteration.
///
/// This iterator centralises the wire-stream traversal so the cap
/// enforcement and TTL ordering enforcement (and any future cache_control
/// post-processor) can share one source of truth for "what's earliest"
/// and "what comes after what".
fn for_each_cache_marker_in_stream_order<F>(body: &mut Value, mut f: F)
where
    F: FnMut(&mut serde_json::Map<String, Value>) -> CacheMarkerVisit,
{
    // 1. tools (FIRST in the API processing stream)
    if let Some(arr) = body.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in arr.iter_mut() {
            if let Some(obj) = tool.as_object_mut()
                && obj.contains_key("cache_control")
                && f(obj) == CacheMarkerVisit::Stop
            {
                return;
            }
        }
    }
    // 2. system (SECOND in the API processing stream)
    if let Some(arr) = body.get_mut("system").and_then(Value::as_array_mut) {
        for block in arr.iter_mut() {
            if let Some(obj) = block.as_object_mut()
                && obj.contains_key("cache_control")
                && f(obj) == CacheMarkerVisit::Stop
            {
                return;
            }
        }
    }
    // 3. messages (LAST in the API processing stream)
    if let Some(arr) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for msg in arr.iter_mut() {
            if let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) {
                for block in content.iter_mut() {
                    if let Some(obj) = block.as_object_mut()
                        && obj.contains_key("cache_control")
                        && f(obj) == CacheMarkerVisit::Stop
                    {
                        return;
                    }
                }
            }
        }
    }
}

#[derive(PartialEq, Eq)]
enum CacheMarkerVisit {
    Continue,
    Stop,
}

/// Walk the encoded body and ensure at most [`MAX_CACHE_BREAKPOINTS`]
/// `cache_control` markers remain. Markers later in the request stream
/// cache more content (Anthropic incremental caching includes everything
/// before the marker), so we drop the EARLIEST markers when over the cap.
///
/// "Earliest" means earliest in the API's stream processing order
/// (`tools → system → messages`), NOT the order JSON fields appear in
/// the body. See [`for_each_cache_marker_in_stream_order`] for the
/// canonical traversal.
///
/// Returns the number of markers removed so the caller can surface a
/// `ModelWarning::lossy` for observability.
fn enforce_cache_breakpoint_cap(body: &mut Value) -> usize {
    let total = count_cache_markers(body);
    if total <= MAX_CACHE_BREAKPOINTS {
        return 0;
    }

    let mut to_drop = total - MAX_CACHE_BREAKPOINTS;
    let removed = to_drop;

    for_each_cache_marker_in_stream_order(body, |obj| {
        if to_drop == 0 {
            return CacheMarkerVisit::Stop;
        }
        obj.remove("cache_control");
        to_drop -= 1;
        CacheMarkerVisit::Continue
    });

    // This is a correctness invariant, not a perf diagnostic — if
    // `for_each_cache_marker_in_stream_order` failed to visit all markers
    // the request would go out with too many cache_control blocks and the
    // API would reject it. Use `assert!` so release builds catch the
    // mismatch too (the cost is a single comparison on a hot path that
    // runs at most once per LLM call).
    assert_eq!(
        to_drop, 0,
        "enforce_cache_breakpoint_cap: failed to remove all excess markers \
         (to_drop={to_drop}, removed={removed})"
    );
    removed
}

/// Total number of `cache_control` markers across the request body. Walks
/// in wire stream order, but the count is order-independent.
fn count_cache_markers(body: &Value) -> usize {
    let mut n = 0;
    if let Some(arr) = body.get("tools").and_then(Value::as_array) {
        n += arr
            .iter()
            .filter(|t| t.get("cache_control").is_some())
            .count();
    }
    if let Some(arr) = body.get("system").and_then(Value::as_array) {
        n += arr
            .iter()
            .filter(|b| b.get("cache_control").is_some())
            .count();
    }
    if let Some(arr) = body.get("messages").and_then(Value::as_array) {
        for msg in arr {
            if let Some(content) = msg.get("content").and_then(Value::as_array) {
                n += content
                    .iter()
                    .filter(|b| b.get("cache_control").is_some())
                    .count();
            }
        }
    }
    n
}

/// Anthropic enforces TTL ordering across the request stream: a longer
/// TTL marker must NOT come after a shorter TTL marker (where "after"
/// means later in the `tools → system → messages` processing order).
/// Violating this returns
/// `400 invalid_request_error: a ttl='1h' cache_control block must not
/// come after a ttl='5m' cache_control block`.
///
/// In normal operation, both per-block markers (from
/// `SystemBlock::cache_marker.ttl`) and `apply_cache_control` markers
/// (from `cc.ttl`) source their TTL from
/// `cache_config.static_ttl`, so they trivially agree. This normalizer
/// exists to keep the codec robust against:
///   - Future refactors that introduce a second TTL source
///   - User code that constructs `SystemBlock` instances with mixed TTLs
///   - Higher-level cache strategies that intentionally vary TTL per block
///
/// **Strategy — auto-upgrade earlier markers** (NOT drop violators):
///
/// Walk in REVERSE wire stream order, tracking the running maximum TTL.
/// When a marker's TTL is *shorter* than the running maximum, upgrade
/// it to the maximum so the forward stream becomes monotonically
/// non-increasing. This preserves ALL markers — high-value system and
/// message markers (which cache more content) are not lost just because
/// an earlier tools marker had a shorter TTL.
///
/// Why upgrade and not drop:
///   - Dropping a marker LOSES a cache breakpoint entirely
///   - Upgrading just extends the cache lifetime — semantically safe
///     for prompt caching since cache hits only match identical
///     prefixes (no stale-data risk)
///   - Anthropic does not bill for cache storage time, only cache
///     creation and reads, so longer TTL has no cost penalty
///   - Auto-upgrade honours more user intent: the markers chosen for
///     specific positions stay where they were, just with a longer
///     lifetime that satisfies the API constraint
///
/// Returns the number of markers whose TTL was upgraded so the caller
/// can surface a `ModelWarning::lossy` for observability.
fn normalize_cache_ttl_ordering(body: &mut Value) -> usize {
    // Pass 1 — collect current TTL strings in wire stream order. We need
    // two passes because Rust's borrow checker won't let us hold mutable
    // references across the iterator's reverse walk.
    let mut current_ttls: Vec<Option<String>> = Vec::new();
    for_each_cache_marker_in_stream_order(body, |obj| {
        let ttl = obj
            .get("cache_control")
            .and_then(|cc| cc.get("ttl"))
            .and_then(Value::as_str)
            .map(str::to_string);
        current_ttls.push(ttl);
        CacheMarkerVisit::Continue
    });

    if current_ttls.is_empty() {
        return 0;
    }

    // Pass 2 — compute target TTLs by walking the collected list in
    // REVERSE, tracking the running max. Each marker that's shorter than
    // the max gets upgraded to the max.
    let mut target_ttls: Vec<Option<String>> = current_ttls.clone();
    let mut running_max_seconds: u64 = 0;
    let mut running_max_string: Option<String> = None;
    for i in (0..target_ttls.len()).rev() {
        let cur_seconds = ttl_to_seconds(target_ttls[i].as_deref());
        if cur_seconds > running_max_seconds {
            running_max_seconds = cur_seconds;
            running_max_string = target_ttls[i].clone();
        } else if cur_seconds < running_max_seconds {
            // This marker's TTL is shorter than a later marker's TTL —
            // upgrade it. Use the exact string of the longer marker so
            // we don't introduce a TTL the API doesn't recognise.
            target_ttls[i] = running_max_string.clone();
        }
        // cur_seconds == running_max_seconds → no change needed
    }

    // Pass 3 — apply target TTLs to markers in stream order.
    let mut idx = 0_usize;
    let mut upgraded = 0_usize;
    for_each_cache_marker_in_stream_order(body, |obj| {
        let target = target_ttls[idx].as_deref();
        let current = current_ttls[idx].as_deref();
        if target != current {
            // Insert or replace the `ttl` field on this marker's
            // cache_control object.
            if let Some(cc) = obj.get_mut("cache_control").and_then(Value::as_object_mut) {
                match target {
                    Some(ttl) => {
                        cc.insert("ttl".into(), Value::String(ttl.to_string()));
                    }
                    None => {
                        cc.remove("ttl");
                    }
                }
                upgraded += 1;
            }
        }
        idx += 1;
        CacheMarkerVisit::Continue
    });

    upgraded
}

/// Convert a `cache_control.ttl` string into seconds for ordering
/// comparisons. Anthropic currently accepts `"5m"` and `"1h"`, but the
/// parser is generic over any `<integer><unit>` form (`s` / `m` / `h` /
/// `d`) so future-added TTL values automatically work without code
/// changes here. Missing or malformed values fall back to the API
/// default of 5 minutes (300 seconds).
///
/// The function is **panic-free for any UTF-8 input**. It uses
/// `char_indices().next_back()` rather than `str::split_at(len-1)`
/// because the latter panics when the trailing byte is in the middle
/// of a multi-byte char (e.g. `"5분"`, `"5🕐"`). Although the cache_ttl
/// is conventionally an ASCII string from `cache_config.static_ttl`,
/// nothing in the IR or builder API enforces that — a defensive
/// codec must not panic on any user-supplied string.
fn ttl_to_seconds(ttl: Option<&str>) -> u64 {
    const DEFAULT_SECONDS: u64 = 300;
    let Some(s) = ttl else {
        return DEFAULT_SECONDS;
    };
    if s.is_empty() {
        return DEFAULT_SECONDS;
    }

    // Take the LAST char (any UTF-8 byte width) and split on its
    // byte index. `char_indices().next_back()` is char-boundary-safe.
    let Some((unit_byte_idx, unit_char)) = s.char_indices().next_back() else {
        return DEFAULT_SECONDS;
    };
    let num_str = &s[..unit_byte_idx];
    if num_str.is_empty() {
        return DEFAULT_SECONDS;
    }

    let n: u64 = match num_str.parse() {
        Ok(n) => n,
        Err(_) => return DEFAULT_SECONDS,
    };
    // Use `saturating_mul` so absurdly large `n` (e.g. `"99999999999999999d"`
    // ≈ 1.7e17 days) clamps to `u64::MAX` instead of panicking on
    // arithmetic overflow in debug mode (or silently wrapping in
    // release). Saturated values are still strictly larger than any
    // realistic Anthropic TTL, so the comparison logic in
    // `normalize_cache_ttl_ordering` keeps producing the right answer.
    match unit_char {
        's' => n,
        'm' => n.saturating_mul(60),
        'h' => n.saturating_mul(3600),
        'd' => n.saturating_mul(86_400),
        // Unknown unit — fall back. The API will reject unknown TTLs
        // with a clearer error than anything we could produce locally.
        _ => DEFAULT_SECONDS,
    }
}

fn apply_cache_control(body: &mut Value, cc: &crate::ir::CacheControl) {
    // Anthropic enforces TTL ordering across the request stream (`tools`
    // → `system` → `messages`): a longer-TTL marker must NOT come after
    // a shorter-TTL marker. Per-block cache markers carry their TTL via
    // `SystemBlock::cache_marker.ttl`; the markers added here from
    // `CacheControl` flags must use the SAME TTL so the stream stays
    // monotonically non-increasing. Defaulting to no-ttl (5m) while the
    // per-block markers run at 1h triggers
    // `system.N.cache_control.ttl: a ttl='1h' must not come after a
    // ttl='5m' cache_control block` from the live API.
    let mut marker = json!({"type": "ephemeral"});
    if let Some(ttl) = &cc.ttl {
        marker["ttl"] = json!(ttl);
    }

    if cc.system
        && let Some(system) = body.get_mut("system")
        && let Some(arr) = system.as_array_mut()
        && let Some(last) = arr.last_mut()
        && let Some(obj) = last.as_object_mut()
    {
        obj.insert("cache_control".into(), marker.clone());
    }
    if cc.tools
        && let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut)
        && let Some(last) = tools.last_mut()
        && let Some(obj) = last.as_object_mut()
    {
        obj.insert("cache_control".into(), marker.clone());
    }
    if cc.conversation
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

/// Precondition: reject the Anthropic-specific incompatibility between
/// **message prefilling** (a trailing assistant message) and
/// `response_format`. Anthropic returns a 400 for this combination; the
/// codec converts it to a local `Error::InvalidRequest` so the caller
/// never wastes a wire round-trip.
fn ensure_not_prefilling(messages: &[Message]) -> Result<()> {
    if let Some(last) = messages.last()
        && last.role == Role::Assistant
    {
        return Err(Error::InvalidRequest(
            "anthropic-messages: message prefilling (trailing assistant message) is \
             incompatible with response_format; Anthropic returns a 400 for this \
             combination. Remove the trailing assistant message or drop \
             response_format."
                .to_string(),
        ));
    }
    Ok(())
}

/// Emit `output_config.format` for the given [`ResponseFormat`].
///
/// Callers are responsible for running [`ensure_not_prefilling`] first —
/// this helper assumes the precondition holds and focuses purely on
/// wire-format translation (SRP).
fn encode_response_format(
    format: &ResponseFormat,
    body: &mut Value,
    warnings: &mut Vec<ModelWarning>,
) {
    match format {
        ResponseFormat::Text => {
            // Text is the default — emitting a format block would be
            // redundant. Nothing to do.
        }
        ResponseFormat::JsonObject => {
            // Anthropic's structured outputs require a schema; JsonObject
            // has no portable mapping on this provider. Surface the
            // emulation gap as a CapabilityEmulated warning.
            warnings.push(ModelWarning::CapabilityEmulated {
                capability: "response_format.json_object".to_string(),
            });
        }
        ResponseFormat::JsonSchema(spec) => {
            let prepared = prepare_schema(
                spec.schema.clone(),
                &SCHEMA_POLICY,
                "response_format.schema",
            );
            warnings.extend(prepared.warnings);
            body["output_config"] = json!({
                "format": {
                    "type": "json_schema",
                    "schema": prepared.value,
                }
            });
            // `SCHEMA_POLICY.wire_supports_{name,description}` are both
            // `false` for Anthropic, so `warn_dropped_metadata` emits
            // lossy warnings for any metadata the caller set. Note that
            // `spec.strict` is intentionally silent — Anthropic is
            // always grammar-constrained, so the flag has no effect.
            warn_dropped_metadata(spec, &SCHEMA_POLICY, CODEC_ID, warnings);
        }
    }
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
    // Anthropic Messages reports `input_tokens` as the *non-cached* input
    // (the portion billed at the standard input rate). The IR contract
    // (see `ir::Usage::add` debug_assert) requires `input_tokens` to be
    // the TOTAL input — i.e. cached_input_tokens must be a subset of
    // input_tokens. Reconstruct the total here so accumulation across
    // turns and pricing math (`billable_input_tokens()`) stay correct.
    let fresh_input = v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let cache_read = v.get("cache_read_input_tokens").and_then(Value::as_u64);
    let cache_create = v.get("cache_creation_input_tokens").and_then(Value::as_u64);
    let total_input = fresh_input + cache_read.unwrap_or(0) + cache_create.unwrap_or(0);
    Usage {
        input_tokens: total_input,
        output_tokens: v.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        cached_input_tokens: cache_read,
        cache_creation_tokens: cache_create,
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
        // Same total-input reconstruction as `decode_usage` — the IR
        // contract requires `cached_input_tokens` to be a subset of
        // `input_tokens`, but Anthropic reports `input_tokens` as the
        // non-cached portion only.
        let fresh_input = usage.get("input_tokens").and_then(Value::as_u64);
        let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
        let cache_create = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64);
        let total_input = match (fresh_input, cache_read, cache_create) {
            (None, None, None) => None,
            _ => {
                Some(fresh_input.unwrap_or(0) + cache_read.unwrap_or(0) + cache_create.unwrap_or(0))
            }
        };
        let pu = crate::ir::PartialUsage {
            output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
            input_tokens: total_input,
            cached_input_tokens: cache_read,
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
        AnthropicOptions, CacheControl, Continuation, JsonSchemaSpec, ModelSettings,
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
    fn encode_tool_definition_strict_emits_wire_flag_and_applies_strict_policy() {
        // Anthropic strict tool use: `strict: true` on the wire and
        // the tool's input_schema runs through the full anthropic()
        // policy (numeric constraints stripped, etc.).
        let c = AnthropicMessagesCodec::new();
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
        assert_eq!(enc.body["tools"][0]["strict"], true);
        // `minimum` is stripped because strict mode applied the
        // anthropic() policy.
        assert!(
            enc.body["tools"][0]["input_schema"]["properties"]["a"]
                .get("minimum")
                .is_none()
        );
        assert!(enc.warnings.iter().any(|w| matches!(
            w, ModelWarning::LossyEncode { field, .. } if field.contains("minimum")
        )));
    }

    #[test]
    fn encode_tool_definition_non_strict_preserves_numeric_constraints() {
        // Non-strict tool uses lenient policy — constraints preserved.
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.tools = vec![ToolDefinition::new(
            "calc",
            json!({
                "type": "object",
                "properties": {
                    "a": {"type": "integer", "minimum": 0, "maximum": 100}
                }
            }),
        )];
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let a = &enc.body["tools"][0]["input_schema"]["properties"]["a"];
        assert_eq!(a["minimum"], 0);
        assert_eq!(a["maximum"], 100);
        // No `strict` flag on the wire.
        assert!(enc.body["tools"][0].get("strict").is_none());
    }

    #[test]
    fn encode_tool_definition_warnings_use_tool_name_source_path() {
        // Regression: walker warnings for tool schemas must be
        // attributed to `tool.<name>.input_schema`, not the default
        // `response_format.schema` prefix. This lets callers
        // disambiguate between response_format and per-tool lossy
        // warnings when both appear in the same request.
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        let mut tool = ToolDefinition::new(
            "measure_temperature",
            json!({
                "type": "object",
                "properties": {
                    "precision": {"type": "number", "minimum": 0.01}
                }
            }),
        );
        tool.strict = true;
        r.tools = vec![tool];
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        // At least one warning must reference the tool-specific path.
        assert!(
            enc.warnings.iter().any(|w| matches!(
                w, ModelWarning::LossyEncode { field, .. }
                if field.starts_with("tool.measure_temperature.input_schema/")
            )),
            "expected a warning prefixed with `tool.measure_temperature.input_schema/`, got: {:?}",
            enc.warnings
        );
        // And no tool warning should accidentally use the response_format prefix.
        assert!(
            !enc.warnings.iter().any(|w| matches!(
                w, ModelWarning::LossyEncode { field, .. }
                if field.starts_with("response_format.schema/") && field.contains("precision")
            )),
            "tool warning was misattributed to response_format.schema: {:?}",
            enc.warnings
        );
    }

    #[test]
    fn encode_tool_definition_strips_jsonschema_metadata_from_tool_parameters() {
        // Walker-level metadata strip applies to tool schemas too,
        // regardless of strict flag.
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.tools = vec![ToolDefinition::new(
            "calc",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {"a": {"type": "number"}}
            }),
        )];
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(
            enc.body["tools"][0]["input_schema"]
                .get("$schema")
                .is_none()
        );
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

    /// Inserting a `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` sentinel block
    /// must (a) drop the sentinel itself from the wire body and
    /// (b) auto-promote the block immediately preceding it to carry
    /// an ephemeral `cache_control` marker, so the static prefix is
    /// cached even when the dynamic suffix changes between calls.
    #[test]
    fn encode_dynamic_boundary_role_promotes_prefix_to_cached() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.system = Some(SystemPrompt::Blocks(vec![
            crate::ir::SystemBlock::uncached("static prefix"),
            crate::ir::SystemBlock::boundary(),
            crate::ir::SystemBlock::dynamic("dynamic rules"),
        ]));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let arr = enc.body["system"].as_array().expect("array");
        // Boundary dropped: 3 → 2 blocks.
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "static prefix");
        // Prefix block carries the cache marker promoted by the
        // boundary role.
        assert_eq!(arr[0]["cache_control"]["type"], "ephemeral");
        // Dynamic suffix block does NOT carry a cache marker.
        assert_eq!(arr[1]["text"], "dynamic rules");
        assert!(arr[1].get("cache_control").is_none());
    }

    #[test]
    fn encode_anthropic_cache_control_marks_system_block() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        r.system = Some(SystemPrompt::Blocks(vec![crate::ir::SystemBlock {
            text: "sys".into(),
            role: crate::ir::SystemBlockRole::Static,
            cache_marker: None,
        }]));
        r.provider_options.anthropic = Some(AnthropicOptions {
            cache_control: Some(CacheControl {
                system: true,
                ..Default::default()
            }),
            ..Default::default()
        });
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["system"][0]["cache_control"]["type"], "ephemeral");
    }

    /// R14 — End-to-end integration test for the cache_control pipeline.
    /// Builds a `ModelRequest` with **mixed per-block TTLs** that would
    /// trigger the live-API ordering rule, then asserts the final encoded
    /// body has been auto-normalised by `normalize_cache_ttl_ordering`
    /// (NOT capped/dropped). This locks in the contract that the codec
    /// post-processing pipeline runs end-to-end on a realistic input
    /// shape, not just on synthetic JSON fixtures.
    #[test]
    fn encode_request_normalizes_mixed_per_block_ttls_end_to_end() {
        use crate::ir::provider_options::CacheMarker;

        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        // Two system blocks, the FIRST is cached at 5m and the SECOND at 1h.
        // In wire stream order this is `system[0]=5m → system[1]=1h`,
        // which violates the "longer TTL must not come after shorter TTL"
        // rule. The R13 normalizer should auto-upgrade system[0] to 1h.
        r.system = Some(SystemPrompt::Blocks(vec![
            crate::ir::SystemBlock {
                text: "stable header".into(),
                role: crate::ir::SystemBlockRole::Static,
                cache_marker: Some(CacheMarker::with_ttl("5m")),
            },
            crate::ir::SystemBlock {
                text: "stable footer".into(),
                role: crate::ir::SystemBlockRole::Static,
                cache_marker: Some(CacheMarker::with_ttl("1h")),
            },
        ]));

        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();

        // Both system blocks must still have cache_control (no markers
        // dropped) and BOTH must now use the longer TTL ("1h") so the
        // wire stream is monotonically non-increasing.
        assert_eq!(enc.body["system"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(enc.body["system"][1]["cache_control"]["ttl"], "1h");

        // The codec should also surface a `lossy` warning so observers
        // know a TTL was upgraded — this is the audit trail for the
        // semantic change.
        assert!(
            enc.warnings.iter().any(|w| matches!(
                w,
                crate::ir::ModelWarning::LossyEncode { field, .. } if field == "cache_control_ttl"
            )),
            "expected cache_control_ttl lossy warning, got: {:?}",
            enc.warnings
        );
    }

    /// R14 — Same pipeline integration test for the cap path. Builds a
    /// request with > 4 markers to verify `enforce_cache_breakpoint_cap`
    /// runs end-to-end and emits the corresponding warning. The cap drops
    /// the EARLIEST markers in wire stream order (`tools → system →
    /// messages`), preserving the more valuable later markers.
    #[test]
    fn encode_request_caps_cache_markers_end_to_end() {
        use crate::ir::provider_options::CacheMarker;

        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi")]);
        // 5 cached system blocks → 5 cache_control markers, exceeds the
        // cap of 4. Note the API stream order is `tools → system →
        // messages`, so since this request has no tools, system[0] is
        // the earliest marker — that's the one that should be dropped.
        r.system = Some(SystemPrompt::Blocks(vec![
            crate::ir::SystemBlock {
                text: "s0".into(),
                role: crate::ir::SystemBlockRole::Static,
                cache_marker: Some(CacheMarker::with_ttl("1h")),
            },
            crate::ir::SystemBlock {
                text: "s1".into(),
                role: crate::ir::SystemBlockRole::Static,
                cache_marker: Some(CacheMarker::with_ttl("1h")),
            },
            crate::ir::SystemBlock {
                text: "s2".into(),
                role: crate::ir::SystemBlockRole::Static,
                cache_marker: Some(CacheMarker::with_ttl("1h")),
            },
            crate::ir::SystemBlock {
                text: "s3".into(),
                role: crate::ir::SystemBlockRole::Static,
                cache_marker: Some(CacheMarker::with_ttl("1h")),
            },
            crate::ir::SystemBlock {
                text: "s4".into(),
                role: crate::ir::SystemBlockRole::Static,
                cache_marker: Some(CacheMarker::with_ttl("1h")),
            },
        ]));

        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();

        // 5 markers → cap drops 1. The earliest (system[0]) is dropped.
        let marker_count = (0..5)
            .filter(|i| enc.body["system"][*i].get("cache_control").is_some())
            .count();
        assert_eq!(marker_count, 4, "must be capped at 4 markers");
        assert!(enc.body["system"][0].get("cache_control").is_none());
        assert!(enc.body["system"][1].get("cache_control").is_some());
        assert!(enc.body["system"][4].get("cache_control").is_some());

        // Cap warning surfaced.
        assert!(
            enc.warnings.iter().any(|w| matches!(
                w,
                crate::ir::ModelWarning::LossyEncode { field, .. } if field == "cache_control"
            )),
            "expected cache_control lossy warning, got: {:?}",
            enc.warnings
        );
    }

    #[test]
    fn encode_response_format_json_schema_emits_output_config() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema(JsonSchemaSpec::new(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}}
        }))));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert_eq!(enc.body["output_config"]["format"]["type"], "json_schema");
        // additionalProperties: false was added by the Anthropic policy.
        assert_eq!(
            enc.body["output_config"]["format"]["schema"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn encode_response_format_json_schema_drops_name_with_lossy_warning() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema(
            JsonSchemaSpec::new(json!({"type": "object"})).with_name("Person"),
        ));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(enc.warnings.iter().any(|w| matches!(
            w,
            ModelWarning::LossyEncode { field, .. } if field == "response_format.name"
        )));
    }

    #[test]
    fn encode_response_format_json_schema_drops_description_with_lossy_warning() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema(
            JsonSchemaSpec::new(json!({"type": "object"})).with_description("A record"),
        ));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(enc.warnings.iter().any(|w| matches!(
            w,
            ModelWarning::LossyEncode { field, .. } if field == "response_format.description"
        )));
    }

    #[test]
    fn encode_response_format_json_schema_strips_unsupported_keywords() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema(JsonSchemaSpec::new(json!({
            "type": "object",
            "properties": {
                "age": {"type": "integer", "minimum": 0, "maximum": 120}
            }
        }))));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        let age = &enc.body["output_config"]["format"]["schema"]["properties"]["age"];
        assert!(age.get("minimum").is_none());
        assert!(age.get("maximum").is_none());
        assert!(enc.warnings.iter().any(|w| matches!(
            w, ModelWarning::LossyEncode { field, .. } if field.contains("minimum"))));
        assert!(enc.warnings.iter().any(|w| matches!(
            w, ModelWarning::LossyEncode { field, .. } if field.contains("maximum"))));
    }

    #[test]
    fn encode_response_format_json_object_emits_capability_emulated() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonObject);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(enc.warnings.iter().any(|w| matches!(
            w,
            ModelWarning::CapabilityEmulated { capability }
            if capability == "response_format.json_object"
        )));
        // No output_config should be emitted.
        assert!(enc.body.get("output_config").is_none());
    }

    #[test]
    fn encode_response_format_text_is_noop() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("say hi")]);
        r.response_format = Some(ResponseFormat::Text);
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(enc.body.get("output_config").is_none());
    }

    #[test]
    fn encode_response_format_strict_false_is_silent_on_anthropic() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema(
            JsonSchemaSpec::new(json!({"type": "object"})).with_strict(false),
        ));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        // No warning for strict=false — Anthropic is always grammar-constrained.
        assert!(!enc.warnings.iter().any(|w| matches!(
            w, ModelWarning::LossyEncode { field, .. } if field.contains("strict")
        )));
    }

    #[test]
    fn encode_response_format_with_trailing_assistant_message_is_rejected() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("hi"), Message::assistant("prefilled")]);
        r.response_format = Some(ResponseFormat::JsonSchema(JsonSchemaSpec::new(json!({
            "type": "object"
        }))));
        let err = c.encode_request(&r, InvocationMode::Unary).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("prefilling") || message.contains("assistant"),
            "expected prefilling error, got: {message}"
        );
    }

    #[test]
    fn encode_response_format_json_schema_rejects_recursive_schema() {
        let c = AnthropicMessagesCodec::new();
        let mut r = req(vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema(JsonSchemaSpec::new(json!({
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {"next": {"$ref": "#/$defs/Node"}}
                }
            },
            "$ref": "#/$defs/Node"
        }))));
        let enc = c.encode_request(&r, InvocationMode::Unary).unwrap();
        assert!(enc.warnings.iter().any(|w| matches!(
            w, ModelWarning::LossyEncode { field, reason }
            if field.contains("$ref") && reason.contains("recursive"))));
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
        // IR contract: input_tokens is the TOTAL input. Anthropic reports
        // it as the *non-cached* portion (10), so the decoder reconstructs
        // 10 + 3 (cache_read) = 13. cached_input_tokens stays 3 (subset).
        assert_eq!(resp.usage.input_tokens, 13);
        assert_eq!(resp.usage.output_tokens, 5);
        assert_eq!(resp.usage.cached_input_tokens, Some(3));
        assert_eq!(resp.usage.billable_input_tokens(), 10);
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

    // =============================================================================
    // R10 fix regression guards
    //
    // Each function below pins one of the five fixes added in R10 after live
    // testing exposed bugs the static audits had missed. Without these unit
    // tests the fixes were only validated by the live integration test, which
    // does not run in CI.
    // =============================================================================

    /// R10-fix-3 — `enforce_cache_breakpoint_cap` drops the EARLIEST markers
    /// when the request stream contains more than `MAX_CACHE_BREAKPOINTS=4`.
    #[test]
    fn enforce_cache_cap_drops_earliest_when_over() {
        let mut body = json!({
            "system": [
                {"type": "text", "text": "s0", "cache_control": {"type": "ephemeral", "ttl": "1h"}},
                {"type": "text", "text": "s1", "cache_control": {"type": "ephemeral", "ttl": "1h"}},
                {"type": "text", "text": "s2", "cache_control": {"type": "ephemeral", "ttl": "1h"}},
            ],
            "tools": [
                {"name": "t0", "cache_control": {"type": "ephemeral", "ttl": "1h"}}
            ],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "u0", "cache_control": {"type": "ephemeral", "ttl": "1h"}}
                ]}
            ]
        });
        // 5 markers total, cap is 4 → drop 1. Earliest in the API's wire
        // stream order is `tools[0]` (the documented processing order is
        // `tools → system → messages`), NOT `system[0]`. Dropping the
        // earliest preserves the more valuable later markers (system
        // blocks cache the tools section + their own content; messages
        // cache everything before them).
        let removed = enforce_cache_breakpoint_cap(&mut body);
        assert_eq!(removed, 1);
        assert!(body["tools"][0].get("cache_control").is_none());
        assert!(body["system"][0].get("cache_control").is_some());
        assert!(body["system"][1].get("cache_control").is_some());
        assert!(body["system"][2].get("cache_control").is_some());
        assert!(
            body["messages"][0]["content"][0]
                .get("cache_control")
                .is_some()
        );
    }

    #[test]
    fn enforce_cache_cap_no_op_when_at_or_under_limit() {
        let mut body = json!({
            "system": [
                {"type": "text", "text": "s0", "cache_control": {"type": "ephemeral", "ttl": "1h"}},
                {"type": "text", "text": "s1", "cache_control": {"type": "ephemeral", "ttl": "1h"}},
            ],
            "tools": [{"name": "t0", "cache_control": {"type": "ephemeral", "ttl": "1h"}}],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "u0", "cache_control": {"type": "ephemeral", "ttl": "1h"}}]}
            ]
        });
        // Exactly 4 markers — no drop.
        assert_eq!(enforce_cache_breakpoint_cap(&mut body), 0);
        assert!(body["system"][0].get("cache_control").is_some());
        assert!(body["system"][1].get("cache_control").is_some());
    }

    #[test]
    fn enforce_cache_cap_handles_string_system_prompt() {
        // SystemPrompt::Text encodes `system` as a JSON string, not an array.
        // The cap function must skip it without panicking.
        let mut body = json!({
            "system": "plain string system",
            "messages": []
        });
        assert_eq!(enforce_cache_breakpoint_cap(&mut body), 0);
        assert_eq!(body["system"], "plain string system");
    }

    #[test]
    fn enforce_cache_cap_drops_across_all_three_positions() {
        // 6 markers split across system + tools + messages.
        let mut body = json!({
            "system": [
                {"type": "text", "text": "s0", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "s1", "cache_control": {"type": "ephemeral"}}
            ],
            "tools": [
                {"name": "t0", "cache_control": {"type": "ephemeral"}},
                {"name": "t1", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "u0", "cache_control": {"type": "ephemeral"}},
                    {"type": "text", "text": "u1", "cache_control": {"type": "ephemeral"}}
                ]}
            ]
        });
        // 6 markers total → drop 2. Wire stream order is `tools → system →
        // messages`, so the 2 earliest are `tools[0]` and `tools[1]`.
        // System and message markers are preserved (they cache more
        // content per Anthropic incremental caching).
        let removed = enforce_cache_breakpoint_cap(&mut body);
        assert_eq!(removed, 2);
        assert!(body["tools"][0].get("cache_control").is_none());
        assert!(body["tools"][1].get("cache_control").is_none());
        assert!(body["system"][0].get("cache_control").is_some());
        assert!(body["system"][1].get("cache_control").is_some());
        assert!(
            body["messages"][0]["content"][0]
                .get("cache_control")
                .is_some()
        );
        assert!(
            body["messages"][0]["content"][1]
                .get("cache_control")
                .is_some()
        );
    }

    // =============================================================================
    // R12/R13 — TTL ordering invariant enforcement (auto-upgrade strategy)
    // =============================================================================
    //
    // Anthropic enforces "longer TTL must NOT come after shorter TTL" across
    // the wire stream `tools → system → messages`. The R10 fix made
    // `apply_cache_control` respect `cc.ttl` so all markers it adds use the
    // same TTL — that closes the most common path. The R12 normalizer is the
    // structural backstop, and the R13 strategy switch (drop → auto-upgrade)
    // makes it preserve all cache breakpoints by upgrading earlier markers'
    // TTLs to match later markers' longer TTLs instead of dropping the
    // longer markers.

    #[test]
    fn normalize_cache_ttl_ordering_no_op_when_all_same_ttl() {
        let mut body = json!({
            "tools": [{"name": "t0", "cache_control": {"type": "ephemeral", "ttl": "1h"}}],
            "system": [
                {"type": "text", "text": "s0", "cache_control": {"type": "ephemeral", "ttl": "1h"}},
                {"type": "text", "text": "s1", "cache_control": {"type": "ephemeral", "ttl": "1h"}}
            ],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "u0", "cache_control": {"type": "ephemeral", "ttl": "1h"}}
                ]}
            ]
        });
        assert_eq!(normalize_cache_ttl_ordering(&mut body), 0);
        // All markers preserved with their original TTL.
        assert_eq!(body["tools"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(body["system"][1]["cache_control"]["ttl"], "1h");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["ttl"],
            "1h"
        );
    }

    #[test]
    fn normalize_cache_ttl_ordering_no_op_when_monotonically_non_increasing() {
        // 1h → 1h → 5m → 5m is valid (each marker's TTL ≤ previous).
        let mut body = json!({
            "tools": [{"name": "t0", "cache_control": {"type": "ephemeral", "ttl": "1h"}}],
            "system": [
                {"type": "text", "text": "s0", "cache_control": {"type": "ephemeral", "ttl": "1h"}},
                {"type": "text", "text": "s1", "cache_control": {"type": "ephemeral", "ttl": "5m"}}
            ],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "u0", "cache_control": {"type": "ephemeral", "ttl": "5m"}}
                ]}
            ]
        });
        assert_eq!(normalize_cache_ttl_ordering(&mut body), 0);
    }

    #[test]
    fn normalize_cache_ttl_ordering_upgrades_earlier_markers_in_system() {
        // tools=5m, system[0..2]=1h ← violation (1h after 5m).
        // R13 strategy: upgrade tools to 1h instead of dropping system markers.
        // Result: tools=1h, system[0..2]=1h. ALL markers preserved.
        let mut body = json!({
            "tools": [{"name": "t0", "cache_control": {"type": "ephemeral", "ttl": "5m"}}],
            "system": [
                {"type": "text", "text": "s0", "cache_control": {"type": "ephemeral", "ttl": "1h"}},
                {"type": "text", "text": "s1", "cache_control": {"type": "ephemeral", "ttl": "1h"}}
            ],
            "messages": []
        });
        let upgraded = normalize_cache_ttl_ordering(&mut body);
        assert_eq!(upgraded, 1, "only tools[0] should be upgraded");
        assert_eq!(body["tools"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(body["system"][1]["cache_control"]["ttl"], "1h");
    }

    #[test]
    fn normalize_cache_ttl_ordering_upgrades_earlier_markers_in_messages() {
        // system=5m, messages[0]=1h ← violation.
        // Upgrade system to 1h. All markers preserved.
        let mut body = json!({
            "system": [{"type": "text", "text": "s0", "cache_control": {"type": "ephemeral", "ttl": "5m"}}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "u0", "cache_control": {"type": "ephemeral", "ttl": "1h"}}
                ]}
            ]
        });
        let upgraded = normalize_cache_ttl_ordering(&mut body);
        assert_eq!(upgraded, 1);
        assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["ttl"],
            "1h"
        );
    }

    #[test]
    fn normalize_cache_ttl_ordering_treats_missing_ttl_as_default_5m() {
        // No-ttl markers default to 5m on the API side. A 1h marker AFTER
        // a no-ttl marker is still a violation; auto-upgrade rewrites the
        // earlier no-ttl marker with an explicit "1h" so the wire stream
        // is monotonically non-increasing.
        let mut body = json!({
            "tools": [{"name": "t0", "cache_control": {"type": "ephemeral"}}],
            "system": [{"type": "text", "text": "s0", "cache_control": {"type": "ephemeral", "ttl": "1h"}}]
        });
        let upgraded = normalize_cache_ttl_ordering(&mut body);
        assert_eq!(upgraded, 1);
        assert_eq!(body["tools"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h");
    }

    #[test]
    fn normalize_cache_ttl_ordering_no_op_when_all_no_ttl() {
        // All markers without an explicit TTL (= all default to 5m). Valid.
        let mut body = json!({
            "tools": [{"name": "t0", "cache_control": {"type": "ephemeral"}}],
            "system": [{"type": "text", "text": "s0", "cache_control": {"type": "ephemeral"}}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "u0", "cache_control": {"type": "ephemeral"}}
                ]}
            ]
        });
        assert_eq!(normalize_cache_ttl_ordering(&mut body), 0);
        // Markers untouched — none of them have a `ttl` field.
        assert!(body["tools"][0]["cache_control"].get("ttl").is_none());
        assert!(body["system"][0]["cache_control"].get("ttl").is_none());
    }

    #[test]
    fn normalize_cache_ttl_ordering_handles_multiple_violations() {
        // Multiple violations across the stream. All earlier short-TTL
        // markers should be upgraded to the longest later TTL.
        // tools=5m, system[0]=5m, system[1]=1h, messages[0]=5m
        // Walking reverse: msg=5m → max=5m. system[1]=1h>5m → max=1h.
        //   system[0]=5m<1h → upgrade. tools=5m<1h → upgrade.
        // Result: 1h, 1h, 1h, 5m (forward, valid non-increasing).
        let mut body = json!({
            "tools": [{"name": "t0", "cache_control": {"type": "ephemeral", "ttl": "5m"}}],
            "system": [
                {"type": "text", "text": "s0", "cache_control": {"type": "ephemeral", "ttl": "5m"}},
                {"type": "text", "text": "s1", "cache_control": {"type": "ephemeral", "ttl": "1h"}}
            ],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "u0", "cache_control": {"type": "ephemeral", "ttl": "5m"}}
                ]}
            ]
        });
        let upgraded = normalize_cache_ttl_ordering(&mut body);
        assert_eq!(upgraded, 2, "tools[0] and system[0] should be upgraded");
        assert_eq!(body["tools"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(body["system"][1]["cache_control"]["ttl"], "1h");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["ttl"],
            "5m"
        );
    }

    #[test]
    fn ttl_to_seconds_known_and_future_values() {
        // Known values (currently accepted by Anthropic).
        assert_eq!(ttl_to_seconds(None), 300);
        assert_eq!(ttl_to_seconds(Some("5m")), 300);
        assert_eq!(ttl_to_seconds(Some("1h")), 3600);

        // Generic parser handles forms Anthropic might add in the future
        // without code changes here. The unit codes are SI-style: s, m,
        // h, d.
        assert_eq!(ttl_to_seconds(Some("30s")), 30);
        assert_eq!(ttl_to_seconds(Some("10m")), 600);
        assert_eq!(ttl_to_seconds(Some("2h")), 7200);
        assert_eq!(ttl_to_seconds(Some("1d")), 86_400);

        // Malformed input falls back to the API default (5m). We don't
        // error here because the API itself will reject unknown TTLs
        // with a clearer error than anything we could produce locally.
        assert_eq!(ttl_to_seconds(Some("garbage")), 300);
        assert_eq!(ttl_to_seconds(Some("")), 300);
        assert_eq!(ttl_to_seconds(Some("h")), 300); // no number
        assert_eq!(ttl_to_seconds(Some("5")), 300); // no unit
    }

    /// R14 — `ttl_to_seconds` must be **panic-free for any UTF-8 input**,
    /// not just ASCII. The R13 implementation used
    /// `s.split_at(s.len() - 1)`, which panics when the trailing byte
    /// falls in the middle of a multi-byte char. Defensive codecs must
    /// never panic on user-supplied strings — even unconventional ones.
    #[test]
    fn ttl_to_seconds_does_not_panic_on_non_ascii_input() {
        // Korean "5분" — '분' is U+BD84, encoded as 3 UTF-8 bytes.
        // Pre-fix `split_at(s.len()-1)` would split mid-byte → panic.
        // Post-fix returns the default since '분' isn't a recognised unit.
        assert_eq!(ttl_to_seconds(Some("5분")), 300);

        // Emoji clock — U+1F550 is 4 UTF-8 bytes. Same pre-fix panic.
        assert_eq!(ttl_to_seconds(Some("5🕐")), 300);

        // Multi-byte char alone (no leading number).
        assert_eq!(ttl_to_seconds(Some("분")), 300);
        assert_eq!(ttl_to_seconds(Some("🕐")), 300);

        // Mixed: number + multi-byte non-unit. Still no panic.
        assert_eq!(ttl_to_seconds(Some("123🕐")), 300);

        // Sanity: the ASCII path still works after the rewrite.
        assert_eq!(ttl_to_seconds(Some("1h")), 3600);
        assert_eq!(ttl_to_seconds(Some("30s")), 30);
    }

    /// R15 — `ttl_to_seconds` must be **panic-free on arithmetic
    /// overflow**, not just on UTF-8 boundaries. The R13/R14
    /// implementation used raw `n * 86_400` which panics in debug
    /// builds when `n` is large enough that the multiplication
    /// overflows `u64::MAX` (≈ 1.84e19). For unit `'d'` the threshold
    /// is `u64::MAX / 86_400 ≈ 2.13e14` days — easily reachable with
    /// a malicious or copy-paste-typoed input like
    /// `"99999999999999999d"` (1.7e17). Defensive code must not panic
    /// on any parseable input.
    ///
    /// Fix is `saturating_mul`, which clamps to `u64::MAX` instead of
    /// panicking. Saturated values are still strictly larger than any
    /// realistic Anthropic TTL, so the ordering comparison in
    /// `normalize_cache_ttl_ordering` keeps producing the right answer.
    #[test]
    fn ttl_to_seconds_does_not_panic_on_arithmetic_overflow() {
        // Days unit — overflow threshold ≈ 2.13e14. 1e17 well exceeds.
        assert_eq!(
            ttl_to_seconds(Some("99999999999999999d")),
            u64::MAX,
            "huge day count must saturate, not panic"
        );

        // Hours unit — overflow threshold ≈ 5.12e15.
        assert_eq!(ttl_to_seconds(Some("99999999999999999h")), u64::MAX);

        // Minutes unit — overflow threshold ≈ 3.07e17.
        assert_eq!(ttl_to_seconds(Some("999999999999999999m")), u64::MAX);

        // Seconds unit cannot overflow (n * 1 = n, always fits in u64).
        assert_eq!(ttl_to_seconds(Some("18446744073709551615s")), u64::MAX);

        // Realistic-but-large values still produce exact results.
        assert_eq!(ttl_to_seconds(Some("1000d")), 1000 * 86_400);
        assert_eq!(ttl_to_seconds(Some("8760h")), 8760 * 3600); // 1 year in hours
    }

    /// R10-fix-4 — `apply_cache_control` propagates `cc.ttl` to all marker
    /// positions so the request stream is monotonically non-increasing in
    /// TTL (Anthropic API enforces this).
    #[test]
    fn apply_cache_control_propagates_ttl_to_all_positions() {
        let mut body = json!({
            "system": [{"type": "text", "text": "sys"}],
            "tools": [{"name": "calc"}],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
        });
        let cc = crate::ir::CacheControl {
            system: true,
            tools: true,
            conversation: true,
            ttl: Some("1h".into()),
        };
        apply_cache_control(&mut body, &cc);
        assert_eq!(body["system"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(body["tools"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["ttl"],
            "1h"
        );
    }

    #[test]
    fn apply_cache_control_omits_ttl_when_none() {
        let mut body = json!({
            "system": [{"type": "text", "text": "sys"}],
        });
        let cc = crate::ir::CacheControl {
            system: true,
            tools: false,
            conversation: false,
            ttl: None,
        };
        apply_cache_control(&mut body, &cc);
        // No `ttl` field — defaults to 5m on the API side.
        assert!(body["system"][0]["cache_control"].get("ttl").is_none());
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    }

    /// R10-fix-5 — `decode_usage` reconstructs total `input_tokens` from
    /// fresh + cache_read + cache_creation so cached stays a subset (the
    /// IR invariant). The previous behaviour panicked under heavy caching
    /// with `cached(5253) > input(2)`.
    #[test]
    fn decode_usage_heavy_cache_reconstruct_total() {
        // Simulates the exact live-API failure observed during R10:
        // fresh=2, cache_read=5253. Pre-fix: input=2, cached=5253 → panic.
        // Post-fix: input=5255, cached=5253, billable=2.
        let usage = json!({
            "input_tokens": 2,
            "output_tokens": 7,
            "cache_read_input_tokens": 5253
        });
        let u = decode_usage(&usage);
        assert_eq!(u.input_tokens, 5255);
        assert_eq!(u.cached_input_tokens, Some(5253));
        assert_eq!(u.billable_input_tokens(), 2);

        // Sanity-check: feeding through `Usage::add` must not panic on
        // the IR invariant `cached <= input`.
        let mut acc = crate::ir::Usage::default();
        acc.add(&u);
        assert_eq!(acc.input_tokens, 5255);
    }

    #[test]
    fn decode_usage_cache_read_plus_creation_both_summed() {
        let usage = json!({
            "input_tokens": 10,
            "output_tokens": 20,
            "cache_read_input_tokens": 100,
            "cache_creation_input_tokens": 50
        });
        let u = decode_usage(&usage);
        assert_eq!(u.input_tokens, 160);
        assert_eq!(u.cached_input_tokens, Some(100));
        assert_eq!(u.cache_creation_tokens, Some(50));
        assert_eq!(u.billable_input_tokens(), 60);
    }

    #[test]
    fn decode_usage_no_cache_fields_unaffected() {
        // No cache fields → behaves identically to the pre-fix decoder.
        let usage = json!({
            "input_tokens": 100,
            "output_tokens": 50
        });
        let u = decode_usage(&usage);
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cached_input_tokens, None);
        assert_eq!(u.cache_creation_tokens, None);
    }

    #[test]
    fn decode_message_delta_streaming_reconstructs_total() {
        // Streaming variant must apply the same total-input reconstruction.
        let v = json!({
            "delta": {"stop_reason": "end_turn"},
            "usage": {
                "input_tokens": 5,
                "output_tokens": 10,
                "cache_read_input_tokens": 200
            }
        });
        let chunks = decode_message_delta(&v);
        // Two chunks: a UsageDelta and a Finish.
        let usage_delta = chunks.iter().find_map(|c| match c {
            ModelStreamChunk::UsageDelta(u) => Some(u),
            _ => None,
        });
        let pu = usage_delta.expect("UsageDelta chunk emitted");
        assert_eq!(pu.input_tokens, Some(205));
        assert_eq!(pu.cached_input_tokens, Some(200));
    }
}
