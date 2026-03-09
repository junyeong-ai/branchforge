# Tokens

Token tracking separates context-window management from billing-oriented accounting.

## Key Concepts

- input tokens
- cache read tokens
- cache write tokens
- output tokens

Context-window usage is based on prompt-side usage, not only fresh input tokens.

## Main Components

- `TokenBudget`
- `ContextWindow`
- `TokenTracker`

## What This Enables

- pre-flight validation
- context threshold warnings
- compaction decisions
- cost-aware execution support

## Related Guides

- `budget.md`
- `session.md`
