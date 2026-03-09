# CLAUDE.md

## Purpose

This repository implements a Rust-native agent runtime built around a graph-first session model.

## What Matters

- `SessionGraph` is the canonical session state.
- `Session.messages` is a derived projection used for message-based APIs.
- Replay, export, bookmarks, checkpoints, and session branching must preserve graph history.
- Persistence backends store graph state and rebuild projections from it.
- Multi-provider support exists through provider adapters for Anthropic, Bedrock, Vertex AI, and Azure AI Foundry.

## Commands

```bash
cargo build --release
cargo nextest run --all-features
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
```

## Coding Guidance

- Keep `SessionGraph` as the source of truth.
- Treat direct `messages` mutation as projection maintenance, not domain state updates.
- Prefer extending graph-first APIs over adding new message-first shortcuts.
- Preserve explicit boundaries between `agent`, `session`, `graph`, `client`, `auth`, and `tools`.
- Keep provider-specific behavior inside adapter and provider layers.
- Keep authentication separate from prompt composition and assistant behavior.
- Prefer small, composable services over compatibility wrappers.
- Remove replaced legacy paths instead of keeping parallel abstractions.

## Key Areas

- `src/graph/`: session graph, replay, export, materialization
- `src/session/`: session facade, persistence backends, compaction, queueing
- `src/agent/`: runtime loop, task orchestration, builder flow
- `src/client/`: provider adapters, request lowering, streaming
- `src/auth/`: credential resolution and refresh
- `src/tools/`: built-in tool registry and execution wiring
