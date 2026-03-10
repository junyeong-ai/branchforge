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
- treat SurrealDB as a live optional graph-native backend and a serious production candidate
- require stronger operational validation before considering it as the default primary backend

## Current Scope

The repository now includes a live `SurrealPersistence` backend that round-trips session snapshots, graph records, summaries, and queue state through the SurrealDB SQL API.

Docker-backed tests currently validate:

- graph and identity round-trips
- concurrent session writes
- compaction round-trips
- scoped graph search
- queue stress behavior
- large graph baseline behavior
- basic failure handling for unreachable endpoints

The remaining gap is operational maturity rather than basic correctness.

## Next Evaluation Steps

1. implement migration/version tracking and an operational runbook
2. define backup and restore strategy
3. run longer soak and performance validation
4. decide whether SurrealDB should remain optional or be promoted toward a primary backend role
