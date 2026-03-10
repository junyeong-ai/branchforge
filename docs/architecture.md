# Architecture

`claude-agent-rs` is a Rust-native agent runtime built around a graph-first session model.

## System Boundaries

- `agent`: execution loop, streaming, task orchestration, builder flow
- `client`: provider adapters, requests, streaming, fallback behavior
- `graph`: canonical session graph, replay, export, materialization
- `session`: session facade, persistence, compaction, queueing
- `auth`: credential resolution and refresh
- `tools`: built-in tool registry and execution wiring
- `context`: CLAUDE.md, rules, memory loading, orchestration
- `security`: secure file access, sandboxing, bash analysis

## Core Design

- `SessionGraph` is canonical state.
- `Session.messages` is a derived projection for message-based APIs.
- Replay, export, bookmarks, and checkpoints operate on graph state.
- Persistence backends rebuild projections from graph state.
- Provider-specific behavior stays inside client adapter layers.
- Tenant scope and principal ownership are modeled separately so storage, budgeting, and request metadata can stay consistent in multi-tenant deployments.

Graph records may also carry creator principal metadata so bookmarks, checkpoints, and nodes can support future audit and ownership-aware workflows.

Graph nodes may also include execution provenance such as source session and subagent type so replay and graph exploration can trace where delegated work originated.

## Prompt Cache Segments

The runtime treats prompt caching as three segments:

- static context: system prompt, `CLAUDE.md`, skill summaries, rule summaries
- tool metadata: built-in tool summaries, MCP tool metadata, server-tool summaries
- conversation history: active message projection with a cache breakpoint on the latest user turn

This keeps graph history and prompt caching aligned without making tool execution semantics part of static context.

## Request Path

1. `Agent` builds runtime state and prompt inputs.
2. `ToolRegistry` exposes local and optional server tools.
3. `Client` lowers normalized requests through a provider adapter.
4. Responses, tool activity, and session updates are recorded back into session state.

## Session Path

1. New work appends graph state.
2. Message projections are refreshed from graph state when needed.
3. Replay and export use graph traversal, not flat message history.
4. Compaction preserves graph history and only shrinks the compatibility projection.

## Related Guides

- `architecture/session-graph.md`
- `architecture/runtime-boundaries.md`
- `architecture/provider-capabilities.md`
- `architecture/surrealdb-evaluation.md`
- `session.md`
