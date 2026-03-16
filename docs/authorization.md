# Authorization

Tool execution is controlled by three orthogonal axes:

- **`ToolSurface`** — build-time: which tools are registered in the registry
- **`ToolPolicy`** — runtime rules: which tools are allowed to execute (allow/deny patterns)
- **`ExecutionMode`** — runtime mode: how tools execute (automatic, supervised, exploration-only)

## Execution Modes

| Mode | Behavior |
|------|----------|
| `Auto` | Tools execute automatically when policy allows (default) |
| `Plan` | Exploration only — read/navigation tools (`Read`, `Glob`, `Grep`, `Plan`, `TodoWrite`, `GraphHistory`) |
| `Supervised` | All tools require user review before execution (human-in-the-loop) |
| `SupervisedFor(set)` | Only specified tools require review; others execute automatically |

## Tool Policy

Policies combine allow and deny rules. Deny rules always take precedence.

```rust
use branchforge::ToolPolicy;

let policy = ToolPolicy::builder()
    .allow("Read")
    .allow("Bash(git:*)")
    .deny("Write(*.env)")
    .build();
```

Scoped rules work for skills:

```rust
let policy = ToolPolicy::builder()
    .allow("Skill")
    .deny("Skill(internal)")
    .build();
```

## Execution Mode Examples

```rust
use branchforge::{Agent, Auth, ExecutionMode};

// Automatic (default) — CI automation
let agent = Agent::builder()
    .auth(Auth::from_env()).await?
    .execution_mode(ExecutionMode::Auto)
    .build().await?;

// Supervised — all tools need user approval
let agent = Agent::builder()
    .auth(Auth::from_env()).await?
    .execution_mode(ExecutionMode::Supervised)
    .build().await?;

// Supervised for specific tools only
let agent = Agent::builder()
    .auth(Auth::from_env()).await?
    .execution_mode(ExecutionMode::supervised_for(["Bash", "Write"]))
    .build().await?;
```

When a tool requires review, the agent emits `AgentEvent::ToolReview` with the tool name and input.

## Decision Flow

1. **ToolPolicy** evaluates allow/deny rules → `Allow` or `Deny`
2. **ExecutionMode** checks mode constraints → `Plan` blocks write tools, `Supervised` requires review
3. **PreToolUse hook** provides additional policy layer → can block or modify input

## Related Guides

- [Security](security.md)
- [Tools](tools.md)
- [Hooks](hooks.md)
