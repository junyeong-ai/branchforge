# branchforge

A Rust-native runtime for stateful agents — pure API agents, local-machine knowledge agents, and full coding agents — built on a four-layer architecture so you only pay for what you use.

[![CI](https://github.com/junyeong-ai/branchforge/actions/workflows/ci.yml/badge.svg)](https://github.com/junyeong-ai/branchforge/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-1.94%2B-orange.svg)](https://www.rust-lang.org)
[![Edition](https://img.shields.io/badge/edition-2024-blue.svg)](https://doc.rust-lang.org/edition-guide/)
[![License](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

English | [한국어](README.ko.md)

## Overview

`branchforge` is more than a thin API binding. It is a Rust-based agent runtime for long-lived engineering workflows.

The project is designed around:

- a graph-first session model where `SessionGraph` is the single source of truth
- durable work history with replay, export, bookmarks, and checkpoints
- native structured outputs across Anthropic, Bedrock, Vertex AI, Azure AI Foundry, OpenAI, and Gemini via a shared `SchemaPolicy` pipeline
- safe local tool execution with explicit authorization control
- workspace resources compatible with the Claude CLI `.claude/` layout

## Layered architecture

`branchforge` is partitioned into four layers, each behind its own Cargo feature so you can opt into exactly the surface area your deployment needs:

| Layer | Feature | What it adds | Typical use |
|-------|---------|--------------|-------------|
| **1 — Pure core** | (always on) | Agent runtime, IR, provider client stack, session graph, hooks, budget, observability, network egress sandbox. Zero filesystem or shell dependencies. | Server-side API agents, customer support bots, workflow orchestrators. |
| **2a — Local FS** | `local-fs` | `Workspace`, `SecureFs` (TOCTOU-safe), Read/Write/Edit/Glob/Grep, Landlock/Seatbelt path sandbox, generic markdown memory loader, `explore`/`plan` subagents. | Research agents, knowledge workers, data analysts on local files. |
| **2b — Coding tools** | `coding-tools` | Bash with tree-sitter AST validation, process scheduler, container detection, CLAUDE.md discovery, git context, `bash` subagent. Depends on Layer 2a. | Claude Code-class coding agents. |
| **3 — Cloud providers** | `aws` / `gcp` / `azure` / `cloud-all` | Bedrock, Vertex (Gemini + Anthropic), Azure AI Foundry transport adapters. Depends on Layer 1. | Multi-cloud or enterprise deployments. |

Default features: `anthropic-direct` (pure Layer 1 — zero filesystem/shell deps). For coding agents add `features = ["coding-tools"]` (transitively enables `local-fs`). See [`docs/architecture/layering.md`](docs/architecture/layering.md) for the full dependency contract.

## Documentation

| Guide | Description |
|-------|-------------|
| [Architecture](docs/architecture.md) | System boundaries and design principles |
| [Layering](docs/architecture/layering.md) | Four-layer feature gating contract |
| [Session & Graph](docs/session.md) | Graph-first session model and persistence |
| [Tools](docs/tools.md) | Built-in tools, access control, and custom tools |
| [Skills](docs/skills.md) | Progressive disclosure and skill system |
| [Subagents](docs/subagents.md) | Delegation, tool restrictions, and model resolution |
| [Authorization](docs/authorization.md) | Execution modes, tool policy, and HITL |
| [Security](docs/security.md) | SecureFs, bash analysis, and sandboxing |
| [Authentication](docs/authentication.md) | OAuth, API keys, and cloud providers |
| [Backend Selection](docs/backend-selection.md) | Memory, JSONL, PostgreSQL, Redis |

## Core Value

- `SessionGraph` is the canonical session state. Message lists are derived on demand via `Session::current_branch_messages()` — there is no cached `messages` field.
- Sessions are managed as work graphs that support branching, replay, and export.
- JSONL, PostgreSQL, and Redis persistence backends are available.
- Built-in tools, MCP, subagents, and skills can be composed in one runtime.
- Structured outputs use a provider-neutral `JsonSchemaSpec` in the IR; each codec runs the schema through a per-provider `SchemaPolicy` at encode time and surfaces any dropped keywords as `ModelWarning::LossyEncode`.

## Quick Start

### Installation

```toml
[dependencies]
branchforge = "0.9"
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
- AWS Bedrock (Converse API)
- Google Vertex AI
- Azure AI Foundry
- OpenAI (GPT-4o, o3, compatible endpoints)
- Google Gemini

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

See [Session & Graph](docs/session.md) for details.

## Runtime Architecture

The agent separates shared infrastructure from per-session state:

```rust
use branchforge::{Agent, AgentRuntime, RunConfig};
use std::sync::Arc;

// AgentRuntime holds client, config, tools, hooks — shared across sessions.
let agent = Agent::builder()
    .auth(Auth::from_env()).await?
    .tools(ToolSurface::core())
    .build()
    .await?;

// Per-execution overrides via RunConfig — no need to rebuild the agent.
let result = agent
    .execute_with(
        "Summarize this file",
        RunConfig::new()
            .model("claude-haiku-4-5-20251001")
            .max_iterations(3)
            .system_prompt("Be concise."),
    )
    .await?;

// Graceful shutdown via CancellationToken.
agent.shutdown_token().cancel();
```

Key capabilities:

- **AgentRuntime**: shared infrastructure (`client`, `config`, `tools`, `hooks`, `budget`) wrapped in `Arc` for multi-session use
- **RunConfig**: per-execution overrides for `model`, `max_tokens`, `max_iterations`, `timeout`, `system_prompt`, `execution_mode`
- **Graceful shutdown**: cooperative cancellation via `CancellationToken` with session state persistence
- **EventBus subscriptions**: `SubscriptionHandle` with RAII auto-unsubscribe on drop

## Structured Outputs

All five codecs (`anthropic-messages`, `openai-chat`, `openai-responses`, `gemini-generate`, `bedrock-converse`) ship native structured output support through a single shared `SchemaPolicy` pipeline. Derive a schema from a Rust type and parse the response back with zero glue code:

```rust
use branchforge::ir::{JsonSchemaSpec, Message, ModelRequest, ResponseFormat};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(JsonSchema, Deserialize, Debug)]
struct Contact {
    name: String,
    email: String,
    plan_interest: String,
}

let request = ModelRequest::new("claude-opus-4-6", vec![Message::user("...")])
    .with_response_format(ResponseFormat::JsonSchema(
        JsonSchemaSpec::from_type::<Contact>()
    ));

let response = client.send(&request).await?;
let contact: Contact = response.json()?;
```

The shared `prepare_schema` pipeline adds `additionalProperties: false`, strips provider-unsupported keywords with `LossyEncode` warnings, rejects recursive `$ref` cycles, and filters non-allowlisted formats — so `schemars`-generated schemas work out of the box across every provider. See `examples/structured_output.rs` for an end-to-end demo.

## Tooling

The default runtime exposes a minimal core tool surface. Optional workflow tools can be enabled when needed.

- File: Read, Write, Edit, Glob, Grep
- Execution: Bash, KillShell
- Extension: Skill
- Optional workflow: Task, TaskOutput, TodoWrite, Plan, GraphHistory
- Server tools: WebFetch, WebSearch, ToolSearch

See [Tools](docs/tools.md) for details.

## Quality Gates

```bash
cargo build --all-features                       # compilation
cargo test --all-features                        # unit + integration
cargo clippy --all-features -- -D warnings       # lint
cargo fmt --all -- --check                       # format
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps  # docs
cargo build --lib --no-default-features          # pure-core gate
cargo test --lib audit_ --all-features           # FSM + enum evolution audit
```
