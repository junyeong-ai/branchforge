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
    .execution_mode(ExecutionMode::SupervisedFor(
        ["Bash", "Write"].into_iter().map(String::from).collect()
    ))
    .build().await?;
```

When a tool requires review, the agent emits `AgentEvent::ToolReview` with the tool name and input.

## Subject extraction

Scoped patterns like `Bash(rm:*)` or `WebFetch(domain:github.com)` match
against **subjects** — strings the tool itself extracts from its own
input. Subject extraction lives on the `Tool` trait via
`Tool::permission_subjects(&input)`, which returns a `Vec<String>`.

```rust
impl SchemaTool for MyTool {
    fn permission_subjects_typed(&self, input: &MyInput) -> Vec<String> {
        vec![input.target_path.clone()]
    }
}
```

Built-in subjects:
- `Bash` → first command token (`rm`, `git`, …)
- `Read`/`Write`/`Edit` → `file_path`
- `Glob`/`Grep` → `path`
- `Skill` → `skill`
- `WebFetch` → `url` (matched against domain patterns via URL parsing)

The permission engine is the rule evaluator; the tool is the
extractor. There is **no parallel extractor registry** — this
design was collapsed in Phase D Workstream A-1 to follow the
"no dual systems" rule in `.claude/rules/naming.md`.

## Decision Flow

1. **ToolPolicy** evaluates allow/deny rules → `Allow` or `Deny`
2. **ExecutionMode** checks mode constraints → `Plan` blocks write tools, `Supervised` requires review
3. **PreToolUse hook** provides additional policy layer → can block or modify input

## Related Guides

- [Security](security.md)
- [Tools](tools.md)
- [Hooks](hooks.md)
