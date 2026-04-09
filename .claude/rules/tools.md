---
paths:
  - "src/tools/**"
---

# Tools Module Rules

- `Tool` trait: `execute(&self, input: Value, context: &ExecutionContext) -> ToolResult`. Do not change this signature.
- `SchemaTool` trait auto-derives `Tool` via blanket impl. Prefer `SchemaTool` for new tools.
- `ExecutionContext` carries security, hooks, session, progress channel, optional `cancel_token`, and optional `approval_sender`.
- Tool cancellation is opt-in: check `context.cancel_token()` in long-running tools. BashTool uses `tokio::select!` to race child process wait vs cancellation.
- `ToolRegistry` enforces timeout via `tokio::time::timeout` and cancellation via `tokio::select!`.
- Tool names are PascalCase (`Read`, `Write`, `Bash`). Skill names are kebab-case (`commit`, `review-pr`).
- Tool metadata methods (all default-impl, zero breakage on add): `interrupt_behavior`, `is_concurrency_safe`, `is_open_world`, `is_destructive`, `validate_input`, `aliases`, `search_hint`, `max_result_size_chars`.
- `MockLlmCall` in `src/client/mock.rs` enables deterministic agent testing with scripted response sequences.
