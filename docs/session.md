# Session Management

Sessions are modeled as graphs, not flat chat transcripts.

## Core Model

- `SessionGraph` is canonical state.
- `Session.messages` is a compatibility projection.
- Branching, replay, export, bookmarks, and checkpoints operate on graph state.

## Session Features

- branch from any node
- replay from any node
- export current branch as structured data or HTML
- create checkpoints for stable milestones
- create bookmarks for reusable navigation points

## Persistence Backends

- `MemoryPersistence`
- `JsonlPersistence`
- `PostgresPersistence`
- `RedisPersistence`

All backends are expected to preserve graph-first semantics and rebuild message projections from graph state.

## Compaction Policy

Compaction preserves graph history.

- A summary node is appended to the graph.
- A compaction checkpoint is recorded.
- The compatibility message projection may be reduced for downstream message-based flows.

## Programmatic Use

```rust
use claude_agent::session::{Session, SessionConfig};

let mut session = Session::new(SessionConfig::default());
session.add_user_message("hello");

let messages = session.current_branch_messages();
let replay = session.replay_input(None);
let export = session.export_current_branch();
```

## Manager API

`SessionManager` provides creation, loading, replay, export, bookmarking, and branching helpers.

## Related Guides

- `architecture/session-graph.md`
- `memory-system.md`
