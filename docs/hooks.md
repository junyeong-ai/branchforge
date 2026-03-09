# Hooks

Hooks let the runtime intercept execution at selected lifecycle points.

## Typical Uses

- inspect or block tool execution
- inject additional context
- run post-tool automation
- observe session lifecycle events

## Event Coverage

Hooks are available for session lifecycle, tool lifecycle, subagent lifecycle, and compaction-related flows.

## Design Rule

- blocking hooks are explicit
- non-blocking hooks should not mutate core graph state directly
- hook behavior should stay orthogonal to provider transport and credential resolution
