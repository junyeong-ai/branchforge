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

## F-rej-017 · "Global persist mutex → per-session DashMap"
- **Origin**: Round-3 analysis (Phase 6 task D5)
- **Refutation**: `Agent.persist_serializer` at `executor.rs:29` is `Arc<Mutex<()>>` — per-Agent instance, not global. Each Agent wraps one session. Multi-tenant: each agent has its own serializer with zero cross-agent contention.

## F-rej-016 · "persist_session_state called 3-5× per turn — reduce frequency"
- **Origin**: Round-3 analysis (Phase 6 task D11)
- **Refutation**: Of 7 calls in execute_inner, 2 are `_detached()` (non-blocking fire-and-forget via persist serializer mutex), 2 are bookends (initial user message + final flush), 1 is post-compaction (significant state change), 1 is post-shutdown, 1 is post-skill. The detached calls at `execution.rs:539,826` are explicitly documented as mid-turn crash-safety saves that don't block the LLM iteration. Per-turn cost: 0 blocking calls in the hot loop (response + tool results are detached), 1 await at compaction boundary. Already optimal.

## F-rej-015 · "OTEL span attributes missing on ProviderClient/Tool/execute_inner"
- **Origin**: Round-3 analysis (Phase 5 OTEL task)
- **Refutation**: `ProviderClient::send` at `provider_client.rs:113` creates `ApiCallSpan::with_system(model, codec_id)` with `gen_ai.request.model`, `gen_ai.system`, full usage + cache + reasoning tokens, latency, error category. `ToolRegistry::execute_with_progress` at `registry.rs:151` creates a `tool.execute` span with `tool.name`, `tool.duration_ms`, `tool.error`, `error.category`. `Agent::execute_inner` has `#[instrument(fields(session_id))]`. All stable OTel semantic conventions are covered.

## F-rej-014 · "CostLedger SSoT needed to replace 3-way cost accumulation"
- **Origin**: Round-3/4 analysis (Phase 5 CostLedger task)
- **Refutation**: The 4 accumulators (total_usage, session.total_usage, metrics, budget_tracker) serve **intentionally different scopes** (per-turn / per-session / per-turn-metrics / per-agent). All receive the **same** `ir_usage` object in the same call chain (`accumulate_response_usage` at `src/agent/common.rs:234`). There is no parallel computation from different sources, so drift is structurally impossible. A CostLedger would be an abstraction over 4 different-scoped views, not a simplification.

## F-rej-013 · "HookEvent manifest subscription + O(1) dispatch cache needed"
- **Origin**: Round-3 analysis (task 4-5)
- **Refutation**: `Hook::events(&self) -> &[HookEvent]` is the manifest. `HookRegistry::rebuild_cache()` at `src/hooks/manager.rs:40-52` builds a `HashMap<HookEvent, Vec<usize>>` dispatch cache on every register/unregister. `hooks_for_event(event)` at line 86 does O(1) lookup. `HookEvent` is `#[non_exhaustive]`.

## F-rej-012 · "SubagentTemplate trait needed to make subagents pluggable"
- **Origin**: Round-3 analysis (task 4-4)
- **Refutation**: `SubagentIndex` is already the template — `::new(name, desc).source(prompt).tools([...]).model_type(...)` creates an arbitrary subagent definition. `AgentBuilder::subagent(index)` registers custom subagents. `builtin_subagents()` is just a convenience for the 4 built-in definitions. No enum dispatch, no sealed type. Users can define and register new subagent types today.

## F-rej-011 · "AgentBuilder needs ConfigFragment trait and 76→15 method reduction"
- **Origin**: Round-3/Round-4 analysis (task 3-1..3-5)
- **Refutation**: `AgentConfig` already HAS sub-config fluent setters (`.model()`, `.execution()`, `.security()`, `.budget()`, `.prompt()`, `.cache()`, `.identity()`, `.stream()`) at `src/agent/config.rs:563-599`. `AgentBuilder::agent_config(config: AgentConfig)` at `builder.rs:206` already accepts a full config. Individual builder setters (`.model()`, `.tools()`, `.working_dir()`, etc.) are OPTIONAL convenience wrappers that don't need to exist for the pattern to work — adding a new field to a sub-config struct does NOT require adding a builder method.
- **What IS true**: the builder has 78 methods, which is large. But the growth problem ("new config field = new builder method") is already solved by the sub-config pattern. The convenience methods are ergonomically valuable for common 2-3 field use cases. Deleting them would harm discoverability.
- **Remaining action**: remove truly unused individual setters (0 external callers) and document the sub-config path as the preferred approach for complex configuration.

## F-rej-010 · "authorization/rules.rs + dsl.rs is a dual rule source — merge"
- **Origin**: Round-3 analysis (task 2-2)
- **Refutation**: `rules.rs` (738 LOC) is the policy **evaluator** (`ToolPolicy → PermissionDecision`). `dsl.rs` (646 LOC) is the DSL **parser** (`string → PermissionRuleSyntax`). They are a pipeline (`dsl parses → rules evaluates`), not a dual system. Merging them would violate single-responsibility.
- **Lesson**: Two files touching the same domain is not automatically a dual system. Check whether they have distinct responsibilities in a pipeline before proposing a merge.

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
