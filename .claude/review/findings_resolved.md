# Rejected & Resolved Findings Ledger

Findings already rejected with evidence or merged via PR. Design reviews must check this file before proposing a finding. Re-submission requires new refuting evidence.

# Rejected Findings

## F-rej-001 · "LlmCall/Retry/FallingBack/CircuitBroken decorators are declared but not implemented"
- **Origin**: Round-1 provider-review subagent
- **Refutation**: `src/client/llm_call.rs` exists (295 LOC). `lib.rs:129` re-exports `LlmCall`, `RetryingClient`, `FallingBackClient`, `CircuitBrokenClient`.
- **Lesson**: Any "X does not exist" finding requires `Read` verification before submission.

## F-rej-002 · "Rename AgentState → AgentStatus (FSM vs classifier confusion)"
- **Refutation**: `.claude/rules/naming.md` closed-list explicitly lists `AgentState` as an FSM with `transition_to`. It is not a classifier.
- **Lesson**: Before any rename proposal, read the `naming.md` open/closed-list in full.

## F-rej-003 · "Change EventBus OverflowPolicy default to BlockWithTimeout"
- **Refutation**: `.claude/rules/events.md` — *"Never block the emitter on a slow subscriber."* Fire-and-forget is a load-bearing invariant.
- **Correct fix**: default → `WarnAndDrop` (still non-blocking) + `dropped_total` counter per subscriber. See P5-4.
- **Lesson**: Default-value proposals must read the module's `.claude/rules/*.md` first.

## F-rej-004 · "Add versioned cache to Session::current_branch_messages()"
- **Refutation**: `.claude/rules/graph-session.md` — *"rebuilds from the graph on every call. There is no cached message field."*
- **Correct fix**: reduce `persist_session_state` call frequency (P6-1), not cache.
- **Lesson**: `graph-session.md` is mandatory reading for any Session performance proposal.

## F-rej-005 · "Rename SessionHandle → SessionHandleTracker"
- **Refutation**: `.claude/rules/naming.md` — *"Tracker = runtime state **map** keyed by dynamic ids."* `SessionHandle` is a single-session concurrent handle (`Arc<SessionHandleInner>` wrapping `{session, executions, input_queue, execution_lock, executing, queue_notify}`), not a map.
- **Correct rename**: `SessionHandle`. See P2-1.

## F-rej-006 · "Introduce ConfigFragment trait to decompose AgentConfig"
- **Refutation**: `src/agent/config.rs:17-80+` shows sub-configs (`AgentModelConfig`, `ExecutionConfig`, `SecurityConfig`, ...) are already decomposed. The defect is the builder's 76-method surface, not config structure.
- **Correct fix**: collapse builder to one setter per sub-config. See P3-1.

## F-rej-008 · "GraphError/McpError/OverflowPolicy missing #[non_exhaustive]"
- **Origin**: Round-2/3 planning
- **Refutation**: Phase 0-2 empirical audit (2026-04-12, commit <pending>) found:
  - `GraphError` at `src/graph/error.rs` — already `#[non_exhaustive]` ✓
  - `McpError` at `src/mcp/mod.rs:533` — already `#[non_exhaustive]` ✓
  - `OverflowPolicy` at `src/events/bus.rs:178` — already `#[non_exhaustive]` ✓
  - Only real violation: `ProviderErrorKind` at `src/lib.rs:388` (1 item, fixed in Phase 0-2)
- **Lesson**: Before proposing "missing attribute X on enum Y", grep the enum definition directly. Don't trust cross-session memory of enum attribute state.

## F-rej-009 · "Demote GraphNode, NodeKind, ReplayInput from public API"
- **Origin**: Round-3 analysis
- **Refutation**: `SessionGraph::nodes()`, `children_of()`, `replay_slice()`, `branch_nodes()`, `current_branch_nodes()` all return `&GraphNode`/`Vec<&GraphNode>`. `SessionManager::replay_input()` returns `ReplayInput`. `Agent::execute_with_replay()` accepts `ReplayInput`. `ExportNode` embeds `NodeKind`. These are **legitimate public API** used for graph introspection.
- **Correct scope**: only `GraphMaterializer` is truly internal (zero-state rebuild mechanism, no public method signature, persistence-backend-only). Demoted to `pub(crate)` + dead `empty()` deleted.

## F-rej-007 · "Extract a single LoopPolicy god trait (iteration + tool selection + recovery)"
- **Refutation**: Recovery already exists as `RecoveryRecipes`. Tool approval already exists via `HookRegistry::PreToolUse`. A god trait would create a dual system with existing extension points (invariant #5).
- **Correct fix**: two small traits — `IterationGate`, `ToolSelectionStrategy`. Keep `RecoveryRecipes`. See P4-1/P4-2.

# Resolved Findings

Empty until first phase PR merges. Format when populated:
```
## F-res-NNN · <title>
- **Commit**: <sha>
- **Merged**: <date>
- **Phase**: <phase id>
- **Evidence**: <before→after file:line>
```
