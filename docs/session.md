# Session Management

Sessions are modeled as graphs, not flat chat transcripts.

## Core Model

- `SessionGraph` is canonical state.
- `Session.messages` is a compatibility projection.
- Branching, replay, export, bookmarks, and checkpoints operate on graph state.
- Sessions can be scoped by `tenant_id` and owned by `principal_id`.

## Session Features

- branch from any node
- replay from any node
- export current branch as structured data or HTML
- create checkpoints for stable milestones
- create bookmarks for reusable navigation points
- explore branch, tree, bookmark, checkpoint, and node summaries through graph exploration APIs
- search graph nodes and derive graph-level session statistics

## Persistence Backends

- `MemoryPersistence`
- `JsonlPersistence`
- `PostgresPersistence`
- `RedisPersistence`

`JsonlPersistence` and `PostgresPersistence` are the graph-first backends. Their durable source of truth is graph events plus graph metadata, and message projections are rebuilt from graph payloads on load.

`RedisPersistence` is a support backend that stores full session snapshots for lightweight persistence, queue, and cache-oriented workloads. It preserves session state, but it is not the event-canonical backend.

## Concurrent Session Access

The `Persistence` trait provides `with_session_lock()` for atomic load-modify-save operations. Default implementations of `append_graph_event` and `add_message` use this method to prevent race conditions when multiple agents access the same session concurrently. `MemoryPersistence` overrides it with an internal write lock for true atomicity.

## Compaction Policy

Compaction preserves graph history.

- A summary node is appended to the graph.
- A compaction checkpoint is recorded.
- The active message projection is reduced to a summary-first view for downstream message-based flows.
- Full branch history remains available for replay, export, bookmarks, checkpoints, and future graph queries.

Compaction does not depend on preserving a fixed number of recent raw turns. Instead, the runtime summarizes the active branch and keeps graph history authoritative.

## Programmatic Use

```rust
use branchforge::session::{Session, SessionConfig};

let mut session = Session::new(SessionConfig::default());
session.add_user_message("hello");

let messages = session.current_branch_messages();
let replay = session.replay_input(None);
let export = session.export_current_branch();
```

## Manager API

`SessionManager` provides creation, loading, replay, export, bookmarking, branching, graph exploration, and graph search helpers.

For multi-tenant deployments, the manager can create sessions with explicit tenant and principal identity so session ownership, budgeting, and request metadata stay aligned.

Graph exploration and graph search can also be guarded by session access scope, allowing tenant/principal-aware access checks before replay, export, search, or statistics are returned.

Graph records can also carry creator and provenance metadata so bookmarks, checkpoints, and delegated subagent work remain traceable over time.

Policy-aware export is also available through `SessionExporter` so identity, provenance, and tool payloads can be included or redacted explicitly depending on audit and sharing requirements.

When provenance is included, export flows can also emit compact provenance digests so delegated work is easier to inspect without reading raw provenance fields everywhere.

For durability and operational recovery, the session module also exposes archive and restore-validation helpers:

- `SessionArchiveService` for canonical archive bundles
- `ArchivePolicy` for archive-specific inclusion rules
- `RestoreVerifier` for restore round-trip validation

Archive bundles preserve session lineage and can optionally carry a pending queue snapshot for restore-oriented workflows.

Archive restore is strict by default:

- restores do not silently overwrite an existing session id
- built-in backends verify the restored round-trip before publishing or committing it
- queue snapshots are replayed and verified during restore
- invalid bundles are rejected before durable publication

The graph exploration layer exposes:

- branch summaries
- tree views
- bookmark and checkpoint listings
- node summaries
- graph search results
- graph-level session statistics

## Related Guides

- `architecture/session-graph.md`
- `memory-system.md`
