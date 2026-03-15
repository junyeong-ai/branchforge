# branchforge

A Rust-native runtime for stateful coding agents.

[![CI](https://github.com/junyeong-ai/branchforge/actions/workflows/ci.yml/badge.svg)](https://github.com/junyeong-ai/branchforge/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-1.94%2B-orange.svg)](https://www.rust-lang.org)
[![Edition](https://img.shields.io/badge/edition-2024-blue.svg)](https://doc.rust-lang.org/edition-guide/)
[![License](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

English | [한국어](README.ko.md)

## Overview

`branchforge` is more than a thin API binding. It is a Rust-based agent runtime for long-lived engineering workflows.

The project is designed around:

- a graph-first session model
- durable work history with replay, export, bookmarks, and checkpoints
- support for Anthropic, Bedrock, Vertex AI, and Azure AI Foundry
- safe local tool execution with explicit authorization control
- workspace resources compatible with the Claude CLI `.claude/` layout

## Documentation

| Guide | Description |
|-------|-------------|
| [Architecture](docs/architecture.md) | System boundaries and design principles |
| [Session & Graph](docs/session.md) | Graph-first session model and persistence |
| [Tools](docs/tools.md) | Built-in tools, access control, and custom tools |
| [Skills](docs/skills.md) | Progressive disclosure and skill system |
| [Subagents](docs/subagents.md) | Delegation, tool restrictions, and model resolution |
| [Authorization](docs/authorization.md) | Modes, rules, and scoped patterns |
| [Security](docs/security.md) | SecureFs, bash analysis, and sandboxing |
| [Authentication](docs/authentication.md) | OAuth, API keys, and cloud providers |
| [Backend Selection](docs/backend-selection.md) | Memory, JSONL, PostgreSQL, Redis |

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
branchforge = "0.2"
tokio = { version = "1", features = ["full"] }
```

### Simple Query

```rust
use branchforge::query;

#[tokio::main]
async fn main() -> branchforge::Result<()> {
    let response = query("Explain the benefits of Rust").await?;
    println!("{response}");
    Ok(())
}
```

### Build an Agent

```rust
use branchforge::{Agent, Auth, ToolSurface};

#[tokio::main]
async fn main() -> branchforge::Result<()> {
    let agent = Agent::builder()
        .auth(Auth::from_env()).await?
        .tools(ToolSurface::core())
        .build()
        .await?;

    let result = agent.execute("Summarize this repository").await?;
    println!("{}", result.text());
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
use branchforge::Auth;

let agent = branchforge::Agent::builder()
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

The default runtime exposes a minimal core tool surface. Optional workflow tools can be enabled when needed.

- File: Read, Write, Edit, Glob, Grep
- Execution: Bash, KillShell
- Extension: Skill
- Optional workflow: Task, TaskOutput, TodoWrite, Plan, GraphHistory
- Server tools: WebFetch, WebSearch, ToolSearch

See `docs/tools.md` for details.

## Documentation

- `docs/architecture.md`
- `docs/authentication.md`
- `docs/cloud-providers.md`
- `docs/session.md`
- `docs/tools.md`
- `docs/security.md`
- `docs/authorization.md`
- `docs/subagents.md`
- `docs/skills.md`
- `docs/memory-system.md`
- `docs/backend-selection.md`
- `docs/audit-export.md`

## Quality Gates

This repository is maintained against the following quality gates.

```bash
cargo nextest run --all-features
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
```
