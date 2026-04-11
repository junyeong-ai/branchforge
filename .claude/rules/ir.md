---
paths:
  - "src/ir/**"
---

# IR Module Rules

Provider-neutral intermediate representation. Codecs translate between these types and provider wire formats; any lossy translation must emit a `ModelWarning`.

## Module layout

- `model.rs` — `ModelRequest`, `ModelResponse`, `Message`, `Role`, `ToolDefinition`, `ToolChoice`, `ResponseFormat`, `JsonSchemaSpec`, `Continuation`, `SystemPrompt`, `SystemBlock`
- `content.rs` — `ContentPart` and sub-enums (`MediaSource`, `ReasoningContent`, `ReasoningKind`, `ReasoningSignature`, `ToolOrigin`, `ToolResultContent`)
- `stream.rs` — `ModelStreamChunk`, `StreamFraming`, `StreamDecodeState`, `PartialUsage`
- `settings.rs` — `ModelSettings`, `ReasoningSettings`, `ReasoningEffort`
- `provider_options.rs` — `ProviderOptions`, `AnthropicOptions`, `OpenAiOptions`, `GeminiOptions`, `BedrockOptions`, `VertexOptions`, `CacheControl`, `CacheMarker`
- `usage.rs` — `Usage`, `ServerToolInvocations`
- `finish.rs` — `FinishReason`
- `warning.rs` — `ModelWarning`
- `capabilities.rs` — `ProviderCapabilities` and sub-structs
- `token_count.rs` — `TokenCount(u64)` newtype

## Top-level invariants

- **`Role` has no `System` variant.** System prompts live on `ModelRequest::system` as a top-level field, not as a message role. This is the only intentional asymmetry in the IR — see the design note on `ModelRequest::system`.
- **`ModelRequest.system` is `Option<SystemPrompt>`**, where `SystemPrompt::Text(String)` is the portable shape and `SystemPrompt::Blocks(Vec<SystemBlock>)` supports Anthropic's per-block `cache_marker`. Codecs that only accept a flat system string flatten via `SystemPrompt::flatten()` and emit a `LossyEncode` warning if `has_block_metadata()` is true.
- **`ContentPart::ToolResult.tool_name` is `Option<String>`** — required by Gemini's `functionResponse.name`, optional elsewhere. Codecs that need it without a value emit a `LossyEncode` warning pointing at `tool_result.tool_name`.

## `Usage` invariant

- `input_tokens` is the **TOTAL** input (cached + non-cached). Anthropic and Bedrock wire formats report `inputTokens` as the non-cached portion; codecs reconstruct the total in `decode_usage`. `Usage::add` has a `debug_assert` that requires `cached_input_tokens <= input_tokens`.
- `billable_input_tokens()` returns `input_tokens - cached_input_tokens.unwrap_or(0)` — the portion billed at the full input rate.

## `JsonSchemaSpec` (structured outputs)

- `ResponseFormat::JsonSchema(JsonSchemaSpec)` is the only wrapper — never match on `{ name, schema, strict }` struct variants. Builder:

  ```rust
  JsonSchemaSpec::new(json!({...}))
      .with_name("Person")
      .with_description("A person record")
      .with_strict(true)
  ```

- **`name` and `description` wire support varies**: OpenAI Chat/Responses and Bedrock Converse emit them natively; Anthropic and Gemini drop them with a `LossyEncode` warning. The truth lives in `SchemaPolicy.wire_supports_name` / `wire_supports_description` — each policy declares it — and the shared `client::schema::warn_dropped_metadata` function reads it to emit warnings uniformly. Never write per-codec metadata-warning helpers; the shared function is the single source of truth.
- **`strict` defaults to `true`** on both the constructor (`JsonSchemaSpec::new`) and serde deserialization (`#[serde(default = "default_strict")]`). Without the explicit serde default, a spec missing the `strict` field would deserialize to `false`, contradicting the constructor default.
- **`JsonSchemaSpec::from_type::<T>()`** uses `schemars::schema_for!(T)` and extracts the type's short name via `short_type_name<T>()`, which strips generic parameters before taking the last `::` segment (so `Vec<Person>` → `"Vec"`, not `"Person>"`).
- Codecs never strip keywords themselves. They delegate to `prepare_schema(value, &SCHEMA_POLICY, "response_format.schema")` for response-format schemas and `prepare_tool_schema(tool.parameters.clone(), &SCHEMA_POLICY, tool.strict, &tool.name)` for tool schemas — see `.claude/rules/schema.md`.

## `ModelResponse::json::<T>()` decoder

`ModelResponse::json::<T: DeserializeOwned>()` is a finish-reason-aware decoder convenience. It returns distinctive `Error::Parse` messages for:

- `FinishReason::ContentFilter` — "model refused or output was filtered"
- `FinishReason::Length` — "response truncated by max_tokens; JSON is likely incomplete"
- Empty text content
- `serde_json::from_str` failure on non-empty text

Callers can disambiguate model behaviour from parse failures by reading the error text, or by checking `response.finish_reason` before calling `.json()`.

## `FinishReason` normalisation

`FinishReason::ContentFilter` unifies Anthropic `refusal`, OpenAI `content_filter`, Gemini `SAFETY`/`RECITATION`/`BLOCKLIST`/`PROHIBITED_CONTENT`, and Bedrock `guardrail_intervened`. Never add a separate `Refusal` variant — use `ContentFilter`.

`FinishReason::PauseTurn` is distinct from `Stop`: it means "call me back with the same history and I'll continue" (Anthropic `pause_turn`, OpenAI Responses `incomplete { reason: tool_call_loop }`). Agent runtimes should re-invoke the model rather than treat it as completion. `FinishReason::should_continue()` returns `true` for `ToolCalls` and `PauseTurn`.

## `ModelWarning` variants

Codecs and the schema walker emit one of:

- `UnsupportedSetting { setting, codec }` — a `ModelSettings` field was set but the codec does not implement it.
- `SettingClamped { setting, from, to }` — a setting was clamped (e.g. `temperature: 2.0` → `1.0`).
- `LossyEncode { field, reason }` — a field could not be represented exactly on the wire. Walker output uses JSON-pointer paths (`response_format.schema/properties/foo/minimum`).
- `DroppedProviderOption { provider, option }` — a `provider_options.<provider>` extension was set for a different provider than the active codec.
- `CapabilityEmulated { capability }` — a capability declared `Emulated` was used. For `response_format`, the string is variant-specific (`response_format.json_object`, `response_format.json_schema`). Tests use `starts_with("response_format")`.
- `Other(String)` — fallback.

## `ProviderCapabilities` honesty

- Every codec returns `&'static ProviderCapabilities` (prefer a `const`). Defaults come from `ProviderCapabilities::unsupported(codec_id)` — every axis is `Support::Unsupported` until explicitly overridden.
- `StructuredOutputSupport.strict` is `true` when the provider validates server-side (Anthropic, Bedrock Converse, OpenAI strict mode). Gemini is `false`.
- The `capability_honesty_response_format` matrix in `tests/codec_contract.rs` enforces that declared capabilities match actual encode behaviour across all 5 codecs × (JsonSchema + JsonObject).
