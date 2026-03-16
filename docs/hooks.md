# Hooks

Hooks intercept execution at lifecycle points for security enforcement, observation, and customization.

## Hook Events

| Event | Blocking | Purpose |
|-------|----------|---------|
| `PreToolUse` | Yes | Inspect, block, or modify tool invocations |
| `PostToolUse` | No | Observe tool results |
| `PostToolUseFailure` | No | Observe tool errors |
| `UserPromptSubmit` | Yes | Validate or transform user input |
| `Stop` | No | Observe agent stop |
| `SubagentStart` | Yes | Control subagent spawning |
| `SubagentStop` | No | Observe subagent completion |
| `PreCompact` | No | Observe compaction triggers |
| `SessionStart` | Yes | Control session initialization |
| `SessionEnd` | No | Observe session completion |
| `PostStreamChunk` | No | Observe streaming text chunks (fail-open) |
| `ModelSelection` | Yes | Override model selection before API call |
| `PreMessage` | Yes | Block or observe outgoing messages |
| `PostMessage` | No | Observe token usage after API response |

## Design Rules

- Blocking hooks (`can_block() = true`) use fail-closed semantics — errors block the operation
- Non-blocking hooks use fail-open semantics — errors are logged but execution continues
- `PostStreamChunk` is dispatched via `tokio::spawn` to never block the stream
- Hook behavior should stay orthogonal to provider transport and credential resolution

## Example

```rust
use branchforge::hooks::{Hook, HookEvent, HookInput, HookOutput, HookContext};

struct AuditHook;

#[async_trait::async_trait]
impl Hook for AuditHook {
    fn name(&self) -> &str { "audit" }
    fn events(&self) -> &[HookEvent] { &[HookEvent::PostToolUse, HookEvent::PostMessage] }
    async fn execute(&self, input: HookInput, _ctx: &HookContext)
        -> Result<HookOutput, branchforge::Error> {
        // Log tool usage or token consumption
        Ok(HookOutput::allow())
    }
}
```

## Related Guides

- [Authorization](authorization.md)
- [Observability](observability.md)
