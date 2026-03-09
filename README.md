# claude-agent-rs

A Rust-native runtime for stateful coding agents.

[![Crates.io](https://img.shields.io/crates/v/claude-agent.svg)](https://crates.io/crates/claude-agent)
[![Docs.rs](https://img.shields.io/docsrs/claude-agent)](https://docs.rs/claude-agent)
[![Rust](https://img.shields.io/badge/rust-1.92%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/crates/l/claude-agent.svg)](LICENSE)

English | [한국어](README.ko.md)

## Overview

`claude-agent-rs` is more than a thin API binding. It is a Rust-based agent runtime for long-lived engineering workflows.

The project is designed around:

- a graph-first session model
- durable work history with replay, export, bookmarks, and checkpoints
- support for Anthropic, Bedrock, Vertex AI, and Azure AI Foundry
- safe local tool execution with explicit permission control
- Claude Code-style project resources such as `CLAUDE.md`, skills, and rules

## Core Value

- `SessionGraph` is the canonical session state.
- `Session.messages` is kept as a projection for message-based APIs.
- Sessions are managed as work graphs that support branching, replay, and export.
- JSONL, PostgreSQL, and Redis persistence backends are available.
- Built-in tools, MCP, subagents, and skills can be composed in one runtime.

## Quick Start

### Installation

```toml
[dependencies]
claude-agent = "0.2"
tokio = { version = "1", features = ["full"] }
```

### Simple Query

```rust
use claude_agent::query;

#[tokio::main]
async fn main() -> claude_agent::Result<()> {
    let response = query("Explain the benefits of Rust").await?;
    println!("{response}");
    Ok(())
}
```

### Build an Agent

```rust
use claude_agent::{Agent, ToolAccess};

#[tokio::main]
async fn main() -> claude_agent::Result<()> {
    let agent = Agent::builder()
        .from_claude_code(".").await?
        .tools(ToolAccess::all())
        .build()
        .await?;

    let result = agent.execute("Summarize this repository").await?;
    println!("{}", result.output);
    Ok(())
}
```

## Authentication

Supported authentication modes:

- Anthropic API key
- Claude Code CLI credentials
- AWS Bedrock
- Google Vertex AI
- Azure AI Foundry

Example:

```rust
use claude_agent::Auth;

let agent = claude_agent::Agent::builder()
    .auth(Auth::api_key("sk-ant-..."))
    .await?
    .build()
    .await?;
```

See `docs/authentication.md` and `docs/cloud-providers.md` for details.

## Sessions and Replay

Sessions use a graph-first model.

- branching
- replay
- export
- bookmarks
- checkpoints

This makes long coding sessions reusable and navigable instead of reducing them to flat logs.

See `docs/session.md` for details.

## Tooling

Built-in tools cover file operations, shell execution, planning, and subagent orchestration.

- File: Read, Write, Edit, Glob, Grep
- Execution: Bash, KillShell
- Agent: Task, TaskOutput, TodoWrite, Skill
- Planning: Plan
- Server tools: WebFetch, WebSearch, ToolSearch

See `docs/tools.md` for details.

## Documentation

- `docs/architecture.md`
- `docs/authentication.md`
- `docs/cloud-providers.md`
- `docs/session.md`
- `docs/tools.md`
- `docs/security.md`
- `docs/permissions.md`
- `docs/subagents.md`
- `docs/skills.md`
- `docs/memory-system.md`

## Quality Gates

This repository is maintained against the following quality gates.

```bash
cargo nextest run --all-features
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
```
