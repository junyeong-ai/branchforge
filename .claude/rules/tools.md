---
paths:
  - "src/tools/**"
---

# Tools Module Rules

- `Tool` trait: `execute(&self, input: Value, context: &ExecutionContext) -> ToolResult`. Do not change this signature.
- `SchemaTool` trait auto-derives `Tool` via blanket impl. Prefer `SchemaTool` for new tools.
- `ExecutionContext` carries security, hooks, session, progress channel, and optional `cancel_token`.
- Tool cancellation is opt-in: check `context.cancel_token()` in long-running tools. BashTool uses `tokio::select!` to race child process wait vs cancellation.
- `ToolRegistry` enforces timeout via `tokio::time::timeout` and cancellation via `tokio::select!`.
- Tool names are PascalCase (`Read`, `Write`, `Bash`). Skill names are kebab-case (`commit`, `review-pr`).
