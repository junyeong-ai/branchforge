# axis · agent-loop

## Defect patterns

### Loop control
- `execute_inner` body exceeding ~250 LOC (monolith re-growth)
- Iteration limit via magic `const MAX_* = N` instead of `IterationGate` / `*Config`
- Structured-output retry budget hardcoded (not configurable via gate)
- Tool call planning inlined instead of delegated to `ToolSelectionStrategy`

### Cost / observability
- Cost accumulated in more than one place without a single `CostLedger` owner
- Cache-hit discount calculated outside the ledger
- OTEL span missing stable attributes (`provider`, `model`, `tool_name`, `duration_ms`)
- Metric defined but never `inc()`/`record()`'d
- `emit_simple` used for a built-in `EventKind` (should be `emit_typed`)

### Model / registry
- `ModelRegistry::resolve` substring fallback (`contains("opus"|...)`) — invariant #7
- Runtime model extension mutating the global `OnceLock` directly (use overlay pattern)
- Model capability table (context window, cache support, vision) hardcoded per codec instead of `ModelSpec`

### Recovery / HITL
- Recovery path skipping `RecoveryRecipes`
- HITL denial without `RecoveryAction::RequestApprovalAgain` transition
- Auth refresh mid-turn losing request state

### Orchestration
- Worker state transitions without FSM
- `tokio::spawn` without `shutdown_token` join (escapes RAII shutdown)
- Hot-reload config code — **should not exist** (explicitly rejected in v8 work plan)

## Rules to load additionally

- `.claude/rules/events.md`
- `.claude/rules/naming.md`

## Stopping criterion

`execute_inner` and all direct callers audited. Cost accumulator call sites grepped. OTEL instrument attributes grepped.
