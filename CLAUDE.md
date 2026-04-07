# CLAUDE.md

Rust-native agent SDK with graph-first session model. `SessionGraph` is the canonical state; `Session.messages` is a derived projection.

## Commands

```bash
cargo build --release
cargo test --all-features                       # local
cargo nextest run --all-features                # CI (requires cargo-nextest)
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
```

## Feature Flags

```bash
cargo build                                     # default: coding-tools
cargo build --no-default-features               # pure SDK core
cargo build --features "coding-tools"           # file I/O, bash, tree-sitter AST
cargo build --features "scheduling"             # cron expressions, remote triggers
cargo build --features "cli-auth"               # Claude Code CLI OAuth
cargo build --features "mcp"                    # MCP server (stdio + SSE)
cargo build --features "cloud-all"              # aws, gcp, azure, openai, gemini
cargo build --features "persistence-all"        # jsonl, postgres, redis
cargo build --features "full"                   # all of the above
cargo build --all-features                      # full + multimedia
```

## Architecture Principles

- `SessionGraph` is the single source of truth. `Session.messages` is always rebuilt via `refresh_message_projection()`.
- Retry (same model, backoff) and Fallback (different model) are separate concerns in `Client`.
- Tool cancellation uses `ExecutionContext.cancel_token` (opt-in, same pattern as `progress_tx`).
- **Provider abstraction uses a 3-axis design**: `ModelCodec` (wire format) × `ModelTransport` (auth + endpoint) × `EndpointShape` (URL pattern bridge). This makes new provider combinations (e.g. Gemini-on-Vertex) fall out as free compositions.
- Provider capabilities are declared via `ProviderCapabilities` (in `src/ir/capabilities.rs`) with `Unsupported` defaults. Each codec returns a `const` value.
- The internal IR (`src/ir/`) is provider-neutral. Codecs translate between IR and wire format, emitting `ModelWarning` for lossy encodes. The old `src/types/` and `src/client/adapter/` are legacy and will be removed once all consumers migrate to IR.
- Errors use typed enums (`SessionError`, `McpError`, `GraphError`). `Error::Provider { kind, hint }` carries actionable hints for well-known failures. `Error::Config(String)` is intentional for developer-facing messages.
- Feature flags gate optional dependencies. Core SDK has zero cloud/DB deps.
- **Token counts are `u64` end-to-end** (Phase 1b-γ). `ir::Usage`, `AgentMetrics.{input,output,cache_*}_tokens`, `ExecutionMetadata.usage`, `TaskExecutionSummary.usage`, and `pricing::PricingTable::calculate` all operate on `u64` / `ir::Usage`. Legacy `types::Usage` (u32) survives only as the on-the-wire DTO for the legacy adapter response shape and is converted at the boundary via `From<&types::Usage> for ir::Usage` in `src/ir/compat.rs` (deleted with the rest of compat in Phase θ).
- **Tool call linkage uses `tool_call_id`** (Phase 1b-δ). The legacy `tool_use_id` field name lives only in `src/types/` and on-the-wire Anthropic payloads. New code (`ToolCallRecord`, `ToolResultMeta`, graph node payloads, OTel spans) all use `tool_call_id`.
- **Session/agent layer uses IR types end-to-end** (Phase 1b-δ). `SessionMessage`, `Session::to_api_messages()`, `AgentResult.messages`, `ReplayInput.messages`, and all content overrides/compaction use `ir::Message`, `ir::ContentPart`, `ir::Role`. Legacy `types::Message`/`types::ContentBlock` survive only in `src/types/`, `src/client/`, `src/ir/compat.rs`, and the on-the-wire DTO. The boundary conversion lives in `ir::compat::{ir_message_to_legacy, legacy_block_to_ir}`. Cache breakpoints moved from per-message `cache_control` to the codec/transport layer.

## Phase 1b migration status

The migration of the agent runtime / session layer to the new IR is in progress.

| Phase | Status | Notes |
|---|---|---|
| α Setup | ✅ | baseline 1396 lib + 48 codec_contract pass |
| β IR defect prepass | ✅ | u64 unification, `Continuation::OpenAiResponses` rename, `idempotency_key`, `Usage::billable_input_tokens`, `Usage::add` invariant guard, `ToolIdSemantics::SynthesizedByName` removed |
| γ-1 pricing IR-native | ✅ | `pricing.calculate(&ir::Usage)`, `BudgetTracker::record(&ir::Usage)`, `TenantBudget::record(&ir::Usage)` |
| γ-2 truncation fix | ✅ | `ExecutionMetadata.usage` and `TaskExecutionSummary.usage` migrated to `ir::Usage` (u64). The CRITICAL u64→u32 truncation at the old `task_registry::execution_summary` is gone. |
| γ-3 AgentMetrics widen | ✅ | `AgentMetrics.{input,output,cache_*}_tokens` widened to u64. |
| δ naming alignment | ✅ | `tool_use_id` → `tool_call_id` rename through `ToolCallRecord`, `ToolResultMeta`, `record_tool` param, graph node JSON payloads, `tool_execute_span`. |
| δ Message/ContentPart cascade | ✅ | `SessionMessage`, `Session::to_api_messages()`, `AgentResult.messages`, `ReplayInput`, compaction, graph replay all use `ir::Message`/`ir::ContentPart`/`ir::Role`. Legacy→IR conversion at `RequestBuilder::build()` boundary via `ir::compat::ir_message_to_legacy`. 1569 lib + 48 codec_contract pass. |
| ε FinishReason cascade | ✅ | `types::StopReason` → `ir::FinishReason` in agent/session. `From<StopReason> for FinishReason` boundary conversion in compat.rs. |
| ζ-1 LlmCall trait + decorators | ✅ | `LlmCall` trait (`send` + `send_stream`) + `RetryingClient`, `FallingBackClient`, `CircuitBrokenClient`, `LegacyBridgeClient`. Wired into `AgentRuntime.llm`. |
| ζ-2 execution loop on LlmCall | ✅ | Non-streaming execution loop dispatches through `self.runtime.llm.send()` → `ir::ModelResponse`. Tool dispatch uses `ContentPart::ToolCall`. `accumulate_response_usage`/`emit_tokens_consumed` accept `&ir::Usage`. |
| ζ-3 streaming + legacy deletion | ⏳ | Streaming agent migration to `LlmCall::send_stream()` + `ModelStreamChunk`. Then delete `src/client/adapter/`, `src/client/messages/`, legacy `Client`/`ClientBuilder`, `streaming.rs`, `batch.rs`, `files.rs`. |
| η `src/types/*` cleanup + `src/client/` → `src/provider/` rename | ⏳ | Delete `types/{message,response,content,document}.rs`. Keep `types/tool/`. |
| θ `src/ir/compat.rs` deletion | ⏳ | Final compat bridge removal + grep guards. |
| ι UX polish | ⏳ | `Agent::quick`, `provider_from_env`, `tracing` span standardisation, `examples/quickstart.rs`. |
| κ Final verification | ⏳ | `cargo test --lib` ≥ 1396, `codec_contract` ≥ 63, clippy 0 warnings, vertex_gemini live calls. |

The locked plan lives in `/Users/mac/.claude/plans/phase1b-final.md`.

## Key Areas

- `src/ir/`: **provider-neutral IR** — `ModelRequest`, `ModelResponse`, `ContentPart`, `FinishReason`, `Usage`, `ModelStreamChunk`, `ModelSettings`, `ProviderOptions`, `ProviderCapabilities`, `ModelWarning`. This is the canonical representation for all LLM calls.
- `src/client/codec/`: **wire-format codecs** — `AnthropicMessagesCodec`, `OpenAiChatCodec`, `OpenAiResponsesCodec`, `GeminiGenerateCodec`, `BedrockConverseCodec`. Pure encoder/decoder, no HTTP/auth.
- `src/client/transport/`: **endpoint + auth** — `DirectTransport` (API key/bearer), `VertexTransport` (GCP ADC, publisher routing), `BedrockTransport` (SigV4), `FoundryTransport` (Azure Entra). `EndpointShape` bridge.
- `src/client/provider_client.rs`: `ProviderClient` — composition of one codec + one transport. `send()` and `send_stream()` pipelines with framing-aware streaming (SSE, AwsEventStream, JsonArray, NdJson).
- `src/client/preset.rs`: `Preset` enum — 8 named (codec × transport) presets with `build_from_env()` factories. `BRANCHFORGE_PROVIDER` env var selects preset.
- `src/graph/`: session graph, replay, export, materialization, validation
- `src/session/`: session facade, persistence (memory, JSONL, postgres, redis), compaction, queueing, locking
- `src/agent/`: runtime loop, execution + streaming, task orchestration, delegation, builder. `AgentBuilder::provider_client(pc)` wires the new stack.
- `src/client/`: (legacy) provider adapters, RetryPolicy, FallbackConfig, CircuitBreaker, streaming, batch, files. `Client::with_provider_client(pc)` bridges old Client to new stack via `ir/compat.rs`.
- `src/auth/`: credential resolution, OAuth token refresh, CLI credential storage, caching
- `src/tools/`: Tool/SchemaTool traits, registry, execution context, progress, cancellation
- `src/authorization/`: execution modes (auto/plan/supervised), tool policy rules, input extractors
- `src/security/`: SecureFs (TOCTOU-safe), bash AST analysis, Landlock/Seatbelt sandbox, resource limits
- `src/mcp/`: MCP client (stdio + SSE), manager, tool cache (TTL), resource queries, reconnect policy
- `src/orchestration/`: coordinator, agent directory, inter-agent messaging, worker constraints
- `src/skills/`: skill index, runtime, progressive disclosure, on-demand loading
- `src/subagents/`: subagent index, builtin agents, delegation runtime
- `src/tokens/`: TokenBudget (cache_creation_tokens), context window, pricing tiers, tracker
- `src/budget/`: BudgetTracker, OnExceed policy, tenant budgets, cost reporting. `calculate_ir()` for IR-native pricing.
- `src/scheduling/`: CronScheduler (interval + cron expressions), RemoteTrigger (async execution)
- `src/events/`: non-blocking EventBus (fire-and-forget), event kinds, subscriptions
- `src/observability/`: metrics (counter/gauge/histogram), OpenTelemetry bridge, spans
- `src/hooks/`: HookManager (blocking, fail-closed), command hooks, lifecycle events
- `src/context/`: PromptOrchestrator, static context, memory loading, rule index
- `src/models/`: model registry, specs, builtin model definitions
- `src/config/`: file/env/memory config sources, composite config, validation
- `src/types/`: (legacy) Message, ContentBlock, ToolDefinition, ApiResponse, Usage — being replaced by `src/ir/`
- `src/common/`: IndexRegistry, ContentSource, frontmatter parsing, named traits
