# ADR-010: `MockLlmCall` scripted conversation helpers

**Status:** Accepted  •  **Date:** 2026-04-11

## Context

`MockLlmCall` has been the test double for `LlmCall` since the
early days of the runtime. The original API was a raw queue:

```rust
MockLlmCall::new()
    .then_response(ModelResponse { /* 8 hand-rolled fields */ })
    .then_stream(vec![
        Ok(ModelStreamChunk::MessageStart { /* 3 fields */ }),
        Ok(ModelStreamChunk::TextDelta { /* 2 fields */ }),
        // ... more chunks ...
        Ok(ModelStreamChunk::Finish { /* 2 fields */ }),
    ])
```

This was a working primitive but forced every test author to
hand-roll `ModelResponse` / `ModelStreamChunk` literals. Common
patterns like "assistant narrates then calls a tool" took 15–20
lines of test scaffolding before the behavioural assertion.

Worse, the hand-rolled chunk sequences for streaming tool calls
were error-prone: the correct shape is
`MessageStart → ToolCallStart → ToolCallArgsDelta → ToolCallEnd → Finish(ToolCalls)`,
and getting any part wrong produced cryptic agent-loop failures
that looked like runtime bugs rather than test setup bugs.

## Decision

Add two layers of ergonomic shortcuts — one on `ModelResponse`
(the IR type) and one on `MockLlmCall` (the test double) — so
scripted multi-turn conversations can be written without touching
a single JSON literal or chunk constructor.

### IR-level `ModelResponse` builders

```rust
impl ModelResponse {
    pub fn from_text(text: impl Into<String>) -> Self;             // existed
    pub fn from_tool_call(                                         // NEW
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self;
    pub fn from_text_and_tool_call(                                // NEW
        text: impl Into<String>,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self;
}
```

Both new constructors stamp `finish_reason = FinishReason::ToolCalls`
so the agent loop correctly dispatches the tool on the next turn.

### `MockLlmCall` shortcuts

```rust
impl MockLlmCall {
    pub fn then_text(self, text: impl Into<String>) -> Self;
    pub fn then_tool_call(
        self,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self;
    pub fn then_text_and_tool_call(
        self,
        text: impl Into<String>,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self;
    pub fn then_stream_text(
        self,
        deltas: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self;
    pub fn then_stream_tool_call(
        self,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self;
}
```

The streaming helpers assemble the full canonical chunk sequence
(`MessageStart → … → Finish`) so test authors cannot accidentally
skip or mis-order the stream shape.

### Example: three-turn scripted conversation

```rust
let mock = MockLlmCall::new()
    .then_text_and_tool_call(
        "Let me search",
        "call_1",
        "Search",
        json!({"query": "rust async"}),
    )
    .then_text("Here are the results")
    .then_text("Done");
```

Compared to the raw primitive, this is five lines vs. fifty, and
every literal that matters (text content, tool name, arguments)
is visible at the call site instead of buried inside struct
literals.

## Consequences

- **Tests become behavioural assertions, not setup scaffolding.**
  The ratio of setup to assertion in typical agent-loop tests
  drops by ~10x.
- **Stream sequence correctness is enforced.** Test authors can no
  longer mis-order chunks because the helper owns the full
  sequence — `then_stream_tool_call` always emits the five
  canonical chunks in the right order.
- **Two-layer design stays honest.** `ModelResponse::from_*`
  constructors live in the IR module because they produce IR
  types. `MockLlmCall::then_*` helpers live in the mock module
  because they queue into the mock's FIFO. Neither layer depends
  on the other — you can build a `ModelResponse` with the IR
  helpers and queue it via `then_response` directly, or use the
  shortcut.
- **No breaking change.** Raw `then_response(ModelResponse { … })`
  still works for tests that need exotic shapes the shortcuts
  don't cover.

## Alternatives rejected

- **Macro-based scripted DSL** (e.g. `script! { user "hi"; assistant "hello" }`).
  More ergonomic in the best case but much harder to debug when
  the macro expansion doesn't match the agent's expectation. The
  method-chain approach is explicit about types and keeps the
  full Rust toolchain (rustc errors, IDE jump-to-definition,
  rustdoc) working.
- **Recording/playback harness** that captures a real agent run
  and replays it. Higher-value for integration tests but
  out-of-scope for the unit-test path this ADR targets. Could
  be layered on top later without affecting this design.
- **Add an expectation API** (`mock.expect_prompt_contains("…")`).
  Would conflate the "emit this response" role with the "verify
  this input" role and make the queue's error messages harder to
  read. Explicit `assert!` on the captured request is clearer.
