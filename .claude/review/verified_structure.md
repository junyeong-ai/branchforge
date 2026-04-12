# Verified Structure Snapshot — 2026-04-12

Empirical code facts captured from direct reads. Design-review findings that contradict entries here without re-verifying the file are auto-rejected.

## Exists and is wired (anti-hallucination)

- `src/client/llm_call.rs` (295 LOC) — `LlmCall` trait + `RetryingClient` + `FallingBackClient` + `CircuitBrokenClient`. Re-exported at `lib.rs:129`. **Resilience decorators are implemented, not stubs.**
- `src/common/extensions.rs` — `Extensions` TypeMap; used by `ExecutionContext`.
- `src/agent/config.rs:17-80+` — sub-configs (`AgentModelConfig`, `ExecutionConfig`, `SecurityConfig`, `BudgetConfig`, `IdentityConfig`, `PromptConfig`, `CacheConfig`, `ServerToolsConfig`, `StreamConfig`) are **already decomposed**. The builder surface is the problem, not config structure.
- `.claude/rules/naming.md` closed-list includes `AgentState`, `SessionState`, `PlanState`, `QueueItemState`, `McpClientState` — FSMs with `transition_to`, not classifiers.
- `src/events/bus.rs:178-188` — `OverflowPolicy` is **already** `#[non_exhaustive]`. Do not re-propose adding the attribute.
- `src/session/session_handle.rs:60-125` — `SessionHandle(Arc<SessionHandleInner>)` wraps `{session: RwLock, executions, input_queue, execution_lock, executing, queue_notify}`. Single-session concurrent handle, **not** a map keyed by dynamic ids.

## Known defects under treatment — phase mapping

| id | file:line | defect | phase |
|---|---|---|---|
| D1 | `lib.rs:144-148` | `GraphMaterializer`/`NodeKind`/`ReplayInput` leaked as public | P1-1 |
| D2 | `src/agent/options/builder.rs` (1488 LOC, 76 pub fn) | Builder surface bloat | P3 |
| D3 | `src/agent/execution.rs:136` (`execute_inner`, 884 LOC) | Loop monolith, extract `IterationGate`/`ToolSelectionStrategy` | P4-1..4-3 |
| D4 | `src/models/registry.rs:72-77` | `contains("opus"\|"sonnet"\|"haiku")` substring fallback | P7.5-1 |
| D5 | `src/security/bash/parser.rs:41-76` | regex + tree-sitter dual system | P7.5-2 |
| D6 | `src/client/transport/bedrock.rs:209-235` | `body.contains("ThrottlingException")` classification | P7.5-3 |
| D7 | `src/observability/cache_break.rs:244-258` | first-match-wins on multi-tool change | P7.5-4 |
| D8 | `src/session/compact/service.rs:16` | `DEFAULT_COMPACT_THRESHOLD=0.8` magic threshold | P7.5-5 |
| D9 | `src/client/mod.rs:85` | `0.15 * random` unnamed jitter factor | P7.5-6 |
| D10 | `src/events/bus.rs:272` | default `OverflowPolicy::Drop` (silent loss) | P5-4 → `WarnAndDrop` |
| D11 | `src/session/state/mod.rs:397-430` | `persist_session_state` called 3-5× per turn | P6-1 (reduce calls, **not** cache) |
| D12 | `src/session/session_handle.rs:125` | `SessionHandle` name hides concurrent session handle role | P2-1 → `SessionHandle` |
| D13 | `src/authorization/rules.rs` + `dsl.rs` | dual rule source | P2-2 |
| D14 | `src/prompts/base.rs:4-17` | `BASE_SYSTEM_PROMPT` always on (~80 tok/turn) | P5.5-1 |
| D15 | `src/context/static_context.rs:88-110` + `src/agent/request.rs:94-98` | MCP tool description duplicated (schema + text) | P5.5-2 |
| D16 | `src/ir/content.rs:139-192` | `ToolOutput::Empty → Text("")` still serialized | P5.5-4 |
| D17 | `src/session/state/message.rs:14-35` | `ExecutionMetadata` 8 optional fields missing `skip_serializing_if` | P5.5-5 |

## Baseline metrics

- 112,201 LOC across 310 `.rs` files
- `cargo test --lib --no-default-features` → 1,364 tests (pure core)
- 6-gate green on `main` @ commit 22af99a
- **Doc-debt** (post-P0-1): 198 files carry `#![allow(missing_docs)]` carve-out. `#![deny(missing_docs)]` is installed at `src/lib.rs:58`. Target: monotonic decrease to 0 by Phase 8. Each phase PR that touches a module MUST remove the carve-out for that module and document its public items as part of the PR.

## Refresh policy

Update an entry only when the fact has been re-read from the current code in the same PR. Never update from memory or intuition. Stale entries are worse than no entries.
