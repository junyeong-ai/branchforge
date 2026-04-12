# Verified Structure Snapshot — 2026-04-12

Empirical code facts captured from direct reads. Findings that contradict entries here without re-verifying the file are auto-rejected. Update only when the fact has been re-read from the current code in the same PR.

## Exists and is wired

- `src/client/llm_call.rs` (295 LOC) — `LlmCall` trait + `RetryingClient` + `FallingBackClient` + `CircuitBrokenClient`. Re-exported at `lib.rs:129`.
- `src/common/extensions.rs` — `Extensions` TypeMap; used by `ExecutionContext`.
- `src/agent/config.rs:17-80+` — sub-configs (`AgentModelConfig`, `ExecutionConfig`, `SecurityConfig`, `BudgetConfig`, `IdentityConfig`, `PromptConfig`, `CacheConfig`, `ServerToolsConfig`, `StreamConfig`) already decomposed. `AgentBuilder::agent_config(AgentConfig)` accepts the full config.
- `src/events/bus.rs:178-188` — `OverflowPolicy` is `#[non_exhaustive]`, default `WarnAndDrop`.
- `src/session/session_handle.rs:60-125` — `SessionHandle(Arc<SessionHandleInner>)`. Single-session concurrent handle.
- `src/agent/policy/{iteration,tool_selection}.rs` — `IterationGate` + `ToolSelectionStrategy` traits wired into `execute_inner` and `AgentRuntime`.
- `src/mcp/mod.rs:493-502` — `McpToolAnnotations` with `read_only_hint`, `destructive_hint`, `idempotent_hint`, `open_world_hint`.
- `src/models/registry.rs:65-72` — `ModelRegistry::resolve` uses exact id + alias lookup only. No substring fallback.
- `src/client/transport/bedrock.rs:206-235` — `classify_error` parses JSON `__type` field. No `body.contains()`.
- `src/client/transport/foundry.rs:198-252` — `classify_error` parses JSON `error` + `error_description` fields.
- `src/observability/cache_break.rs:91` — `ToolSchemaChanged { tools: Vec<String> }`.
- `src/client/mod.rs:60-70` — `BackoffStrategy::jitter_fraction: f64` (replaces old `jitter: bool`).

## Baseline metrics

- 112k+ LOC across 310+ `.rs` files
- `cargo test --lib --all-features` → 1966 tests pass
- 8-gate CI: build, test, clippy -D warnings, fmt --check, doc -D warnings, pure-core build, FSM audit, enum audit
- Doc-debt: 198 files carry `#![allow(missing_docs)]` carve-out (monotone decrease target)
