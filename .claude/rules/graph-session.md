---
paths:
  - "src/graph/**"
  - "src/session/**"
---

# Graph & Session Rules

- `SessionGraph` is the single source of truth. All other state is derived.
- Never mutate `Session.messages` directly as domain state — it is a projection rebuilt from the graph.
- Replay, export, bookmarks, checkpoints, and branching operate on graph nodes, not message lists.
- Persistence backends (JSONL, PostgreSQL, Redis) store graph state and rebuild projections from it.
- Session compaction preserves graph structure while reducing storage footprint.
