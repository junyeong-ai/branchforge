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
- Provider capabilities are declared via `ProviderAdapter::profile()` returning `ProviderProfile`.
- Errors use typed enums (`SessionError`, `McpError`, `GraphError`). `Error::Config(String)` is intentional for developer-facing messages.
- Feature flags gate optional dependencies. Core SDK has zero cloud/DB deps.

## Key Areas

- `src/graph/`: session graph, replay, export, materialization, validation
- `src/session/`: session facade, persistence (memory, JSONL, postgres, redis), compaction, queueing, locking
- `src/agent/`: runtime loop, execution + streaming, task orchestration, delegation, builder
- `src/client/`: provider adapters, RetryPolicy, FallbackConfig, CircuitBreaker, streaming, batch, files
- `src/auth/`: credential resolution, OAuth token refresh, CLI credential storage, caching
- `src/tools/`: Tool/SchemaTool traits, registry, execution context, progress, cancellation
- `src/authorization/`: execution modes (auto/plan/supervised), tool policy rules, input extractors
- `src/security/`: SecureFs (TOCTOU-safe), bash AST analysis, Landlock/Seatbelt sandbox, resource limits
- `src/mcp/`: MCP client (stdio + SSE), manager, tool cache (TTL), resource queries, reconnect policy
- `src/orchestration/`: coordinator, agent directory, inter-agent messaging, worker constraints
- `src/skills/`: skill index, runtime, progressive disclosure, on-demand loading
- `src/subagents/`: subagent index, builtin agents, delegation runtime
- `src/tokens/`: TokenBudget (cache_creation_tokens), context window, pricing tiers, tracker
- `src/budget/`: BudgetTracker, OnExceed policy, tenant budgets, cost reporting
- `src/scheduling/`: CronScheduler (interval + cron expressions), RemoteTrigger (async execution)
- `src/events/`: non-blocking EventBus (fire-and-forget), event kinds, subscriptions
- `src/observability/`: metrics (counter/gauge/histogram), OpenTelemetry bridge, spans
- `src/hooks/`: HookManager (blocking, fail-closed), command hooks, lifecycle events
- `src/context/`: PromptOrchestrator, static context, memory loading, rule index
- `src/models/`: model registry, specs, builtin model definitions
- `src/config/`: file/env/memory config sources, composite config, validation
- `src/types/`: Message, ContentBlock, ToolDefinition, ApiResponse, Usage
- `src/common/`: IndexRegistry, ContentSource, frontmatter parsing, named traits
