# Product Vision

This project is a Rust-native runtime for stateful coding agents.

The runtime is built around five product bets:

- Session graphs, not flat chat logs
- Capability-aware multi-provider execution
- Safe local execution with auditable tool use
- Replayable, exportable, shareable work history
- Clean boundaries between runtime, prompting, credentials, and transport

The system optimizes for long-lived engineering workflows rather than single-turn API access.

## Non-goals

- Reproducing any vendor CLI behavior as a product goal
- Hiding provider differences behind a leaky generic surface
- Keeping legacy naming or compatibility layers after better boundaries exist
- Shipping optional abstractions that add indirection without operational value

## Core Design Rules

1. Session state is modeled as an append-only graph of events and derived views.
2. Execution writes facts to the graph; it does not mutate ad hoc state spread across modules.
3. Credentials, prompts, and provider transport are separate concerns.
4. Unsupported provider capabilities are rejected or degraded explicitly.
5. Every major subsystem must be testable in isolation and observable in production.
