---
paths:
  - "src/graph/**"
  - "src/session/**"
---

# Graph & Session Rules

- `SessionGraph` is the single source of truth. All other state is derived.
- `Session::current_branch_messages()` rebuilds from the graph on every call. There is no cached message field.
- `SessionGraph::apply_event()` applies events in O(1). Full rebuild via `GraphMaterializer::from_events()` is only for loading from persistence.
- Replay, export, bookmarks, checkpoints, and branching operate on graph nodes, not message lists.
- `archive_before(watermark)` soft-archives old nodes — they stay in the graph for referential integrity but are excluded from message projection.
- Persistence backends (Memory, JSONL, PostgreSQL, Redis) store graph events and rebuild on load.
- `fork_session` uses `with_session_lock` to prevent stale-snapshot races.
