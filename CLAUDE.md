# CLAUDE.md

## Purpose

Rust-native agent runtime built around a graph-first session model.
`SessionGraph` is the canonical session state; `Session.messages` is a derived projection.

## Commands

```bash
cargo build --release
cargo test --all-features                       # local
cargo nextest run --all-features                # CI (requires cargo-nextest)
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
```

## Feature Flags

```bash
cargo build                                     # default: coding-tools enabled
cargo build --no-default-features               # pure SDK core (no file/bash tools)
cargo build --features "coding-tools"           # file I/O, bash, bash AST analysis
cargo build --features "scheduling"             # cron scheduler, remote triggers
cargo build --features "cli-auth"               # Claude Code CLI OAuth credentials
cargo build --features "mcp"                    # MCP server integration
cargo build --features "cloud-all"              # aws, gcp, azure, openai, gemini
cargo build --features "persistence-all"        # jsonl, postgres, redis
cargo build --features "full"                   # all of the above
cargo build --all-features                      # full + multimedia
```

## Coding Guidance

- Keep `SessionGraph` as the source of truth.
- Treat direct `messages` mutation as projection maintenance, not domain state updates.
- Prefer extending graph-first APIs over adding new message-first shortcuts.
- Preserve explicit boundaries between modules (see Key Areas below).
- Keep provider-specific behavior inside adapter and provider layers.
- Keep authentication separate from prompt composition and assistant behavior.
- Prefer small, composable services over compatibility wrappers.
- Remove replaced legacy paths instead of keeping parallel abstractions.

## Key Areas

- `src/graph/`: session graph, replay, export, materialization
- `src/session/`: session facade, persistence backends, compaction, queueing
- `src/agent/`: runtime loop, task orchestration, builder flow
- `src/client/`: provider adapters, request lowering, streaming
- `src/auth/`: credential resolution, OAuth token refresh, CLI credential storage
- `src/tools/`: built-in tool registry and execution wiring
- `src/authorization/`: execution modes, tool policy rules, tool limits
- `src/events/`: non-blocking event bus for observability
- `src/mcp/`: MCP server transport and tool discovery
- `src/orchestration/`: multi-agent coordination, agent directory, inter-agent messaging
- `src/security/`: SecureFs, bash command analysis, sandboxing
- `src/skills/`: skill registry, progressive disclosure, on-demand loading
- `src/subagents/`: delegation, tool restrictions, model resolution
- `src/tokens/`: token counting, budget, cache break detection
- `src/scheduling/`: cron scheduler, remote triggers
