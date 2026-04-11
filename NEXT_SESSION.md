# Next Session — Resume Plan

> **Status**: 0.9.0 baseline. The current session committed Phase 1 (4-layer architecture) + Phase 2 correctness + a deep architectural overhaul of the provider stack, recovery framework, permission DSL, and budget surface. **12 work items closed in the previous session** (W-1, W-2, W-3, W-4, W-5, W-6, W-8, W-11, W-13, W-14, W-15, W-23). **11 work items remain**, listed below in execution order.

## Constraints carried over from the last session

1. **No backwards compatibility.** Delete legacy code aggressively. Treat the codebase as if it had been designed this way from the start.
2. **100% fact-based.** Verify every claim with grep / cargo check. Do not invent function signatures or behavior.
3. **Long-term maintainable.** Each change must be flexible, extensible, follow OCP/SRP/DRY. No tactical patches.
4. **Each work item is integrated.** Do **not** add library types that nothing consumes — that was the failure mode of the prior shipping pass and is what the meta-review caught.

## Quality gates (all must pass before declaring an item done)

```bash
cargo test --lib --all-features        # 1815+ tests
cargo test --lib --no-default-features # 1451+ tests
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --lib --no-deps --all-features
```

## Remaining work items (in execution order)

### **W-7 — Unified task/session FSM** ★ largest scope, do first

**Goal**: Collapse `TaskStatus` (6 variants), `SessionState` (9 variants), and `SubagentState` (6 variants) into a **single canonical FSM**. Today three parallel enums describe overlapping subsets of the same lifecycle, and the codebase has implicit conversions between them via `From` impls.

**Approach**:
- Decide the unified variant set. Recommendation: **8 variants** —
  `Created`, `Spawning`, `Awaiting`, `Ready`, `Running`, `Cancelling`, `Finished`, `Failed`, `Cancelled`.
  (Drop `Finalizing` from `TaskStatus` and `WaitingForTools` from `SessionState`; both can be subsumed by `Running`.)
- Move the unified enum to `src/agent/lifecycle.rs` (or rename `subagent_fsm.rs` → `lifecycle.rs`).
- Delete `agent::TaskStatus` from `task_output.rs`. Delete `session::state::enums::SessionState`.
- Migrate every callsite — `TaskRegistry`, `task_output.rs`, `session::manager`, `session::state`, persistence backends (Postgres, Redis JSON serialization).
- All mutations go through `transition_to(next)` — no direct field assignment.
- Persistence schema migration: status column → varchar of new variants. Document migration in CHANGELOG.

**Risk**: Persistence backends (Postgres, Redis) serialize the old enums. Plan a one-shot schema change.

**Acceptance**: `grep TaskStatus` → 0. `grep SessionState` → 0. All tests pass. Persistence backends still round-trip.

**Estimated scope**: 20+ files, ~500 LOC delta.

---

### **W-9 — `AgentContract<C>` integration into TaskTool**

**Goal**: Make `TypedAgentInvoker<C>` consumable through the same `TaskTool` registration path as untyped subagents — currently it's a standalone wrapper with no integration into the agent runtime.

**Approach**:
- Add `TaskTool::register_typed::<C: AgentContract>()` builder method
- Subagent dispatch checks the registered contract type and routes typed input through `TypedAgentInvoker::encode_prompt`
- Untyped path (`TaskInput { prompt: String }`) becomes a specialization of `AgentContract<Input=String, Output=Value>`
- Add an end-to-end test using a custom `ResearchContract` that returns `Vec<Citation>`

**Acceptance**: Agent loop can call a typed subagent and get back a typed `C::Output` with no `serde_json::from_value` boilerplate at the callsite.

**Estimated scope**: ~150 LOC, 1 new test file.

---

### **W-10 — Gemini cache lifecycle wiring**

**Goal**: The `gemini_cache.rs` helpers exist but are stateless functions. Wire them into a real lifecycle struct that uses `DirectTransport` for auth and produces `cachedContents/<id>` strings consumable by `GeminiOptions::cached_content`.

**Approach**:
- Create `src/client/cache/gemini.rs` (move from `src/client/codec/gemini_cache.rs`)
- Add `GeminiCacheClient { http, transport }` with `create / get / delete` async methods
- The transport handles auth (DirectAuth::QueryParam for AI Studio, future Vertex via VertexTransport)
- Integration test using wiremock — verify the URL path / body / response parsing
- Update `src/client/codec/mod.rs` to remove the misplaced gemini_cache export

**Acceptance**: Calling `GeminiCacheClient::create(...)` against wiremock yields a `cache_name` that the agent can stash in `GeminiOptions.cached_content` and the next `send()` reflects in the wire body.

**Estimated scope**: ~250 LOC including tests.

---

### **W-12 — `RequestPipeline` extraction (DRY for streaming/unary)**

**Goal**: Both `agent/streaming.rs::do_start_request` and `agent/execution.rs` build a `BudgetContext`, run preflight, send the request, accumulate usage, run reconciler, and apply hooks. The exact same 5-step pipeline appears twice (with minor stream/unary variations).

**Approach**:
- New module `src/agent/request_pipeline.rs`
- `RequestPipeline { runtime, tool_state, ... }` struct
- Methods: `prepare(request) -> PreparedRequest` (preflight + estimate stash) and `record_outcome(prepared, response)` (reconcile + accumulate)
- Replace inline construction in streaming.rs + execution.rs
- Add `AgentRuntime::budget_context(&self) -> BudgetContext<'_>` accessor to fix the 10x inline construction issue
- Stream and unary remain separate top-level methods but share the pipeline scaffolding

**Acceptance**: `grep "BudgetContext {" src/agent/` → 1 hit (the accessor) instead of 10. Pipeline tests pass.

**Estimated scope**: ~200 LOC + refactoring streaming.rs and execution.rs.

---

### **W-16 — Replace minimal validator with `jsonschema` crate**

**Goal**: The hand-rolled `validate_structured_output` walks JSON manually and skips `pattern`, `format`, `allOf`, `anyOf`, `oneOf`, `if/then/else`, numeric constraints, length constraints. False negatives are subtle. Replace with the `jsonschema` crate.

**Approach**:
- Add `jsonschema = "0.20"` to Cargo.toml (~1MB binary footprint)
- Replace the hand-rolled walker in `src/client/schema/validate.rs` with `jsonschema::validator_for(&schema)?.validate(&value)`
- Map `jsonschema::ValidationError` to the existing `StructuredOutputValidationError` type
- Keep the JSON pointer in the error (jsonschema provides `instance_path`)
- Verify the existing 11 tests still pass + add tests for `pattern`, `oneOf`, `format`

**Decision**: Pure-core users that opt into structured output get full validation. The dep cost is acceptable.

**Acceptance**: `validate_structured_output` produces correct errors for all standard JSON Schema constructs. Existing test suite + 5 new tests all pass.

**Estimated scope**: ~100 LOC delta + 1 dep.

---

### **W-17 → W-22 — Naming consistency pass** (single PR)

Renames to perform:
- **W-17**: `mcp::LifecyclePhase` → `mcp::McpClientState` (uniform "State" terminology with `SubagentState`)
- **W-18**: Error type suffix unification:
  - `StructuredOutputValidationError` → `SchemaValidationError`
  - `PermissionRuleParseError` → `DslParseError`
  - `SubagentTransitionError` → `FsmTransitionError`
- **W-19**: `request_had_cache_markers` private fn → already done in W-4 as `ModelRequest::has_cache_markers()`. Verify no remaining `request_had_*` patterns.
- **W-20**: `compute_drift` / `record_estimate_drift` → already done in W-13 as `EstimateReconciler::{compute, observe}`. Verify.
- **W-21**: `TaskRegistry` → `TaskTracker` (it tracks runtime tasks; "Registry" implies init-time keyed lookup)
- **W-22**: Audit `McpToolsetRegistry` vs `McpManager` for redundancy. If overlapping, consolidate into `McpManager`.

**Approach**: Use `rust-analyzer`'s rename refactoring (or coordinated grep+sed). Run quality gate after each rename.

**Acceptance**: `grep` for old names → 0. All tests + clippy + rustdoc clean.

**Estimated scope**: ~30 file touches, mechanical.

---

### **W-24 — McpManager lock ordering documentation**

**Goal**: `McpManager` holds `Arc<RwLock<HashMap<String, McpClient>>>` AND `Arc<RwLock<DegradedReport>>`. No documented lock ordering → potential deadlock.

**Approach**:
- Document the rule in a module-level comment: **always acquire `servers` before `degraded`**
- Audit `add_server_tracked` and any other multi-lock callsite to confirm compliance
- Add a `#[cfg(debug_assertions)]` regression test that intentionally tries to acquire in reverse order with `try_write` and asserts failure detection (or document why none is needed)

**Acceptance**: Lock ordering rule documented in `src/mcp/manager.rs`. CLAUDE.md "Lock Ordering" section mentions MCP manager.

**Estimated scope**: ~30 LOC docs + 1 audit.

---

### **W-25 — `TaskTracker` integration with unified FSM**

**Depends on**: W-7 (unified FSM) and W-21 (TaskRegistry → TaskTracker rename).

**Goal**: After W-7 lands, the unified FSM lives at `agent::lifecycle::AgentLifecycleState` (or similar). `TaskTracker` (formerly `TaskRegistry`) currently mutates status via direct field assignment in `apply_transition`. Make all mutations go through `state.transition_to(next)`.

**Approach**:
- Find all `task_runtime.status = ...` callsites
- Replace with `task_runtime.state.transition_to(next)?`
- Reject illegal transitions at the type level

**Acceptance**: No direct status field assignments. All transitions validated.

**Estimated scope**: ~80 LOC delta.

---

### **W-26 — `is_container()` integration into sandbox decision**

**Goal**: `src/security/sandbox/detect.rs::is_container()` exists with 0 callsites. Wire it into the sandbox application path so agents running inside Docker/Kubernetes do not attempt nested namespace isolation (which would either fail or be redundant).

**Approach**:
- In `Sandbox::create_runtime` (or wherever the Linux Landlock path activates), check `is_container()` first
- If true, log a tracing event explaining the host already provides isolation and skip namespace-based policies
- Add a unit test verifying the log path runs (use `tracing-test`)

**Acceptance**: Running the agent inside a container does not attempt to call `unshare` or layer additional Landlock rules on top of the container's existing seccomp profile.

**Estimated scope**: ~50 LOC.

---

### **W-27 / W-28 / W-29 — Documentation + ADRs + test matrix expansion**

**W-27**: Update `.claude/rules/` files with new conventions:
- State machine terminology (`State` not `Phase`)
- Error type naming (`<Module>Error` for module-wide, `<Module><Op>Error` for specific)
- "No dual systems" rule — when adding a new abstraction, either integrate it or delete the legacy

**W-28**: New `docs/architecture/decisions/` directory with ADRs for the major decisions made in the last two sessions:
- **ADR-001**: ProfileRegistry over Preset enum (open-set vs closed-set)
- **ADR-002**: Unified lifecycle FSM (TaskStatus + SessionState + SubagentState merge — written **after** W-7)
- **ADR-003**: SystemBlockRole over magic-string sentinel
- **ADR-004**: jsonschema crate dependency (after W-16)
- **ADR-005**: RecoveryRecipe replaces RecoveryStrategy (pure decisions + side-effecting executor)
- **ADR-006**: 4-layer feature gating (pure core / local-fs / coding-tools / cloud)

**W-29**: Expand `tests/general_purpose_sdk_regression.rs`:
- 5 codec × cache marker leak regression (already added in W-1, verify still present)
- Recovery executor end-to-end with mock
- Unified FSM lifecycle round-trip (post-W-7)
- ProfileRegistry user-registered profile end-to-end
- DSL → ToolPolicy → enforcement round-trip

**Acceptance**: All three gates landed in a single docs PR.

**Estimated scope**: ~3 ADR files + ~10 KB rules updates + ~500 LOC test expansion.

---

## Recommended order (matches the prior plan dependency graph)

| # | Item | Why this order |
|---|---|---|
| 1 | **W-7** | Largest, blocks W-25, persistence schema migration risk — do early so the rest builds on the unified FSM |
| 2 | **W-25** | Wires `TaskTracker` to use the unified FSM mutation path |
| 3 | **W-9** | Builds on the unified FSM (TaskTool migration) |
| 4 | **W-10** | Independent, small |
| 5 | **W-12** | DRY refactor, easier after FSM unification |
| 6 | **W-16** | Independent dep addition |
| 7 | **W-26** | Independent, small |
| 8 | **W-24** | Documentation only |
| 9 | **W-17 → W-22** | Naming pass — do AFTER all renames, ONE consolidated PR |
| 10 | **W-27 / W-28 / W-29** | Docs + tests, last |

## Files to read first in the next session

When the next session opens, the agent should immediately read:

1. `NEXT_SESSION.md` (this file)
2. `CLAUDE.md` — design invariants
3. `.claude/rules/client.md`, `schema.md`, `ir.md` — module rules
4. `docs/architecture/layering.md` — 4-layer contract
5. `src/agent/recovery_recipes.rs` + `src/agent/recovery_executor.rs` — example of the "pure decision + side-effecting executor" pattern that W-7's lifecycle FSM should follow
6. `src/client/preset.rs` — example of OCP-compliant open registry that should guide W-9's typed subagent registry

## Suggested resume prompt (paste this verbatim into the next session)

> Continue the W-7 → W-29 work plan from `NEXT_SESSION.md`. Same constraints as the last session: no backwards compatibility, 100% fact-based (verify with grep/cargo before claiming anything), long-term maintainable design, **every work item must be fully integrated — no library types with zero consumers**. Run the full quality gate (`cargo test --lib --all-features`, `--no-default-features`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --all -- --check`, `RUSTDOCFLAGS="-D warnings" cargo doc --lib --no-deps --all-features`) after each work item. Read `NEXT_SESSION.md` and `CLAUDE.md` first. Begin with **W-7 (unified task/session FSM)** since it blocks W-9 and W-25 and has the largest blast radius — produce a short design note before touching code, listing the 8 unified variants and the persistence migration plan, and wait for my approval. After approval, execute W-7 → W-25 → W-9 → W-10 → W-12 → W-16 → W-26 → W-24 → naming pass (W-17–W-22) → docs (W-27–W-29) in that order.

## Current quality baseline (verify on resume)

```
cargo test --lib --all-features        → 1815 passed
cargo test --lib --no-default-features → 1451 passed
clippy --all-features                  → clean
fmt --check                            → clean
rustdoc --all-features                 → clean
```

If any of these regress on resume, **stop and diagnose before adding new work**. The baseline must be green.

## Work units committed in this session (for reference)

1. **chore(deps): bump to 0.9.0 + 4-layer feature gating** — Cargo.toml/lock
2. **feat(arch): Phase 1 — 4-layer architecture foundation** — `src/common/extensions.rs`, `src/workspace.rs`, `src/context/environment_source.rs`, `src/context/memory_content.rs`, `docs/architecture/layering.md`
3. **feat(client): provider stack overhaul** — ProfileRegistry (open registry, deletes Preset enum), 5 codecs, schema validation pipeline (`src/client/schema/`), streaming cancel propagation, cache marker detection, IR `routing_model_id`/`has_cache_markers`, `SystemBlockRole` typed boundary, `DirectAuth::None`
4. **feat(mcp): 11-phase lifecycle + DegradedReport** — `LifecyclePhase` enum, validated transitions, symmetric `BTreeMap<String, ServerHealth>`, `add_server_tracked`
5. **feat(budget): preflight + estimator + reconciler + policy rename** — `RequestTokenEstimate`, `EstimateReconciler::observe/compute`, `BudgetExceedPolicy` (renamed from OnExceed)
6. **feat(agent): recovery + FSM + contract + DSL + bash validator** — `RecipeRegistry` + `RecoveryExecutor` (deletes legacy `RecoveryStrategy`), `SubagentState`, `AgentContract`, permission `dsl` parser (deletes `from_scoped`/`allow_pattern`/etc), bash validator wiring
7. **test+example+docs**: regression tests, layered examples (customer_support/research/data_analysis/multi_provider/structured_output), README 4-layer section, layering.md, codec contract `system_block_boundary_must_not_leak`
