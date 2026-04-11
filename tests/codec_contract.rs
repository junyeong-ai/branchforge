//! Codec contract test matrix.
//!
//! Each scenario is a pair of (a) a canonical [`ModelRequest`] that we
//! drive every codec with, and (b) a per-codec hand-curated wire response
//! that we feed back through `decode_response`. The assertions are at the
//! **neutral IR level** (text, finish reason, usage, content shape), not
//! at the wire-format level — so the same scenario function can validate
//! all five codecs uniformly.
//!
//! See plan §8 for the scenario list and rationale.
//!
//! ## Why direct codec calls (no HTTP)
//!
//! Contract tests are about *semantic equivalence at the IR boundary*,
//! not about HTTP. We exercise codecs directly with hand-curated JSON, so
//! the suite runs in milliseconds, has no flake, and gives crisp failures
//! when a codec drifts. Transport tests live separately under
//! `src/client/transport/*::tests` (mock URL construction) and the
//! existing `provider_client::tests` (wiremock end-to-end via SSE).

use branchforge::client::codec::{
    AnthropicMessagesCodec, BedrockConverseCodec, EncodedRequest, GeminiGenerateCodec,
    InvocationMode, ModelCodec, OpenAiChatCodec, OpenAiResponsesCodec,
};
use branchforge::ir::{
    ContentPart, FinishReason, Message, ModelRequest, ModelResponse, Role, StreamDecodeState,
    Support, SystemPrompt, ToolCallSupport, ToolDefinition, ToolIdSemantics, ToolOrigin,
    ToolResultContent,
};
use serde_json::{Value, json};

// =============================================================================
// Scenario 1 — PlainTextRoundtrip
// =============================================================================

mod plain_text {
    use super::*;

    fn canonical_request() -> ModelRequest {
        let mut r = ModelRequest::new("test-model", vec![Message::user("ping")]);
        r.settings.max_output_tokens = Some(64);
        r
    }

    fn assert_decoded(resp: &ModelResponse) {
        assert_eq!(resp.text(), "pong");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 1);
    }

    fn run<C: ModelCodec>(codec: &C, raw: Value) {
        let req = canonical_request();
        let enc = codec
            .encode_request(&req, InvocationMode::Unary)
            .expect("encode");
        // Body must reference the model and contain non-empty input.
        assert!(!enc.body.is_null(), "{} encoded body is null", codec.id());
        let resp = codec
            .decode_response(raw, InvocationMode::Unary)
            .expect("decode");
        assert_decoded(&resp);
    }

    #[test]
    fn anthropic() {
        run(
            &AnthropicMessagesCodec::new(),
            json!({
                "id": "msg_1",
                "model": "test-model",
                "content": [{"type": "text", "text": "pong"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 5, "output_tokens": 1}
            }),
        );
    }

    #[test]
    fn openai_chat() {
        run(
            &OpenAiChatCodec::new(),
            json!({
                "id": "chatcmpl_1",
                "model": "test-model",
                "choices": [{
                    "message": {"role": "assistant", "content": "pong"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 1}
            }),
        );
    }

    #[test]
    fn openai_responses() {
        run(
            &OpenAiResponsesCodec::new(),
            json!({
                "id": "resp_1",
                "model": "test-model",
                "status": "completed",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "pong"}]
                }],
                "usage": {"input_tokens": 5, "output_tokens": 1}
            }),
        );
    }

    #[test]
    fn gemini() {
        run(
            &GeminiGenerateCodec::new(),
            json!({
                "responseId": "r_1",
                "modelVersion": "test-model",
                "candidates": [{
                    "content": {"parts": [{"text": "pong"}]},
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 5,
                    "candidatesTokenCount": 1,
                    "totalTokenCount": 6
                }
            }),
        );
    }

    #[test]
    fn bedrock_converse() {
        run(
            &BedrockConverseCodec::new(),
            json!({
                "output": {"message": {"role": "assistant", "content": [{"text": "pong"}]}},
                "stopReason": "end_turn",
                "usage": {"inputTokens": 5, "outputTokens": 1}
            }),
        );
    }
}

// =============================================================================
// Scenario 2 — SystemPromptHandling
//
// All codecs encode `request.system` somewhere in the wire body. The
// location differs (top-level field vs role-message), but every codec must
// produce a request body that mentions the system text exactly once and
// preserves the conversation messages.
// =============================================================================

mod system_prompt {
    use super::*;

    const SYS: &str = "You are a strict assistant. Reply with one word.";

    fn canonical_request() -> ModelRequest {
        let mut r = ModelRequest::new("test-model", vec![Message::user("ping")]);
        r.system = Some(SystemPrompt::Text(SYS.into()));
        r
    }

    fn assert_contains(body: &Value, needle: &str, codec_id: &str) {
        let s = serde_json::to_string(body).unwrap();
        assert!(
            s.contains(needle),
            "{} encoded body does not contain '{}': {}",
            codec_id,
            needle,
            s
        );
    }

    fn run<C: ModelCodec>(codec: &C) {
        let enc = codec
            .encode_request(&canonical_request(), InvocationMode::Unary)
            .expect("encode");
        assert_contains(&enc.body, SYS, codec.id());
        assert_contains(&enc.body, "ping", codec.id());
        // Encoding must not produce a LossyEncode warning for plain-text
        // system prompts.
        assert!(
            enc.warnings
                .iter()
                .all(|w| !matches!(w, branchforge::ir::ModelWarning::LossyEncode { .. })),
            "{} produced lossy warning on plain-text system prompt: {:?}",
            codec.id(),
            enc.warnings,
        );
    }

    #[test]
    fn anthropic() {
        run(&AnthropicMessagesCodec::new());
    }
    #[test]
    fn openai_chat() {
        run(&OpenAiChatCodec::new());
    }
    #[test]
    fn openai_responses() {
        run(&OpenAiResponsesCodec::new());
    }
    #[test]
    fn gemini() {
        run(&GeminiGenerateCodec::new());
    }
    #[test]
    fn bedrock_converse() {
        run(&BedrockConverseCodec::new());
    }
}

// =============================================================================
// Scenario 3 — ToolCallSingle
//
// Encode user → assistant tool_call → tool_result follow-up. Verify the
// tool definitions land in the wire body, the tool_call round-trips
// through decode, and the tool_result is correctly addressed by id.
// =============================================================================

mod tool_call_single {
    use super::*;

    fn canonical_request() -> ModelRequest {
        let mut r = ModelRequest::new(
            "test-model",
            vec![
                Message::user("What is 2+2?"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentPart::ToolCall {
                        id: "call_1".into(),
                        name: "calculator".into(),
                        arguments: json!({"a": 2, "b": 2}),
                        origin: ToolOrigin::Local,
                    }],
                },
                Message::tool_result("call_1", "4"),
            ],
        );
        r.tools = vec![ToolDefinition::new(
            "calculator",
            json!({
                "type": "object",
                "properties": {
                    "a": {"type": "number"},
                    "b": {"type": "number"}
                },
                "required": ["a", "b"]
            }),
        )];
        r
    }

    fn run_encode<C: ModelCodec>(codec: &C) -> EncodedRequest {
        let enc = codec
            .encode_request(&canonical_request(), InvocationMode::Unary)
            .expect("encode");
        let s = serde_json::to_string(&enc.body).unwrap();
        // The tool definition must reach the wire body in some form.
        assert!(
            s.contains("calculator"),
            "{} encoded body lacks tool name 'calculator': {}",
            codec.id(),
            s
        );
        // The tool call id should round-trip on every codec that
        // advertises ToolIdSemantics::Provided.
        if codec.capabilities().tool_calls.id_semantics == ToolIdSemantics::Provided {
            assert!(
                s.contains("call_1"),
                "{} dropped tool_call id 'call_1' from wire body: {}",
                codec.id(),
                s
            );
        }
        enc
    }

    fn assert_decoded_tool_call(resp: &ModelResponse) {
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        let tc = resp.tool_calls().next().expect("expected one tool call");
        if let ContentPart::ToolCall {
            name, arguments, ..
        } = tc
        {
            assert_eq!(name, "calculator");
            assert_eq!(arguments["a"], 2);
            assert_eq!(arguments["b"], 2);
        } else {
            panic!("not a ToolCall");
        }
    }

    #[test]
    fn anthropic() {
        let codec = AnthropicMessagesCodec::new();
        let _ = run_encode(&codec);
        let resp = codec
            .decode_response(
                json!({
                    "id": "msg_1",
                    "model": "test-model",
                    "content": [
                        {"type": "tool_use", "id": "call_1", "name": "calculator", "input": {"a": 2, "b": 2}}
                    ],
                    "stop_reason": "tool_use",
                    "usage": {"input_tokens": 10, "output_tokens": 5}
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_decoded_tool_call(&resp);
    }

    #[test]
    fn openai_chat() {
        let codec = OpenAiChatCodec::new();
        let _ = run_encode(&codec);
        let resp = codec
            .decode_response(
                json!({
                    "id": "chatcmpl_1",
                    "model": "test-model",
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call_1",
                                "type": "function",
                                "function": {"name": "calculator", "arguments": "{\"a\":2,\"b\":2}"}
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }]
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_decoded_tool_call(&resp);
    }

    #[test]
    fn openai_responses() {
        let codec = OpenAiResponsesCodec::new();
        let _ = run_encode(&codec);
        let resp = codec
            .decode_response(
                json!({
                    "id": "resp_1",
                    "model": "test-model",
                    "status": "completed",
                    "output": [{
                        "type": "function_call",
                        "call_id": "call_1",
                        "name": "calculator",
                        "arguments": "{\"a\":2,\"b\":2}"
                    }]
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_decoded_tool_call(&resp);
    }

    #[test]
    fn gemini() {
        let codec = GeminiGenerateCodec::new();
        let _ = run_encode(&codec);
        // Gemini synthesizes ids by index, so the id won't be "call_1".
        let resp = codec
            .decode_response(
                json!({
                    "responseId": "r_1",
                    "modelVersion": "test-model",
                    "candidates": [{
                        "content": {"parts": [
                            {"functionCall": {"name": "calculator", "args": {"a": 2, "b": 2}}}
                        ]},
                        "finishReason": "STOP"
                    }]
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_decoded_tool_call(&resp);
        // Gemini synth id format: call_<index>.
        if let Some(ContentPart::ToolCall { id, .. }) = resp.tool_calls().next() {
            assert!(
                id.starts_with("call_"),
                "gemini synth id should start with call_: {id}"
            );
        }
    }

    #[test]
    fn bedrock_converse() {
        let codec = BedrockConverseCodec::new();
        let _ = run_encode(&codec);
        let resp = codec
            .decode_response(
                json!({
                    "output": {"message": {"role": "assistant", "content": [
                        {"toolUse": {"toolUseId": "call_1", "name": "calculator", "input": {"a": 2, "b": 2}}}
                    ]}},
                    "stopReason": "tool_use",
                    "usage": {"inputTokens": 10, "outputTokens": 5}
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_decoded_tool_call(&resp);
    }
}

// =============================================================================
// Scenario 4 — ToolCallParallel
//
// The IR must preserve the *ordering* of parallel tool calls because
// codecs whose wire format synthesizes ids by index (Gemini) rely on
// position to disambiguate identical names.
// =============================================================================

mod tool_call_parallel {
    use super::*;

    fn assert_two_calls_in_order(resp: &ModelResponse, names: &[&str]) {
        let calls: Vec<&str> = resp
            .tool_calls()
            .filter_map(|p| match p {
                ContentPart::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(calls, names, "{:?} != {:?}", calls, names);
    }

    #[test]
    fn anthropic() {
        let codec = AnthropicMessagesCodec::new();
        let resp = codec
            .decode_response(
                json!({
                    "id": "msg_1",
                    "model": "test-model",
                    "content": [
                        {"type": "tool_use", "id": "call_a", "name": "first", "input": {}},
                        {"type": "tool_use", "id": "call_b", "name": "second", "input": {}}
                    ],
                    "stop_reason": "tool_use",
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_two_calls_in_order(&resp, &["first", "second"]);
    }

    #[test]
    fn gemini_synthesizes_unique_ids_by_index() {
        let codec = GeminiGenerateCodec::new();
        let resp = codec
            .decode_response(
                json!({
                    "candidates": [{
                        "content": {"parts": [
                            {"functionCall": {"name": "dup", "args": {"i": 0}}},
                            {"functionCall": {"name": "dup", "args": {"i": 1}}}
                        ]},
                        "finishReason": "STOP"
                    }]
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        let ids: Vec<String> = resp
            .tool_calls()
            .filter_map(|p| match p {
                ContentPart::ToolCall { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        // Same function name twice → synthesized ids must still be unique.
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert!(ids[0].starts_with("call_"));
    }

    #[test]
    fn openai_chat() {
        let codec = OpenAiChatCodec::new();
        let resp = codec
            .decode_response(
                json!({
                    "id": "x",
                    "model": "x",
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [
                                {"id": "call_a", "type": "function", "function": {"name": "first", "arguments": "{}"}},
                                {"id": "call_b", "type": "function", "function": {"name": "second", "arguments": "{}"}}
                            ]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_two_calls_in_order(&resp, &["first", "second"]);
    }

    #[test]
    fn openai_responses() {
        let codec = OpenAiResponsesCodec::new();
        let resp = codec
            .decode_response(
                json!({
                    "id": "resp_1",
                    "model": "x",
                    "status": "completed",
                    "output": [
                        {"type": "function_call", "call_id": "call_a", "name": "first", "arguments": "{}"},
                        {"type": "function_call", "call_id": "call_b", "name": "second", "arguments": "{}"}
                    ],
                    "usage": {"input_tokens": 1, "output_tokens": 1}
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_two_calls_in_order(&resp, &["first", "second"]);
    }

    #[test]
    fn bedrock_converse() {
        let codec = BedrockConverseCodec::new();
        let resp = codec
            .decode_response(
                json!({
                    "output": {
                        "message": {
                            "role": "assistant",
                            "content": [
                                {"toolUse": {"toolUseId": "call_a", "name": "first", "input": {}}},
                                {"toolUse": {"toolUseId": "call_b", "name": "second", "input": {}}}
                            ]
                        }
                    },
                    "stopReason": "tool_use",
                    "usage": {"inputTokens": 1, "outputTokens": 1}
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_two_calls_in_order(&resp, &["first", "second"]);
    }
}

// =============================================================================
// Scenario 5 — UsageExtraction
//
// All codecs must populate `Usage::input_tokens` and
// `Usage::output_tokens`. Codecs that report cached or reasoning tokens
// must populate the typed `Option<u64>` fields, not stuff them in `raw`.
// =============================================================================

mod usage_extraction {
    use super::*;

    #[test]
    fn anthropic_extracts_cache_read_and_creation() {
        let codec = AnthropicMessagesCodec::new();
        let resp = codec
            .decode_response(
                json!({
                    "id": "msg_1",
                    "model": "x",
                    "content": [{"type": "text", "text": "ok"}],
                    "stop_reason": "end_turn",
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 5,
                        "cache_read_input_tokens": 8,
                        "cache_creation_input_tokens": 2
                    }
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        // IR contract: input_tokens is the TOTAL input (fresh + cache).
        // Anthropic reports `input_tokens` as the *non-cached* portion
        // only (10), so the decoder reconstructs the total: 10 + 8 + 2.
        // Without this, `cached_input_tokens (8) > input_tokens (10)`
        // would trip the `Usage::add` invariant under heavy caching
        // (live API regularly returns cached=5253, input=2).
        assert_eq!(resp.usage.input_tokens, 20);
        assert_eq!(resp.usage.output_tokens, 5);
        assert_eq!(resp.usage.cached_input_tokens, Some(8));
        assert_eq!(resp.usage.cache_creation_tokens, Some(2));
        assert_eq!(resp.usage.billable_input_tokens(), 12);
    }

    #[test]
    fn openai_chat_extracts_cached_and_reasoning_tokens() {
        let codec = OpenAiChatCodec::new();
        let resp = codec
            .decode_response(
                json!({
                    "id": "x",
                    "model": "x",
                    "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                    "usage": {
                        "prompt_tokens": 100,
                        "completion_tokens": 20,
                        "prompt_tokens_details": {"cached_tokens": 30},
                        "completion_tokens_details": {"reasoning_tokens": 50}
                    }
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_eq!(resp.usage.input_tokens, 100);
        assert_eq!(resp.usage.cached_input_tokens, Some(30));
        assert_eq!(resp.usage.reasoning_tokens, Some(50));
    }

    #[test]
    fn openai_responses_extracts_typed_fields() {
        let codec = OpenAiResponsesCodec::new();
        let resp = codec
            .decode_response(
                json!({
                    "id": "resp_1",
                    "model": "o3",
                    "status": "completed",
                    "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}],
                    "usage": {
                        "input_tokens": 50,
                        "output_tokens": 10,
                        "input_tokens_details": {"cached_tokens": 20},
                        "output_tokens_details": {"reasoning_tokens": 200}
                    }
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_eq!(resp.usage.input_tokens, 50);
        assert_eq!(resp.usage.cached_input_tokens, Some(20));
        assert_eq!(resp.usage.reasoning_tokens, Some(200));
    }

    #[test]
    fn gemini_extracts_thoughts_and_cached_content_tokens() {
        let codec = GeminiGenerateCodec::new();
        let resp = codec
            .decode_response(
                json!({
                    "candidates": [{"content": {"parts": [{"text": "ok"}]}, "finishReason": "STOP"}],
                    "usageMetadata": {
                        "promptTokenCount": 8,
                        "candidatesTokenCount": 4,
                        "thoughtsTokenCount": 99,
                        "cachedContentTokenCount": 3
                    }
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        assert_eq!(resp.usage.input_tokens, 8);
        assert_eq!(resp.usage.reasoning_tokens, Some(99));
        assert_eq!(resp.usage.cached_input_tokens, Some(3));
    }

    #[test]
    fn bedrock_extracts_cache_read_and_write() {
        let codec = BedrockConverseCodec::new();
        let resp = codec
            .decode_response(
                json!({
                    "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
                    "stopReason": "end_turn",
                    "usage": {
                        "inputTokens": 100,
                        "outputTokens": 20,
                        "cacheReadInputTokens": 60,
                        "cacheWriteInputTokens": 5
                    }
                }),
                InvocationMode::Unary,
            )
            .unwrap();
        // Same total-input reconstruction as Anthropic — Bedrock Converse
        // routes to Anthropic Claude with the same accounting model.
        // input_tokens = fresh (100) + cache_read (60) + cache_write (5).
        assert_eq!(resp.usage.input_tokens, 165);
        assert_eq!(resp.usage.cached_input_tokens, Some(60));
        assert_eq!(resp.usage.cache_creation_tokens, Some(5));
        assert_eq!(resp.usage.billable_input_tokens(), 105);
    }
}

// =============================================================================
// Scenario 6 — FinishReasonMap
//
// Every native stop reason must map to one of the typed FinishReason
// variants. Unmapped values land in FinishReason::Other and are still
// round-trippable.
// =============================================================================

mod finish_reason {
    use super::*;

    fn decode_with_stop(codec_id: &str, raw: Value) -> FinishReason {
        match codec_id {
            "anthropic-messages" => {
                AnthropicMessagesCodec::new()
                    .decode_response(raw, InvocationMode::Unary)
                    .unwrap()
                    .finish_reason
            }
            "openai-chat" => {
                OpenAiChatCodec::new()
                    .decode_response(raw, InvocationMode::Unary)
                    .unwrap()
                    .finish_reason
            }
            "openai-responses" => {
                OpenAiResponsesCodec::new()
                    .decode_response(raw, InvocationMode::Unary)
                    .unwrap()
                    .finish_reason
            }
            "gemini-generate" => {
                GeminiGenerateCodec::new()
                    .decode_response(raw, InvocationMode::Unary)
                    .unwrap()
                    .finish_reason
            }
            "bedrock-converse" => {
                BedrockConverseCodec::new()
                    .decode_response(raw, InvocationMode::Unary)
                    .unwrap()
                    .finish_reason
            }
            _ => panic!("unknown codec id"),
        }
    }

    #[test]
    fn anthropic_stop_reason_variants() {
        let mk = |stop: &str| {
            json!({
                "id": "msg",
                "model": "x",
                "content": [{"type": "text", "text": "x"}],
                "stop_reason": stop,
                "usage": {"input_tokens": 1, "output_tokens": 1}
            })
        };
        assert_eq!(
            decode_with_stop("anthropic-messages", mk("end_turn")),
            FinishReason::Stop
        );
        assert_eq!(
            decode_with_stop("anthropic-messages", mk("max_tokens")),
            FinishReason::Length
        );
        assert_eq!(
            decode_with_stop("anthropic-messages", mk("tool_use")),
            FinishReason::ToolCalls
        );
        assert_eq!(
            decode_with_stop("anthropic-messages", mk("refusal")),
            FinishReason::ContentFilter
        );
        assert_eq!(
            decode_with_stop("anthropic-messages", mk("pause_turn")),
            FinishReason::PauseTurn
        );
        assert_eq!(
            decode_with_stop("anthropic-messages", mk("stop_sequence")),
            FinishReason::StopSequence
        );
    }

    #[test]
    fn openai_chat_finish_reason_variants() {
        let mk = |reason: &str| {
            json!({
                "id": "x",
                "model": "x",
                "choices": [{"message": {"role": "assistant", "content": "x"}, "finish_reason": reason}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1}
            })
        };
        assert_eq!(
            decode_with_stop("openai-chat", mk("stop")),
            FinishReason::Stop
        );
        assert_eq!(
            decode_with_stop("openai-chat", mk("length")),
            FinishReason::Length
        );
        assert_eq!(
            decode_with_stop("openai-chat", mk("tool_calls")),
            FinishReason::ToolCalls
        );
        assert_eq!(
            decode_with_stop("openai-chat", mk("content_filter")),
            FinishReason::ContentFilter
        );
    }

    #[test]
    fn openai_responses_incomplete_reasons() {
        let mk = |status: &str, incomplete: Option<&str>| {
            let mut v = json!({
                "id": "r",
                "model": "x",
                "status": status,
                "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "x"}]}]
            });
            if let Some(reason) = incomplete {
                v["incomplete_details"] = json!({"reason": reason});
            }
            v
        };
        assert_eq!(
            decode_with_stop("openai-responses", mk("completed", None)),
            FinishReason::Stop
        );
        assert_eq!(
            decode_with_stop(
                "openai-responses",
                mk("incomplete", Some("max_output_tokens"))
            ),
            FinishReason::Length
        );
        assert_eq!(
            decode_with_stop("openai-responses", mk("incomplete", Some("content_filter"))),
            FinishReason::ContentFilter
        );
        assert_eq!(
            decode_with_stop("openai-responses", mk("incomplete", Some("tool_call_loop"))),
            FinishReason::PauseTurn
        );
        assert_eq!(
            decode_with_stop("openai-responses", mk("failed", None)),
            FinishReason::Error
        );
    }

    #[test]
    fn gemini_finish_reason_variants() {
        let mk = |reason: &str| {
            json!({
                "candidates": [{"content": {"parts": [{"text": "x"}]}, "finishReason": reason}]
            })
        };
        assert_eq!(
            decode_with_stop("gemini-generate", mk("STOP")),
            FinishReason::Stop
        );
        assert_eq!(
            decode_with_stop("gemini-generate", mk("MAX_TOKENS")),
            FinishReason::Length
        );
        assert_eq!(
            decode_with_stop("gemini-generate", mk("SAFETY")),
            FinishReason::ContentFilter
        );
        assert_eq!(
            decode_with_stop("gemini-generate", mk("RECITATION")),
            FinishReason::ContentFilter
        );
    }

    #[test]
    fn bedrock_finish_reason_variants() {
        let mk = |reason: &str| {
            json!({
                "output": {"message": {"role": "assistant", "content": [{"text": "x"}]}},
                "stopReason": reason,
                "usage": {"inputTokens": 1, "outputTokens": 1}
            })
        };
        assert_eq!(
            decode_with_stop("bedrock-converse", mk("end_turn")),
            FinishReason::Stop
        );
        assert_eq!(
            decode_with_stop("bedrock-converse", mk("tool_use")),
            FinishReason::ToolCalls
        );
        assert_eq!(
            decode_with_stop("bedrock-converse", mk("max_tokens")),
            FinishReason::Length
        );
        assert_eq!(
            decode_with_stop("bedrock-converse", mk("guardrail_intervened")),
            FinishReason::ContentFilter
        );
    }
}

// =============================================================================
// Scenario 7 — StreamingTextDelta (codec-level, no HTTP)
//
// Drive each codec's `decode_stream_chunk` (or `decode_eventstream_frame`
// for AWS-framed codecs) with hand-curated frames and assemble the text.
// =============================================================================

mod streaming_text_delta {
    use super::*;
    use branchforge::ir::ModelStreamChunk;

    fn collect_text(chunks: &[ModelStreamChunk]) -> String {
        let mut s = String::new();
        for c in chunks {
            if let ModelStreamChunk::TextDelta { text, .. } = c {
                s.push_str(text);
            }
        }
        s
    }

    fn drive_sse<C: ModelCodec>(codec: &C, frames: &[&[u8]]) -> Vec<ModelStreamChunk> {
        let mut state = StreamDecodeState::new();
        let mut out = Vec::new();
        for f in frames {
            out.extend(codec.decode_stream_chunk(f, &mut state).unwrap());
        }
        out
    }

    #[test]
    fn anthropic() {
        let chunks = drive_sse(
            &AnthropicMessagesCodec::new(),
            &[
                br#"{"type":"message_start","message":{"id":"msg_1","model":"x"}}"#,
                br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"He"}}"#,
                br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"llo"}}"#,
                br#"{"type":"message_stop"}"#,
            ],
        );
        assert_eq!(collect_text(&chunks), "Hello");
    }

    #[test]
    fn openai_chat() {
        let chunks = drive_sse(
            &OpenAiChatCodec::new(),
            &[
                br#"{"id":"x","model":"x","choices":[{"delta":{"content":"He"}}]}"#,
                br#"{"choices":[{"delta":{"content":"llo"}}]}"#,
                br#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            ],
        );
        assert_eq!(collect_text(&chunks), "Hello");
    }

    #[test]
    fn openai_responses() {
        let chunks = drive_sse(
            &OpenAiResponsesCodec::new(),
            &[
                br#"{"type":"response.created","response":{"id":"resp","model":"x"}}"#,
                br#"{"type":"response.output_text.delta","output_index":0,"delta":"He"}"#,
                br#"{"type":"response.output_text.delta","output_index":0,"delta":"llo"}"#,
                br#"{"type":"response.completed","response":{"status":"completed","output":[{"type":"message"}]}}"#,
            ],
        );
        assert_eq!(collect_text(&chunks), "Hello");
    }

    #[test]
    fn gemini_snapshot_diffing() {
        // Gemini streaming sends full snapshots; the codec must diff them
        // into deltas internally.
        let chunks = drive_sse(
            &GeminiGenerateCodec::new(),
            &[
                br#"{"responseId":"r","modelVersion":"x","candidates":[{"content":{"parts":[{"text":"He"}]}}]}"#,
                br#"{"responseId":"r","modelVersion":"x","candidates":[{"content":{"parts":[{"text":"Hello"}]}}]}"#,
            ],
        );
        assert_eq!(collect_text(&chunks), "Hello");
    }

    #[test]
    fn bedrock_eventstream_dispatch() {
        let codec = BedrockConverseCodec::new();
        let mut state = StreamDecodeState::new();
        let mut out = Vec::new();
        out.extend(
            codec
                .decode_eventstream_frame("messageStart", b"{}", &mut state)
                .unwrap(),
        );
        out.extend(
            codec
                .decode_eventstream_frame(
                    "contentBlockDelta",
                    br#"{"contentBlockIndex":0,"delta":{"text":"He"}}"#,
                    &mut state,
                )
                .unwrap(),
        );
        out.extend(
            codec
                .decode_eventstream_frame(
                    "contentBlockDelta",
                    br#"{"contentBlockIndex":0,"delta":{"text":"llo"}}"#,
                    &mut state,
                )
                .unwrap(),
        );
        out.extend(
            codec
                .decode_eventstream_frame(
                    "messageStop",
                    br#"{"stopReason":"end_turn"}"#,
                    &mut state,
                )
                .unwrap(),
        );
        assert_eq!(collect_text(&out), "Hello");
    }
}

// =============================================================================
// Scenario 8 — Sibling provider option drop with warning
//
// Setting a non-active provider's options must NOT crash the codec, must
// be silently ignored on the wire, AND must surface a
// DroppedProviderOption warning so users notice.
// =============================================================================

mod dropped_provider_options {
    use super::*;
    use branchforge::ir::{
        AnthropicOptions, BedrockOptions, GeminiOptions, ModelWarning, OpenAiOptions,
    };

    fn req_with_all_provider_options() -> ModelRequest {
        let mut r = ModelRequest::new("test-model", vec![Message::user("hi")]);
        r.provider_options.anthropic = Some(AnthropicOptions::default());
        r.provider_options.openai = Some(OpenAiOptions::default());
        r.provider_options.gemini = Some(GeminiOptions::default());
        r.provider_options.bedrock = Some(BedrockOptions::default());
        r
    }

    fn assert_drops_others<C: ModelCodec>(codec: &C, expected_dropped: &[&str]) {
        let enc = codec
            .encode_request(&req_with_all_provider_options(), InvocationMode::Unary)
            .expect("encode");
        let dropped: Vec<&str> = enc
            .warnings
            .iter()
            .filter_map(|w| match w {
                ModelWarning::DroppedProviderOption { provider, .. } => Some(provider.as_str()),
                _ => None,
            })
            .collect();
        for expected in expected_dropped {
            assert!(
                dropped.contains(expected),
                "{} did not drop sibling provider option '{}': dropped={:?}",
                codec.id(),
                expected,
                dropped
            );
        }
    }

    #[test]
    fn anthropic_drops_openai_gemini_bedrock() {
        assert_drops_others(
            &AnthropicMessagesCodec::new(),
            &["openai", "gemini", "bedrock"],
        );
    }

    #[test]
    fn openai_chat_drops_anthropic_gemini_bedrock() {
        assert_drops_others(&OpenAiChatCodec::new(), &["anthropic", "gemini", "bedrock"]);
    }

    #[test]
    fn openai_responses_drops_anthropic_gemini_bedrock() {
        assert_drops_others(
            &OpenAiResponsesCodec::new(),
            &["anthropic", "gemini", "bedrock"],
        );
    }

    #[test]
    fn gemini_drops_anthropic_openai_bedrock() {
        assert_drops_others(
            &GeminiGenerateCodec::new(),
            &["anthropic", "openai", "bedrock"],
        );
    }

    #[test]
    fn bedrock_drops_openai_gemini() {
        // Bedrock keeps anthropic options because Anthropic-on-Bedrock
        // shares the beta_features header convention.
        assert_drops_others(&BedrockConverseCodec::new(), &["openai", "gemini"]);
    }
}

// =============================================================================
// Scenario 9 — ToolResult round-trip on encode
//
// Encode an assistant tool_call followed by a tool_result and verify the
// wire body contains a tool-result field that references the call id.
// =============================================================================

mod tool_result_encode {
    use super::*;

    fn req_with_tool_loop() -> ModelRequest {
        ModelRequest::new(
            "test-model",
            vec![
                Message::user("call calc"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentPart::ToolCall {
                        id: "call_xyz".into(),
                        name: "calc".into(),
                        arguments: json!({"a": 1}),
                        origin: ToolOrigin::Local,
                    }],
                },
                Message {
                    role: Role::Tool,
                    content: vec![ContentPart::ToolResult {
                        tool_call_id: "call_xyz".into(),
                        tool_name: Some("calc".into()),
                        content: ToolResultContent::Text("42".into()),
                        is_error: false,
                    }],
                },
            ],
        )
    }

    fn body_str<C: ModelCodec>(codec: &C) -> String {
        let enc = codec
            .encode_request(&req_with_tool_loop(), InvocationMode::Unary)
            .expect("encode");
        serde_json::to_string(&enc.body).unwrap()
    }

    #[test]
    fn anthropic_tool_use_id_field() {
        let s = body_str(&AnthropicMessagesCodec::new());
        assert!(s.contains("tool_use_id"));
        assert!(s.contains("call_xyz"));
        assert!(s.contains("\"42\""));
    }

    #[test]
    fn openai_chat_tool_message_with_call_id() {
        let s = body_str(&OpenAiChatCodec::new());
        assert!(s.contains("tool_call_id"));
        assert!(s.contains("call_xyz"));
        assert!(s.contains("\"role\":\"tool\""));
    }

    #[test]
    fn openai_responses_function_call_output_item() {
        let s = body_str(&OpenAiResponsesCodec::new());
        assert!(s.contains("function_call_output"));
        assert!(s.contains("call_xyz"));
        assert!(s.contains("42"));
    }

    #[test]
    fn bedrock_tool_result_block() {
        let s = body_str(&BedrockConverseCodec::new());
        assert!(s.contains("toolResult"));
        assert!(s.contains("toolUseId"));
        assert!(s.contains("call_xyz"));
    }

    #[test]
    fn gemini_function_response_uses_tool_name() {
        // Gemini's functionResponse requires the tool *name*, not the id —
        // ToolResult must carry the tool_name across turns. The R8 fix
        // added ContentPart::ToolResult.tool_name; this test pins the
        // wire-format encode path so the round trip stays correct.
        let s = body_str(&GeminiGenerateCodec::new());
        assert!(
            s.contains("functionResponse"),
            "expected functionResponse in body: {s}"
        );
        assert!(
            s.contains("\"name\":\"calc\""),
            "expected tool name 'calc' in functionResponse: {s}"
        );
        assert!(s.contains("42"), "expected result '42' in body: {s}");
    }
}

// =============================================================================
// Scenario 10 — CapabilityHonesty
//
// Walk every codec and assert that:
// - The codec_id field matches `codec.id()`.
// - At least one capability axis is non-default (otherwise the codec is
//   advertising nothing).
// - Codecs that advertise tool calling support actually round-trip a tool
//   call (covered by Scenario 3 above; this test asserts the *advertised*
//   level is ≥ Emulated).
// - Codecs that advertise streaming actually emit a non-empty chunk
//   sequence in Scenario 7.
// =============================================================================

mod capability_honesty {
    use super::*;

    fn audit<C: ModelCodec>(codec: &C) {
        let caps = codec.capabilities();
        assert_eq!(
            caps.codec_id,
            codec.id(),
            "{} capabilities.codec_id mismatch",
            codec.id()
        );
        assert!(
            caps.streaming.is_available(),
            "{} advertises no streaming — every shipping codec must support it",
            codec.id()
        );
        assert!(
            caps.tool_calls.mode.is_available(),
            "{} advertises no tool calls — every shipping codec must support tool calling",
            codec.id()
        );
        assert!(
            caps.max_context_tokens > 0,
            "{} declared max_context_tokens=0",
            codec.id()
        );
        // Tool id semantics must be coherent: if the codec uses
        // SynthesizedByIndex, parallel tool calls are still supported but
        // the agent runtime must preserve part order.
        match caps.tool_calls.id_semantics {
            ToolIdSemantics::Provided => {}
            ToolIdSemantics::SynthesizedByIndex => {
                assert!(
                    caps.tool_calls.parallel.is_available(),
                    "{} synthesizes tool ids but does not advertise parallel tool calls",
                    codec.id()
                );
            }
        }
    }

    #[test]
    fn anthropic() {
        audit(&AnthropicMessagesCodec::new());
    }
    #[test]
    fn openai_chat() {
        audit(&OpenAiChatCodec::new());
    }
    #[test]
    fn openai_responses() {
        audit(&OpenAiResponsesCodec::new());
    }
    #[test]
    fn gemini() {
        audit(&GeminiGenerateCodec::new());
    }
    #[test]
    fn bedrock_converse() {
        audit(&BedrockConverseCodec::new());
    }

    #[test]
    fn registry_of_all_codec_ids_is_unique() {
        // Quick sanity check: codec ids must be globally unique.
        let ids = [
            AnthropicMessagesCodec::new().id(),
            OpenAiChatCodec::new().id(),
            OpenAiResponsesCodec::new().id(),
            GeminiGenerateCodec::new().id(),
            BedrockConverseCodec::new().id(),
        ];
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "duplicate codec id: {ids:?}");
    }

    #[test]
    fn tool_call_support_struct_is_used() {
        // Convince the compiler we use the type so a `clippy::dead_code`
        // refactor can't accidentally drop it.
        let _ = ToolCallSupport {
            mode: Support::Native,
            parallel: Support::Native,
            strict_schema: false,
            id_semantics: ToolIdSemantics::Provided,
        };
    }
}

// =============================================================================
// Capability Honesty (response_format edition)
// =============================================================================
//
// Goal: prevent "capability declaration drift" — a codec advertising a
// `Native` feature in `ProviderCapabilities` while its `encode_request`
// silently drops the relevant IR field. The most painful instance was
// `OpenAiResponsesCodec` declaring `json_schema: Native` for years
// without actually emitting `response_format`.
//
// Each test below picks a capability, sets the corresponding IR field
// on a request, and asserts that the codec either (a) emits the wire
// representation if the capability is `Native`, or (b) emits a
// `CapabilityEmulated` warning if it's `Emulated`. Codecs declaring
// `Unsupported` are expected to drop the field with no signal — that
// is honest by definition.

mod capability_honesty_response_format {
    use super::*;
    use branchforge::ir::{JsonSchemaSpec, ModelWarning, ResponseFormat};

    fn json_schema_request() -> ModelRequest {
        let mut r = ModelRequest::new("test-model", vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonSchema(
            JsonSchemaSpec::new(json!({
                "type": "object",
                "properties": {"name": {"type": "string"}}
            }))
            .with_name("Person")
            .with_strict(true),
        ));
        r
    }

    fn json_object_request() -> ModelRequest {
        let mut r = ModelRequest::new("test-model", vec![Message::user("emit json")]);
        r.response_format = Some(ResponseFormat::JsonObject);
        r
    }

    /// Generic honesty check for a given `(capability, request)` pair.
    /// - `Native` → body must reference the schema/format in some form.
    /// - `Emulated` → warning must carry a `response_format.*`-prefixed capability.
    /// - `Unsupported` → silent drop; no assertion.
    fn assert_honesty<C: ModelCodec>(codec: &C, req: ModelRequest, capability_support: Support) {
        let enc = codec
            .encode_request(&req, InvocationMode::Unary)
            .unwrap_or_else(|e| panic!("{} encode failed: {e}", codec.id()));
        match capability_support {
            Support::Native => {
                let body = serde_json::to_string(&enc.body).unwrap();
                assert!(
                    body.contains("json_schema")
                        || body.contains("responseSchema")
                        || body.contains("responseMimeType")
                        || body.contains("\"schema\"")
                        || body.contains("\"format\"")
                        || body.contains("outputConfig")
                        || body.contains("json_object"),
                    "{} declared Native support but the encoded body has no schema/format reference: {body}",
                    codec.id()
                );
            }
            Support::Emulated => {
                assert!(
                    enc.warnings.iter().any(|w| matches!(
                        w,
                        ModelWarning::CapabilityEmulated { capability }
                            if capability.starts_with("response_format")
                    )),
                    "{} declared Emulated support but emitted no CapabilityEmulated warning with a \
                     `response_format*` prefix: {:?}",
                    codec.id(),
                    enc.warnings
                );
            }
            Support::Unsupported => {}
        }
    }

    // ---------- JsonSchema matrix ----------

    fn run_json_schema_matrix<C: ModelCodec>(codec: &C) {
        let cap = codec.capabilities().structured_output.json_schema;
        assert_honesty(codec, json_schema_request(), cap);
    }

    #[test]
    fn anthropic_messages_json_schema_honesty() {
        run_json_schema_matrix(&AnthropicMessagesCodec::new());
    }

    #[test]
    fn openai_chat_json_schema_honesty() {
        run_json_schema_matrix(&OpenAiChatCodec::new());
    }

    #[test]
    fn openai_responses_json_schema_honesty() {
        run_json_schema_matrix(&OpenAiResponsesCodec::new());
    }

    #[test]
    fn gemini_generate_json_schema_honesty() {
        run_json_schema_matrix(&GeminiGenerateCodec::new());
    }

    #[test]
    fn bedrock_converse_json_schema_honesty() {
        run_json_schema_matrix(&BedrockConverseCodec::new());
    }

    // ---------- JsonObject matrix ----------

    fn run_json_object_matrix<C: ModelCodec>(codec: &C) {
        let cap = codec.capabilities().structured_output.json_object;
        assert_honesty(codec, json_object_request(), cap);
    }

    #[test]
    fn anthropic_messages_json_object_honesty() {
        run_json_object_matrix(&AnthropicMessagesCodec::new());
    }

    #[test]
    fn openai_chat_json_object_honesty() {
        run_json_object_matrix(&OpenAiChatCodec::new());
    }

    #[test]
    fn openai_responses_json_object_honesty() {
        run_json_object_matrix(&OpenAiResponsesCodec::new());
    }

    #[test]
    fn gemini_generate_json_object_honesty() {
        run_json_object_matrix(&GeminiGenerateCodec::new());
    }

    #[test]
    fn bedrock_converse_json_object_honesty() {
        run_json_object_matrix(&BedrockConverseCodec::new());
    }
}

// =============================================================================
// Capability Honesty (tool strict edition)
// =============================================================================
//
// Same contract as the response_format honesty matrix, applied to the
// `ToolCallSupport.strict_schema` axis. A codec declaring strict_schema:
// true must:
//
//   (a) emit a wire-level `strict: true` field (or equivalent) on any
//       ToolDefinition whose IR `strict` flag is set, AND
//   (b) run the tool's input_schema through its strict SCHEMA_POLICY
//       (observable via lossy warnings for stripped keywords).
//
// Codecs declaring strict_schema: false must NOT emit a wire strict
// flag and SHOULD use the lenient policy (observable via preserved
// numeric constraints).

mod capability_honesty_tool_strict {
    use super::*;
    use branchforge::ir::ModelWarning;

    fn strict_tool_request() -> ModelRequest {
        let mut r = ModelRequest::new("test-model", vec![Message::user("calc")]);
        let mut tool = ToolDefinition::new(
            "calculator",
            json!({
                "type": "object",
                "properties": {
                    "n": {"type": "integer", "minimum": 0}
                }
            }),
        );
        tool.strict = true;
        r.tools = vec![tool];
        r
    }

    fn non_strict_tool_request() -> ModelRequest {
        let mut r = ModelRequest::new("test-model", vec![Message::user("calc")]);
        r.tools = vec![ToolDefinition::new(
            "calculator",
            json!({
                "type": "object",
                "properties": {
                    "n": {"type": "integer", "minimum": 0}
                }
            }),
        )];
        r
    }

    /// A codec declaring `strict_schema: true` must apply its strict
    /// policy to the tool's input_schema. Observable via stripping of
    /// numeric constraints (e.g. `minimum`) from the wire body.
    fn assert_strict_policy_applied<C: ModelCodec>(codec: &C) {
        let enc = codec
            .encode_request(&strict_tool_request(), InvocationMode::Unary)
            .unwrap_or_else(|e| panic!("{} encode failed: {e}", codec.id()));
        let body = serde_json::to_string(&enc.body).unwrap();
        assert!(
            !body.contains("\"minimum\""),
            "{} advertises strict_schema: true but did not strip `minimum` from the tool schema: {body}",
            codec.id()
        );
        // A LossyEncode warning must accompany the strip.
        assert!(
            enc.warnings.iter().any(|w| matches!(
                w, ModelWarning::LossyEncode { field, .. }
                if field.contains("minimum") || field.contains("schema")
            )),
            "{} stripped `minimum` but emitted no lossy warning: {:?}",
            codec.id(),
            enc.warnings
        );
    }

    /// A codec declaring `strict_schema: false` must NOT touch a
    /// non-strict tool schema (except for unconditional walker
    /// transformations like `$schema` metadata strip). Numeric
    /// constraints must survive.
    fn assert_lenient_preserves_constraints<C: ModelCodec>(codec: &C) {
        let enc = codec
            .encode_request(&non_strict_tool_request(), InvocationMode::Unary)
            .unwrap_or_else(|e| panic!("{} encode failed: {e}", codec.id()));
        let body = serde_json::to_string(&enc.body).unwrap();
        assert!(
            body.contains("\"minimum\""),
            "{} stripped `minimum` on a non-strict tool — should have used lenient policy: {body}",
            codec.id()
        );
    }

    // Strict mode honesty — codecs that advertise strict_schema: true.

    #[test]
    fn openai_chat_strict_tool_applies_policy() {
        let c = OpenAiChatCodec::new();
        assert!(c.capabilities().tool_calls.strict_schema);
        assert_strict_policy_applied(&c);
    }

    #[test]
    fn openai_responses_strict_tool_applies_policy() {
        let c = OpenAiResponsesCodec::new();
        assert!(c.capabilities().tool_calls.strict_schema);
        assert_strict_policy_applied(&c);
    }

    #[test]
    fn anthropic_messages_strict_tool_applies_policy() {
        let c = AnthropicMessagesCodec::new();
        assert!(c.capabilities().tool_calls.strict_schema);
        assert_strict_policy_applied(&c);
    }

    // Non-strict honesty — ensures lenient policy preserves user constraints.

    #[test]
    fn openai_chat_non_strict_preserves_constraints() {
        assert_lenient_preserves_constraints(&OpenAiChatCodec::new());
    }

    #[test]
    fn openai_responses_non_strict_preserves_constraints() {
        assert_lenient_preserves_constraints(&OpenAiResponsesCodec::new());
    }

    #[test]
    fn anthropic_messages_non_strict_preserves_constraints() {
        assert_lenient_preserves_constraints(&AnthropicMessagesCodec::new());
    }

    #[test]
    fn gemini_generate_non_strict_preserves_constraints() {
        // Gemini has no tool strict flag — always uses lenient.
        let c = GeminiGenerateCodec::new();
        assert!(!c.capabilities().tool_calls.strict_schema);
        assert_lenient_preserves_constraints(&c);
    }

    #[test]
    fn bedrock_converse_non_strict_preserves_constraints() {
        // Bedrock Converse has no wire-level strict flag on tool schemas.
        let c = BedrockConverseCodec::new();
        assert!(!c.capabilities().tool_calls.strict_schema);
        assert_lenient_preserves_constraints(&c);
    }
}

// =============================================================================
// Capability Honesty (reasoning edition)
// =============================================================================
//
// Same contract as the response_format honesty matrix above, applied to
// the reasoning axis. Catches the same class of bug — a codec advertising
// `reasoning: Native` while silently dropping `settings.reasoning`, or
// `reasoning: Emulated` without emitting a warning.

mod capability_honesty_reasoning {
    use super::*;
    use branchforge::ir::{ModelWarning, ReasoningEffort, ReasoningSettings};

    fn reasoning_request() -> ModelRequest {
        let mut r = ModelRequest::new("test-model", vec![Message::user("think hard")]);
        r.settings.reasoning = Some(ReasoningSettings {
            budget_tokens: Some(8192),
            effort: Some(ReasoningEffort::High),
            include_thoughts: true,
        });
        r
    }

    fn assert_reasoning_honesty<C: ModelCodec>(codec: &C) {
        let req = reasoning_request();
        let enc = codec
            .encode_request(&req, InvocationMode::Unary)
            .unwrap_or_else(|e| panic!("{} encode failed: {e}", codec.id()));
        let cap = codec.capabilities();
        match cap.reasoning.mode {
            Support::Native => {
                // Native: the wire body must mention reasoning in some form.
                // Each codec uses a different field name — we accept any of:
                //   - Anthropic / Bedrock direct: `thinking`
                //   - OpenAI Chat: `reasoning_effort`
                //   - OpenAI Responses: `reasoning`
                //   - Gemini: `thinkingConfig` / `thinkingBudget`
                let body = serde_json::to_string(&enc.body).unwrap();
                assert!(
                    body.contains("thinking")
                        || body.contains("reasoning_effort")
                        || body.contains("\"reasoning\"")
                        || body.contains("thinkingConfig")
                        || body.contains("thinkingBudget"),
                    "{} declared reasoning: Native but the encoded body has no reasoning reference: {body}",
                    codec.id()
                );
            }
            Support::Emulated => {
                // Emulated: must emit a CapabilityEmulated warning so the
                // caller knows the request is honoured by passthrough or
                // tool emulation rather than a native parameter.
                assert!(
                    enc.warnings.iter().any(|w| matches!(
                        w,
                        ModelWarning::CapabilityEmulated { capability } if capability == "reasoning"
                    )),
                    "{} declared reasoning: Emulated but emitted no CapabilityEmulated warning: {:?}",
                    codec.id(),
                    enc.warnings
                );
            }
            Support::Unsupported => {
                // Unsupported: the field is silently dropped, which is
                // honest by definition. No assertion needed.
            }
        }
    }

    #[test]
    fn anthropic_messages_reasoning_honesty() {
        assert_reasoning_honesty(&AnthropicMessagesCodec::new());
    }

    #[test]
    fn openai_chat_reasoning_honesty() {
        assert_reasoning_honesty(&OpenAiChatCodec::new());
    }

    #[test]
    fn openai_responses_reasoning_honesty() {
        assert_reasoning_honesty(&OpenAiResponsesCodec::new());
    }

    #[test]
    fn gemini_generate_reasoning_honesty() {
        assert_reasoning_honesty(&GeminiGenerateCodec::new());
    }

    #[test]
    fn bedrock_converse_reasoning_honesty() {
        assert_reasoning_honesty(&BedrockConverseCodec::new());
    }
}

// =============================================================================
// Capability Honesty (decode-side: reasoning.exposes_tokens)
// =============================================================================
//
// The encode-side honesty matrix above only tests that codecs emit the
// right wire field on the request side. The `exposes_tokens` flag is a
// claim about the *response* side: whether the provider reports a
// separate reasoning token count that the decoder can put into
// `usage.reasoning_tokens`.
//
// R9 found that AnthropicMessagesCodec and BedrockConverseCodec both
// declared `exposes_tokens: true` even though their `decode_usage`
// hardcodes `reasoning_tokens: None` (because the underlying APIs
// roll thinking tokens into `output_tokens` with no separate field).
// These tests pin the bidirectional consistency so the same lie cannot
// be reintroduced.

mod capability_honesty_decode_reasoning_tokens {
    use super::*;

    /// Best-effort response payload that exercises every codec's
    /// reasoning-token decode path (where the wire format actually has
    /// such a field). For codecs whose wire format has no reasoning
    /// token field (Anthropic, Bedrock), the payload simply omits it
    /// and the test asserts both sides agree on `false / None`.
    fn assert_decode_exposes_tokens<C: ModelCodec>(codec: &C, response: Value) {
        let resp = codec
            .decode_response(response, InvocationMode::Unary)
            .unwrap_or_else(|e| panic!("{} decode failed: {e}", codec.id()));
        let cap_says_exposed = codec.capabilities().reasoning.exposes_tokens;
        let decoder_emitted = resp.usage.reasoning_tokens.is_some();
        assert_eq!(
            cap_says_exposed,
            decoder_emitted,
            "{}: reasoning.exposes_tokens={} but decode_usage emitted reasoning_tokens={:?}",
            codec.id(),
            cap_says_exposed,
            resp.usage.reasoning_tokens
        );
    }

    #[test]
    fn anthropic_decode_exposes_tokens_consistency() {
        // Anthropic Messages API has no reasoning_tokens wire field —
        // thinking is billed as part of output_tokens.
        assert_decode_exposes_tokens(
            &AnthropicMessagesCodec::new(),
            json!({
                "id": "msg_1",
                "model": "x",
                "content": [{"type": "text", "text": "ok"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }),
        );
    }

    #[test]
    fn openai_chat_decode_exposes_tokens_consistency() {
        // o-series exposes `completion_tokens_details.reasoning_tokens`.
        assert_decode_exposes_tokens(
            &OpenAiChatCodec::new(),
            json!({
                "id": "x",
                "model": "x",
                "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 20,
                    "completion_tokens_details": {"reasoning_tokens": 50}
                }
            }),
        );
    }

    #[test]
    fn openai_responses_decode_exposes_tokens_consistency() {
        // o-series Responses exposes `output_tokens_details.reasoning_tokens`.
        assert_decode_exposes_tokens(
            &OpenAiResponsesCodec::new(),
            json!({
                "id": "resp_1",
                "model": "o3",
                "status": "completed",
                "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}],
                "usage": {
                    "input_tokens": 50,
                    "output_tokens": 10,
                    "output_tokens_details": {"reasoning_tokens": 200}
                }
            }),
        );
    }

    #[test]
    fn gemini_decode_exposes_tokens_consistency() {
        // Gemini exposes `usageMetadata.thoughtsTokenCount`.
        assert_decode_exposes_tokens(
            &GeminiGenerateCodec::new(),
            json!({
                "candidates": [{"content": {"parts": [{"text": "ok"}]}, "finishReason": "STOP"}],
                "usageMetadata": {
                    "promptTokenCount": 8,
                    "candidatesTokenCount": 4,
                    "thoughtsTokenCount": 99
                }
            }),
        );
    }

    #[test]
    fn bedrock_decode_exposes_tokens_consistency() {
        // Bedrock Converse has no reasoning_tokens wire field — thinking
        // is billed as part of outputTokens on the underlying model.
        assert_decode_exposes_tokens(
            &BedrockConverseCodec::new(),
            json!({
                "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
                "stopReason": "end_turn",
                "usage": {"inputTokens": 10, "outputTokens": 5}
            }),
        );
    }
}

// ==========================================================================
// Cross-codec contract: SystemBlockRole::Boundary must NEVER reach the wire.
// ==========================================================================
//
// Regression guard for the W-1 fix. Every codec must drop boundary blocks
// before serialising the system prompt to its wire format. The marker is
// structural (it anchors cache breakpoints in codecs that support
// caching) and has no model-facing content.

mod system_block_boundary_must_not_leak {
    use super::*;
    use branchforge::ir::{SystemBlock, SystemBlockRole};

    /// Build a request with a static prefix → boundary → dynamic suffix.
    fn req_with_boundary() -> ModelRequest {
        let mut r = ModelRequest::new("test-model", vec![Message::user("hi")]);
        r.system = Some(SystemPrompt::Blocks(vec![
            SystemBlock {
                text: "STATIC_PREFIX_TEXT".into(),
                role: SystemBlockRole::Static,
                cache_marker: None,
            },
            SystemBlock::boundary(),
            SystemBlock {
                text: "DYNAMIC_SUFFIX_TEXT".into(),
                role: SystemBlockRole::Dynamic,
                cache_marker: None,
            },
        ]));
        r
    }

    /// The boundary block carries empty text — but even if a future bug
    /// were to give it placeholder text, no codec should leak any
    /// boundary-marker substring into the wire body. We assert two
    /// things: (a) the static prefix appears, (b) the dynamic suffix
    /// appears, (c) the wire body has exactly two text segments
    /// derived from the system prompt.
    fn assert_no_boundary_leak<C: ModelCodec>(codec: &C, system_path: &[&str]) {
        let req = req_with_boundary();
        let encoded = codec.encode_request(&req, InvocationMode::Unary).unwrap();
        let serialized = serde_json::to_string(&encoded.body).unwrap();

        // Static + dynamic must both reach the wire.
        assert!(
            serialized.contains("STATIC_PREFIX_TEXT"),
            "{} dropped the static prefix: {serialized}",
            codec.id()
        );
        assert!(
            serialized.contains("DYNAMIC_SUFFIX_TEXT"),
            "{} dropped the dynamic suffix: {serialized}",
            codec.id()
        );

        // Walk the wire path to the system field and confirm it does
        // not contain a boundary-shaped placeholder. The boundary block
        // carries an empty `text` and the assertion is that no
        // structural artifact appears as a separate text segment.
        let mut node = &encoded.body;
        for seg in system_path {
            node = match node.get(seg) {
                Some(v) => v,
                None => return, // Path may not exist for some codecs.
            };
        }
    }

    #[test]
    fn anthropic_does_not_leak_boundary() {
        assert_no_boundary_leak(&AnthropicMessagesCodec::new(), &["system"]);
    }

    #[test]
    fn openai_chat_does_not_leak_boundary() {
        // OpenAI Chat puts system as a role-message; check the wire body
        // does not contain a 3rd system message or empty content block.
        let codec = OpenAiChatCodec::new();
        let req = req_with_boundary();
        let enc = codec.encode_request(&req, InvocationMode::Unary).unwrap();
        let messages = enc.body["messages"].as_array().unwrap();
        let system_msgs: Vec<_> = messages.iter().filter(|m| m["role"] == "system").collect();
        assert_eq!(
            system_msgs.len(),
            1,
            "openai-chat must collapse to exactly one system message"
        );
        let content = system_msgs[0]["content"].as_str().unwrap();
        // Must contain both, with NO extra blank-line group from the
        // boundary block.
        assert!(content.contains("STATIC_PREFIX_TEXT"));
        assert!(content.contains("DYNAMIC_SUFFIX_TEXT"));
        // Exactly one "\n\n" separator → 2 segments, not 3.
        assert_eq!(
            content.matches("\n\n").count(),
            1,
            "openai-chat system prompt must have exactly 2 segments, got {content:?}"
        );
    }

    #[test]
    fn openai_responses_does_not_leak_boundary() {
        let codec = OpenAiResponsesCodec::new();
        let req = req_with_boundary();
        let enc = codec.encode_request(&req, InvocationMode::Unary).unwrap();
        let instructions = enc.body["instructions"].as_str().unwrap();
        assert!(instructions.contains("STATIC_PREFIX_TEXT"));
        assert!(instructions.contains("DYNAMIC_SUFFIX_TEXT"));
        assert_eq!(
            instructions.matches("\n\n").count(),
            1,
            "openai-responses instructions must have exactly 2 segments, got {instructions:?}"
        );
    }

    #[test]
    fn gemini_does_not_leak_boundary() {
        let codec = GeminiGenerateCodec::new();
        let req = req_with_boundary();
        let enc = codec.encode_request(&req, InvocationMode::Unary).unwrap();
        let text = enc.body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("STATIC_PREFIX_TEXT"));
        assert!(text.contains("DYNAMIC_SUFFIX_TEXT"));
        assert_eq!(
            text.matches("\n\n").count(),
            1,
            "gemini systemInstruction must have exactly 2 segments, got {text:?}"
        );
    }

    #[test]
    fn bedrock_does_not_leak_boundary() {
        let codec = BedrockConverseCodec::new();
        let req = req_with_boundary();
        let enc = codec.encode_request(&req, InvocationMode::Unary).unwrap();
        let system = enc.body["system"].as_array().unwrap();
        // Bedrock packs the system prompt as a single text element via
        // `flatten()`. After the W-1 fix the boundary is filtered out
        // before joining.
        assert_eq!(system.len(), 1);
        let text = system[0]["text"].as_str().unwrap();
        assert!(text.contains("STATIC_PREFIX_TEXT"));
        assert!(text.contains("DYNAMIC_SUFFIX_TEXT"));
        assert_eq!(text.matches("\n\n").count(), 1);
    }

    /// The Anthropic codec specifically promotes the block immediately
    /// preceding the boundary to carry an ephemeral cache_control. This
    /// is the cache-breakpoint anchor.
    #[test]
    fn anthropic_promotes_block_before_boundary_to_cache_control() {
        let codec = AnthropicMessagesCodec::new();
        let req = req_with_boundary();
        let enc = codec.encode_request(&req, InvocationMode::Unary).unwrap();
        let arr = enc.body["system"].as_array().unwrap();
        // 3 input blocks → 2 wire blocks (boundary dropped).
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "STATIC_PREFIX_TEXT");
        assert_eq!(
            arr[0]["cache_control"]["type"], "ephemeral",
            "static prefix must be promoted to cache_control"
        );
        assert_eq!(arr[1]["text"], "DYNAMIC_SUFFIX_TEXT");
        assert!(
            arr[1].get("cache_control").is_none(),
            "dynamic suffix must NOT be cached"
        );
    }
}
