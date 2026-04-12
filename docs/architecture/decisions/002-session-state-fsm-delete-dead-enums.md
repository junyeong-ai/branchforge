# ADR-002: Unified `SessionState` FSM; delete dead lifecycle enums

**Status:** Accepted  •  **Date:** 2026-04-11

## Context

Three parallel lifecycle enums existed:

1. **`session::SessionState`** — 9 variants (`Created`, `Active`,
   `WaitingForTools`, `Completing`, `Failing`, `Cancelling`,
   `Completed`, `Failed`, `Cancelled`). Actively used across
   `Session`, persistence, and `TaskTracker`.
2. **`agent::TaskStatus`** — 6 variants, a `From<SessionState>`
   projection used only by `TaskOutputResult.status`.
3. **`agent::SubagentState`** — 6 variants (`Spawning`, `Awaiting`,
   `Ready`, `Running`, `Finished`, `Failed`). Speculatively added
   as a "forward-looking" FSM for subagent handshake phases.
   **Zero production consumers** at the time of the audit — every
   subagent ran in-process on the same tokio runtime with no
   process-level handshake to observe.

The initial W-7 proposal was to merge all three into a single
9-variant unified enum. On closer inspection this was worse than
the status quo: it baked zero-consumer variants (`Spawning`,
`Awaiting`, `Ready`) into a type nothing could emit, violating the
"no library types with zero consumers" invariant the surrounding
refactor was trying to restore.

A second smell surfaced: the three finalizing states
(`Completing`/`Failing`/`Cancelling`) were almost collapsed into a
single `Finalizing` variant. That would have been a regression — the
3-way split encodes **terminal intent at the type level** (if you
are in `Completing`, you are guaranteed to end in `Completed`), and
`terminal_from_finalizing()` is a clean 1-to-1 mapping. Collapsing
the states would have traded enum bloat for struct bloat (a
separate `pending_outcome` field) and weakened the invariant.

## Decision

**Delete, do not merge.** The resulting shape:

1. **Delete `agent::SubagentState`** outright — file removed,
   re-exports stripped, the one regression test replaced with a
   `SessionState::transition_to` round-trip. No callers existed.
2. **Delete `agent::TaskStatus`** — `TaskOutputResult.status`
   becomes `Option<SessionState>` (with `None` = task not found).
3. **Strengthen `session::SessionState`** into a real FSM:
   - `Active` → `Running` (matches the `is_running()` helper that
     was already used in 9 places).
   - `WaitingForTools` removed — tool-wait is an observable event
     on the graph, not a top-level lifecycle phase.
   - Add `transition_to(self, next) -> Result<Self, SessionTransitionError>`
     with a validated forward DAG.
   - Keep the three finalizing variants intact.
4. **`Session::transition(next)`** is the only mutation entry point
   for `Session.state`; direct field assignment is forbidden
   outside the module.
5. **`Session::reset_for_resume()`** is the single documented escape
   hatch from the forward-only FSM, used exclusively by the
   `TaskTracker` resume path to rehydrate terminal tasks.
6. **`Persistence::finalize(id, terminal)`** replaces the unchecked
   `set_state(id, state)` trait method. Internally drives the FSM
   through `Session::finalize` for in-memory backends and uses a
   `state NOT IN (terminal…)` SQL guard on Postgres.

Final variant set (8): `Created`, `Running`, `Completing`,
`Failing`, `Cancelling`, `Completed`, `Failed`, `Cancelled`.

Transition graph:

```text
Created ──▶ Running ──┬─▶ Completing ──▶ Completed
                      ├─▶ Failing ─────▶ Failed
                      └─▶ Cancelling ──▶ Cancelled

Failing and Cancelling are reachable from any non-terminal state
(hard-abort and user-cancel lanes).
```

## Consequences

- **Invariant enforcement**: every transition now passes through
  `transition_to`, so illegal moves are caught at the type level.
- **One source of truth**: `TaskTracker`, persistence, and session
  management all speak the same lifecycle vocabulary.
- **Wire format break**: the Postgres `state` column and serialised
  session snapshots now use `running` / `completed` instead of
  `active` / `completed`. The CHANGELOG carries the one-line
  migration SQL. No in-code lenient parser — this is a clean break.
- **Forward-only**: resume is an explicit API (`reset_for_resume`),
  not an accidental side effect of a permissive `set_state`.
- **Smaller public surface**: three enums → one.

## Alternatives rejected

- **9-variant unified enum including `Spawning`/`Awaiting`/`Ready`.**
  Bakes zero-consumer variants into a canonical type. Rejected.
- **Collapse `Completing`/`Failing`/`Cancelling` into a single
  `Finalizing` variant with a separate `outcome` field.** Weaker
  invariant, more struct fields, no meaningful win.
- **Leave `TaskStatus` as a projection.** Two enums that are
  isomorphic modulo the `NotFound` sentinel is a DRY violation and
  forces every consumer to carry `From`/`Into` conversions.
- **Keep a lenient `from_str_lenient` loader.** Would have allowed
  gradual migration, but the project's no-backwards-compat stance
  makes the one-line SQL migration cheaper than a permanent parser.
