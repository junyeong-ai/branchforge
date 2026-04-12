# axis · architecture

## Defect patterns to hunt

- **SSoT mirror** — any `cached_messages`, `message_snapshot`, `branch_messages_cache` field on `Session` or downstream types
- **Cyclic imports** — `session ↔ graph ↔ tools` or similar cycles
- **Feature-gate leak** — `src/**/*.rs` importing `rustix`/`tree_sitter`/cloud-specific code without the corresponding `#[cfg]`
- **Lock order violation** — `.await` while holding `RwLock::write` / `Mutex::lock` guard
- **RAII shutdown escape** — `tokio::spawn` inside `AgentRuntime` that captures `Arc<AgentRuntime>` (prevents Drop from firing)
- **Graph direct mutation** — `session.graph.apply_event(_)` outside `Session::apply_graph_event(_)`
- **Dual public+internal export** — `pub use` of types that are also intended as `pub(crate)`

## Rules to load additionally

- `.claude/rules/graph-session.md`
- `.claude/rules/events.md`
- `.claude/rules/naming.md`

## Stopping criterion

All bullets above visited against current `src/` tree. No new patterns added inline — propose via skill PR.
