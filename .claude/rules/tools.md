---
paths:
  - "src/tools/**"
---

# Tools Module Rules

- `Tool` trait: `execute(&self, input: Value, context: &ExecutionContext) -> ToolResult`. Do not change this signature.
- `SchemaTool` trait auto-derives `Tool` via blanket impl. Prefer `SchemaTool` for new tools.
- `ExecutionContext` carries security, hooks, session, progress channel, and optional `cancel_token`. Human-in-the-loop is handled at the agent-runtime layer via `AgentRuntime.human: Option<Arc<dyn HumanInteractionHandler>>` (Phase D C-1), not through the execution context.
- Tool cancellation is opt-in: check `context.cancel_token()` in long-running tools. BashTool uses `tokio::select!` to race child process wait vs cancellation.
- `ToolRegistry` enforces timeout via `tokio::time::timeout` and cancellation via `tokio::select!`.
- Tool names are PascalCase (`Read`, `Write`, `Bash`). Skill names are kebab-case (`commit`, `review-pr`).
- `MockLlmCall` (re-exported at `crate::MockLlmCall`) enables deterministic agent testing with scripted `then_text` / `then_tool_call` / `then_stream_text` / `then_stream_tool_call` helpers.

## Tool self-description methods

Every `Tool` is responsible for declaring its own capabilities. The
runtime never hard-codes per-tool-name rules — it asks the tool.
All metadata methods have fail-closed defaults so an unknown tool
is assumed mutating, serial, and permission-gated.

### Static metadata (on `SchemaTool` as associated consts)

- `ALIASES: &[&str]` — alternative lookup names for backward-compat renames.
- `SEARCH_HINT: Option<&str>` — 3–10 word phrase consumed by `ToolSearchManager`.
- `MAX_RESULT_SIZE_BYTES: usize` — inline result cap in **bytes** (UTF-8-encoded length, matching `String::len`). Outputs exceeding this are spilled to the registry's configured `OverflowStore`; the result carries an `OverflowRef` pointing at the spilled blob. Phase C-7.
- `INTERRUPT_BEHAVIOR: InterruptBehavior` — `Cancel` (default) or `Block` when user interrupts mid-execution.
- `READ_ONLY: bool` — coarse fallback used only when `is_read_only_typed` is not overridden.

### Input-aware capability queries (typed methods on `SchemaTool`)

- `is_read_only_typed(&Input)` — safe to run in parallel with other readers.
- `is_concurrency_safe_typed(&Input)` — no shared-resource contention.
- `is_destructive_typed(&Input)` — irreversible action (triggers HITL escalation).
- `is_open_world_typed(&Input)` — reaches network / external services.
- `requires_user_interaction_typed(&Input)` — blocks concurrent execution, requires TTY.
- `permission_subjects_typed(&Input) -> Vec<String>` — subjects the permission DSL matches rules against.

### Preflight validation

- `validate_input_typed(&Input, &ExecutionContext) -> ValidationResult` — **side-effect-free** preflight check. Schedulers call it in parallel across pending tool calls, reject the invalid ones, then serially execute the valid ones. Returns `Err(ValidationError { message, code })` on failure.

### `Tool` trait equivalents

The erased `Tool` trait has the same methods taking `&Value` instead of the typed input. The blanket `impl<T: SchemaTool> Tool for T` deserializes once per call and forwards to the typed version; deserialization failures fall back to the static defaults.

### Example: Bash input-aware classification

`BashTool` uses `crate::security::bash::BashAnalyzer::classify_*` helpers to make `is_read_only / is_concurrency_safe / is_destructive / is_open_world` truly input-aware. `Bash("ls")` reports read-only + concurrency-safe; `Bash("rm -rf /")` reports destructive. The same tool cannot be marked static-read-only — that would lose this dynamic classification.
