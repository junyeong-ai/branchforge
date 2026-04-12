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

## Verified clean per axis (2026-04-12)

Re-verified areas. Future reviews should skip these unless the code has changed since the date above.

### architecture axis
- SSoT: no `cached_messages`, `message_snapshot`, `branch_messages_cache` on Session/SessionHandle/SessionGraph
- Feature-gate: `pub mod security` gated `#[cfg(feature = "local-fs")]` in lib.rs:88; `pub mod bash` gated `#[cfg(feature = "coding-tools")]` in security/mod.rs:13
- Lock ordering: no lock held across `.await` on user-supplied future (persistence mutation_lock holds across internal save, not user closures — closures are sync `FnOnce(&mut Session)`)
- RAII shutdown: no `tokio::spawn` captures `Arc<AgentRuntime>` (child tokens via `shutdown.child_token()`)
- Direct graph mutation: only persistence.rs:220 (inside `with_session_lock`) and test code
- Dual public/internal export: clean
- Cyclic imports: clean (session→graph ok, no graph→session or tools→session)

### provider-graph axis
- 3-axis orthogonality: codecs pure (no I/O), transports stateful (auth/TLS)
- Capability honesty: all 5 codecs verified via codec_contract.rs tests
- Schema pipeline: all codecs use shared `prepare_schema()`/`prepare_tool_schema()`
- Streaming: all decode within `decode_stream_chunk`/`decode_eventstream_frame`
- Tool-pair integrity: `archive_before()` uses `tool_pair_adjusted_watermark()` walk-back
- Token drift: clean — `current_input_tokens` set only via `update_usage()` method
- Persistence schema versioning: all backends write `SessionSchemaVersion::CURRENT`

### tools-naming axis
- All 5 canonical FSMs correctly lack `#[non_exhaustive]`
- Manager/Registry/Tracker boundaries clean (SessionManager=lifecycle, HookRegistry=no lifecycle, TaskTracker=keyed map)
- No `#[allow(dead_code)]` on production code (only in `#[cfg(test)]`)
- No `// removed`, `// deprecated`, `// TODO migrate` markers
- Skills vs Plugins: distinct systems (runtime prompt injection vs namespace resource loading)

### agent-loop axis
- Cost accumulation: single funnel through `accumulate_response_usage()` in common.rs:234
- ModelRegistry::resolve: exact match only (registry.rs:65-72), no substring
- No hot-reload config code
- emit_simple: only Custom("cost_report") and Custom("context_recovery") in non-test code (SessionChanged fixed to emit_typed)

### info-hygiene axis
- ExecutionMetadata: all 9 Option fields have `#[serde(skip_serializing_if = "Option::is_none")]`
- ProviderOptions + all sub-structs: all fields have skip_serializing_if
- Tool descriptions: not duplicated between ToolDefinition.description and system prompt
- Dynamic rules: boundary marker is documented design trade-off (cache correctness > cache efficiency)
