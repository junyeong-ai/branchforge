# SurrealDB Evaluation

This document evaluates SurrealDB 3.x as a graph-native persistence candidate for `claude-agent-rs`.

## Why It Is Relevant

The runtime already uses a graph-first session model.

The strongest reasons to evaluate SurrealDB are:

- session state is already graph-shaped
- replay, bookmarks, checkpoints, and branching all depend on graph traversal
- future graph search and graph analytics can become first-class product features
- SurrealDB supports graph, document, and vector-oriented models in one system

## Where SurrealDB Could Be Better Than PostgreSQL

- graph traversal queries may be simpler to express
- branch and node relationship queries may require less relational glue
- graph-native exploration and search features could map more directly to storage
- future graph plus vector features could share one backend

## Where PostgreSQL Still Wins Today

- stronger operational familiarity
- mature production tooling and backup workflows
- existing backend is already implemented and validated at the library level

## Adoption Criteria

SurrealDB should only move beyond prototype status if it shows clear value in at least one of these areas:

- simpler implementation of branch, bookmark, checkpoint, and replay queries
- cleaner graph exploration APIs with less projection glue
- better support for future graph search and graph analytics
- meaningful reduction in storage-model mismatch versus PostgreSQL

## Current Recommendation

- keep PostgreSQL as the primary production-oriented relational backend
- treat SurrealDB as an experimental graph-native backend candidate
- validate graph exploration and graph search value first, then compare backend leverage

## Prototype Scope

The current repository includes a minimal `SurrealPersistence` prototype that exports graph-shaped records from a session and defines the backend surface without claiming production readiness.

It is intentionally not wired to a live SurrealDB client yet.

## Next Evaluation Steps

1. implement graph exploration tools on top of the current graph model
2. measure which queries are awkward in PostgreSQL
3. compare those queries against a real SurrealDB implementation
4. decide whether SurrealDB remains experimental, becomes optional, or should be dropped
