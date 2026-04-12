# ADR-007: Checkpoint captures Session + Budget, nothing else

**Status:** Accepted  •  **Date:** 2026-04-11

## Context

`AgentCheckpoint` was originally a speculative struct that captured
`iteration`, `api_calls`, `tool_calls`, `total_cost_usd`,
`execution_mode`, and `budget_remaining` — six fields promising
"everything needed to resume an agent after a crash."

Deep audit found the implementation was a **fake**:

1. `Agent::checkpoint()` hardcoded `api_calls: 0`, `tool_calls: 0`,
   `budget_remaining: None` — three fields that were literal zeros
   on every call.
2. `total_cost_usd` was read from `Session::total_cost_usd()`, a
   field the agent runtime never wrote to during execution (only
   persistence reload paths updated it).
3. `AgentBuilder::resume_from(checkpoint)` only restored
   `session_id` and `execution_mode` — the budget field was
   completely ignored on the restore path.

The serde round-trip tests passed because they never exercised the
capture or restore paths, only the type's own serialization.

Adding plumbing to populate the fake fields was possible (wire
`Agent.metrics` behind a `Mutex`, seed it on resume) but the
underlying question was: **what state actually survives across
`execute()` calls?** Once that question was framed correctly, the
answer was much smaller than the original struct implied.

## Decision

`AgentCheckpoint` captures exactly **two** pieces of state, plus one
configuration mirror and one audit timestamp:

```rust
pub struct AgentCheckpoint {
    pub session_id: SessionId,          // ← SessionManager reloads
    pub execution_mode: ExecutionMode,  // ← config mirror
    pub budget_spent_usd: Decimal,      // ← BudgetTracker::restore_spent
    pub session_usage: Usage,           // ← audit / dashboard continuity
    pub created_at: DateTime<Utc>,
}
```

Everything the old struct carried (iterations, api_calls, tool_calls)
is **per-run DTO state** that lives on `AgentMetrics`, returned from
each `execute()` call, and has no meaningful cross-boundary identity.
Putting it in a checkpoint would falsely imply `execute()` is not
idempotent across process restarts — it is, and should remain so.

The two truly persistent pieces:

1. **`Session`** — graph, usage, todos, plan, cached context.
   `SessionManager` persistence already handles this. The checkpoint
   only carries `session_id` so a fresh process can reload it.

2. **`BudgetTracker`** — cross-session cost accumulator. A fresh
   tracker built after restart would start at zero, which would
   defeat `max_cost_usd` enforcement. The checkpoint captures
   `used_cost_usd()` and the restore path seeds it via
   [`BudgetTracker::restore_spent`] (new API).

`AgentBuilder::resume_from(checkpoint)` wires all three restorations:
session reload (via `resume_session_id`), execution-mode reset, and
budget restoration. The restoration happens **after** the agent
runtime is fully assembled and **before** `execute()` is called, so
new usage accumulates correctly on top of the seeded baseline.

## Consequences

- **Smaller, honest type.** Five fields instead of seven, every one
  backed by a real source of truth.
- **`Agent::checkpoint()` reads from the actual accumulators**:
  `runtime.budget_tracker.used_cost_usd()` and
  `session.total_usage().clone()`. No more zeros.
- **`BudgetTracker::restore_spent(cost: Decimal)`** is a new public
  API. Internally it does the same `used_cost_bits.fetch_add` as
  the `record` path, so the existing overflow guard applies.
- **`Agent::restore_budget_spent(cost)`** is `pub(crate)` — a
  builder-time-only helper that enforces "restore exactly once,
  before the first request." Calling it after `execute()` would
  double-count the freshly-recorded usage.
- **Regression test** (`agent_checkpoint_budget_restore_round_trip`
  in `tests/general_purpose_sdk_regression.rs`) proves end-to-end:
  pre-crash → serialize → post-restart → seeded tracker equals
  live-accumulated tracker.

## Alternatives rejected

- **Fix the six-field struct in place** by wiring `AgentMetrics`
  persistence. Would have required a `Mutex`-wrapped metrics field
  on `Agent`, a first-call-only restore gate, and duplicate state
  across the checkpoint and the per-run `AgentMetrics`. All of that
  to carry information that is already observable from the session
  graph (`iterations ≈ message_count / 2`).
- **New persistence-backend method** `save_checkpoint(session_id, ckpt)`.
  Would have coupled checkpoint storage to the `Persistence` trait,
  which is intentionally narrow. Caller-side serialization keeps
  the trait clean and lets consumers pick their own storage (disk,
  KV, DB).
- **Automatic checkpoint on interval or shutdown.** Can be layered
  on top of the explicit `Agent::checkpoint()` API if needed; not
  part of the minimum viable design.
