# Session Graph

The canonical conversation model is a session graph.

## Goals

- Represent branching work inside one logical session
- Preserve checkpoints, summaries, plans, todos, and tool provenance
- Support replay, export, filtering, and branch comparison
- Keep persistence append-only and crash-safe

## Model

- `SessionGraph` owns immutable events and derived node views
- `GraphNode` is the queryable projection used by runtime and export layers
- `BranchId` identifies a named line of work
- `Checkpoint` marks a stable point in the graph with labels and tags
- `Bookmark` marks a reusable navigation point tied to a concrete node

## Rules

1. New work appends events.
2. Compaction creates summary nodes; it does not rewrite history.
3. Branching forks from an existing node.
4. Query APIs are typed and filterable by node kind, branch, tag, and time.
5. Persistence stores the event log as the source of truth and rebuilds projections when needed.

## Projection Policy

- `SessionGraph` is canonical state.
- `Session.messages` is a derived projection used for compatibility with message-based APIs.
- Persistence backends may store message projections for efficiency, but graph events and graph metadata remain authoritative.
- Checkpoints and bookmarks are first-class graph metadata and must round-trip independently of message projections.
- Compaction may intentionally shrink the message projection for downstream APIs, but it must preserve graph history by appending summary/checkpoint metadata instead of deleting graph state.
