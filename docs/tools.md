# Tools

The runtime includes built-in tools for file operations, execution, planning, and agent orchestration.

## Built-in Tool Groups

- File: `Read`, `Write`, `Edit`, `Glob`, `Grep`
- Execution: `Bash`, `KillShell`
- Agent: `Task`, `TaskOutput`, `TodoWrite`, `Skill`
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

## Related Guides

- `permissions.md`
- `security.md`
- `subagents.md`
