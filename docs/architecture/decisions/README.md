# Architecture Decision Records

This directory holds short, immutable notes documenting the load-bearing
design decisions of BranchForge. Each ADR answers: **what did we decide,
why, and what would have to change for us to revisit it?**

ADRs are not maintained. Once accepted, they describe history. If a
decision is revisited, write a new ADR that supersedes the old one and
link both ways.

## Format

Every ADR follows the same four-section skeleton:

1. **Context** — the problem and the constraints that forced a choice.
2. **Decision** — the one-line verdict and the shape of the design.
3. **Consequences** — what this buys us, what it costs, what downstream
   code now assumes.
4. **Alternatives rejected** — the paths we considered and why they lost.

## Index

| # | Title | Status |
| :--- | :--- | :--- |
| [001](001-profile-registry-over-preset-enum.md) | ProfileRegistry over Preset enum | Accepted |
| [002](002-session-state-fsm-delete-dead-enums.md) | Unified `SessionState` FSM; delete dead lifecycle enums | Accepted |
| [003](003-system-block-role-typed-boundary.md) | `SystemBlockRole` typed boundary | Accepted |
| [004](004-jsonschema-crate-dependency.md) | `jsonschema` crate for structured-output validation | Accepted |
| [005](005-recovery-recipe-pure-decision.md) | `RecoveryRecipe` pure decision + executor | Accepted |
| [006](006-four-layer-feature-gating.md) | 4-layer feature gating | Accepted |
| [007](007-checkpoint-is-session-plus-budget.md) | Checkpoint captures Session + Budget, nothing else | Accepted |
| [008](008-typed-event-payloads.md) | Typed `EventPayload` trait for built-in event kinds | Accepted |
| [009](009-stream-aggregator-canonical-consumer.md) | `StreamAggregator` — canonical consumer for `AgentEvent` streams | Accepted |
| [010](010-mock-llm-call-scripted-helpers.md) | `MockLlmCall` scripted conversation helpers | Accepted |
