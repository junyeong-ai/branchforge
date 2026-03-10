# Tools

The runtime includes built-in tools for file operations, execution, planning, and agent orchestration.

## Built-in Tool Groups

- File: `Read`, `Write`, `Edit`, `Glob`, `Grep`
- Execution: `Bash`, `KillShell`
- Agent: `Task`, `TaskOutput`, `TodoWrite`, `Skill`, `GraphHistory`
- Planning: `Plan`

## Optional Server Tools

When supported by the provider path, the runtime can expose:

- `WebFetch`
- `WebSearch`
- `ToolSearch`

## Access Control

Use `ToolAccess` to allow all, allow a subset, or exclude specific tools.

```rust
use claude_agent::ToolAccess;

ToolAccess::all();
ToolAccess::only(["Read", "Grep", "Glob"]);
ToolAccess::except(["Bash", "Write"]);
```

## Security Notes

- file access integrates with the secure file layer
- shell execution is analyzed before execution
- permission rules can restrict tools and scoped inputs

## Skill Tool

The `Skill` tool is a progressive-disclosure tool.

- it exposes lightweight skill metadata in tool descriptions
- it loads full skill content only on invocation
- inline skills return rendered instructions into the current conversation
- forked skills delegate execution through a separate agent context

## Graph History Tool

The `GraphHistory` tool exposes graph-first session exploration.

- branch summaries
- tree views
- bookmarks and checkpoints
- node summaries
- graph search
- graph-level session statistics

## Related Guides

- `permissions.md`
- `security.md`
- `subagents.md`
