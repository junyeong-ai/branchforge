# Subagents

Subagents are independent agent executions launched through the task system.

## Built-in Types

- `bash`
- `explore`
- `plan`
- `general`

## What Subagents Are For

- isolate exploration work
- delegate planning
- run long or parallel tasks
- keep the main agent context focused

## How They Are Started

Subagents are typically launched through the `Task` tool.

```json
{
  "description": "Review the auth module",
  "prompt": "Inspect authentication code for risks and summarize findings",
  "subagent_type": "explore"
}
```

## Background Execution

Subagents can run in the background and later be polled through `TaskOutput`.

Built-in subagent names are case-sensitive and lowercase:

- `bash`
- `explore`
- `plan`
- `general`

## Relationship to Sessions

- subagents run with their own execution context
- task and replay flows can bootstrap message context when needed
- skills with `context: fork` use the same isolated execution model

## Related Guides

- `tools.md`
- `skills.md`
