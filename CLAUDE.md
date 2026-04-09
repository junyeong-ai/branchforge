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
- Provider capabilities are declared via `ProviderCapabilities` (in `src/ir/capabilities.rs`) with `Unsupported` defaults. Each codec returns a `const` value. Capability declarations must be honest — if a codec advertises `json_schema: Native` it must actually emit `response_format` in `encode_request`.
- The internal IR (`src/ir/`) is provider-neutral. Codecs translate between IR and wire format, emitting `ModelWarning` for lossy encodes.
- Errors use typed enums (`SessionError`, `McpError`, `GraphError`). `Error::Provider { kind, hint }` carries actionable hints for well-known failures. `Error::Config(String)` is intentional for developer-facing messages.
- HTTP error classification is distributed: each `ModelTransport` implements `classify_error(status, body)` for vendor-specific patterns (Vertex quota project, Bedrock throttling, Foundry Entra refresh hints). The central `provider_client::classify_response_error` only delegates — adding a new transport never requires editing the central function.
- Feature flags gate optional dependencies. Core SDK has zero cloud/DB deps.
- **Token counts are `u64` end-to-end**. `ir::Usage`, `AgentMetrics.{input,output,cache_*}_tokens`, `ExecutionMetadata.usage`, `TaskExecutionSummary.usage`, and `pricing::PricingTable::calculate` all operate on `u64` / `ir::Usage`. There is no longer a u32 wire DTO in the public surface.
- **Tool call linkage uses `tool_call_id`** end-to-end (`ToolCallRecord`, `ToolResultMeta`, graph node payloads, OTel spans).
- **Session/agent layer uses IR types end-to-end**. `SessionMessage`, `Session::current_branch_messages()`, `AgentResult.messages`, `ReplayInput.messages`, and all content overrides/compaction use `ir::Message`, `ir::ContentPart`, `ir::Role`. Cache breakpoints live at the codec/transport layer.
- **`SessionGraph` is the single source of truth** — and the compiler enforces it. `Session.messages` no longer exists as a cached field; `Session::current_branch_messages()` always rebuilds from the graph. `Session.graph` is `pub(crate)` so external code cannot mutate the SSoT directly; use `Session::graph()` for read access and the `add_message`/`fork_at`/`bookmark_*`/`checkpoint_*` methods for mutations.
- **`ToolDefinition` is the wire-format IR type** (`ir::ToolDefinition`). The local runtime spec (with `defer_loading` and token estimation) is `types::ToolSpec` — distinct name, distinct purpose. Conversion happens in `RequestBuilder::build()`.
- **Cancellation propagation**: The agent execution loop wires `runtime.shutdown.child_token()` into `ToolRegistry::execute_with_cancel`, so graceful shutdown aborts in-flight tools instead of waiting for their natural completion. Streaming execution already used the same pattern.

## Key Areas

- `src/ir/`: **provider-neutral IR** — `ModelRequest`, `ModelResponse`, `ContentPart`, `FinishReason`, `Usage`, `ModelStreamChunk`, `ModelSettings`, `ProviderOptions`, `ProviderCapabilities`, `ModelWarning`. This is the canonical representation for all LLM calls.
- `src/client/codec/`: **wire-format codecs** — `AnthropicMessagesCodec`, `OpenAiChatCodec`, `OpenAiResponsesCodec`, `GeminiGenerateCodec`, `BedrockConverseCodec`. Pure encoder/decoder, no HTTP/auth.
- `src/client/transport/`: **endpoint + auth** — `DirectTransport` (API key/bearer), `VertexTransport` (GCP ADC, publisher routing), `BedrockTransport` (SigV4), `FoundryTransport` (Azure Entra). `EndpointShape` bridge.
- `src/client/provider_client.rs`: `ProviderClient` — composition of one codec + one transport. `send()` and `send_stream()` pipelines with framing-aware streaming (SSE, AwsEventStream, JsonArray, NdJson).
- `src/client/preset.rs`: `Preset` enum — 8 named (codec × transport) presets with `build_from_env()` factories. `BRANCHFORGE_PROVIDER` env var selects preset.
- `src/graph/`: session graph, replay, export, materialization, validation
- `src/session/`: session facade, persistence (memory, JSONL, postgres, redis), compaction, queueing, locking
- `src/agent/`: runtime loop, execution + streaming, task orchestration, delegation, builder. `AgentBuilder::provider_client(pc)` wires a `ProviderClient` directly into the runtime.
- `src/client/`: provider stack — `codec/`, `transport/`, `provider_client.rs`, `preset.rs`, `llm_call.rs` (with `RetryingClient`/`FallingBackClient`/`CircuitBrokenClient` decorators). No monolithic `Client` type exists.
- `src/auth/`: credential resolution, OAuth token refresh (wired into `DirectTransport::refresh()` via `CredentialProvider`), CLI credential storage, caching
- `src/tools/`: Tool/SchemaTool traits, registry, execution context, progress, cancellation
- `src/authorization/`: execution modes (auto/plan/supervised), tool policy rules, input extractors
- `src/security/`: SecureFs (TOCTOU-safe), bash AST analysis, Landlock/Seatbelt sandbox, resource limits
- `src/mcp/`: MCP client (stdio + SSE), manager, tool cache (TTL), resource queries, reconnect policy
- `src/orchestration/`: coordinator, agent directory, inter-agent messaging, worker constraints
- `src/skills/`: skill index, runtime, progressive disclosure, on-demand loading
- `src/subagents/`: subagent index, builtin agents, delegation runtime
- `src/tokens/`: TokenTracker (uses `ir::Usage` directly), context window, pricing tiers
- `src/budget/`: BudgetTracker, OnExceed policy, tenant budgets, cost reporting. `calculate_ir()` for IR-native pricing.
- `src/scheduling/`: CronScheduler (interval + cron expressions), RemoteTrigger (async execution)
- `src/events/`: non-blocking EventBus (fire-and-forget), event kinds, subscriptions
- `src/observability/`: metrics (counter/gauge/histogram), OpenTelemetry bridge, spans
- `src/hooks/`: HookRegistry (blocking, fail-closed), command hooks, lifecycle events
- `src/context/`: PromptOrchestrator, static context, memory loading, rule index
- `src/models/`: model registry, specs, builtin model definitions
- `src/config/`: file/env/memory config sources, composite config, validation
- `src/types/`: tool executor contract — `ToolError`, `ToolInput`, `ToolOutput`, `ToolResult`, `ToolOutputBlock`, `ToolSpec` (local runtime spec, distinct from `ir::ToolDefinition`), plus `ModelUsage`/`UsageProvider` for rolled-up cost reporting and `ServerToolUse` for builtin-tool tracking.
- `src/common/`: IndexRegistry, ContentSource, frontmatter parsing, named traits
