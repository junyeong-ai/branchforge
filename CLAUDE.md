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
