# ADR-009: `StreamAggregator` — canonical consumer for `AgentEvent` streams

**Status:** Accepted  •  **Date:** 2026-04-11

## Context

`Agent::execute_stream()` yields a `Stream<Item = Result<AgentEvent>>`
where:

- Text output arrives as a sequence of `AgentEvent::Text { delta }`
  chunks that must be concatenated for rendering.
- Thinking / reasoning output arrives the same way via
  `AgentEvent::Thinking { content }`.
- Tool calls surface as `ToolStart` → (`ToolProgress`)* → `ToolComplete`
  with the results scattered across three event kinds.
- Token usage is updated only when `TurnUsage` arrives (once per
  API call).
- The terminal `Complete(Box<AgentResult>)` carries the final
  result and metrics.

Every chat UI / dashboard / CLI consumer has to accumulate this
state themselves. In practice, each caller reimplements the same
logic — track text buffer, maintain a `HashMap<tool_id, state>`,
update it on start/complete — invariably with subtle bugs around
insertion order, error paths, and "what if the stream ends before
Complete?".

Rewriting this accumulator for every SDK consumer is wasteful and
leaks the wire shape into application code.

## Decision

Introduce `StreamAggregator` as the canonical consumer. It
collapses any `AgentEvent` stream into a typed snapshot that UIs
can render on every update without hand-rolled state tracking.

```rust
pub struct StreamAggregator { /* ... */ }

impl StreamAggregator {
    pub fn new() -> Self;
    pub fn apply(&mut self, event: &AgentEvent);
    pub async fn drain<S>(stream: S) -> (Self, Option<crate::Error>)
        where S: Stream<Item = crate::Result<AgentEvent>> + Unpin;

    pub fn text(&self) -> &str;
    pub fn thinking(&self) -> &str;
    pub fn tools(&self) -> Vec<&ToolCallState>;  // insertion order
    pub fn tool(&self, id: &str) -> Option<&ToolCallState>;
    pub fn usage(&self) -> StreamUsage;
    pub fn is_complete(&self) -> bool;
    pub fn final_result(&self) -> Option<&AgentResult>;
    pub fn finish_reason(&self) -> Option<&FinishReason>;
}
```

`ToolCallState` captures the per-tool-call lifecycle:

```rust
pub struct ToolCallState {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    pub status: ToolCallStatus,  // Running | InReview | Succeeded | Failed | Blocked
    pub output: Option<String>,
    pub is_error: bool,
    pub duration_ms: Option<u64>,
    pub blocked_reason: Option<String>,
    pub progress: Vec<ToolProgressEntry>,
}
```

### Two entry points: `apply` and `drain`

- **`apply(&event)`** — the caller drives stream iteration and
  invokes `apply` on every event. UIs re-render after each call
  with the up-to-date snapshot. This is the normal pattern.
- **`drain(stream)`** — one-shot helper that consumes a whole
  stream and returns `(Self, Option<Error>)`. On error, the
  aggregator still reflects every event applied before the
  failure — no "throw away partial state on the first hiccup"
  anti-pattern.

### Insertion-order preservation with O(1) insert

Tool calls are stored in two parallel structures:

- `tool_map: HashMap<String, ToolCallState>` — O(1) lookup by id.
- `tool_order: Vec<String>` — insertion order for rendering.

On every tool event, `ensure_tool(id, name)` does one
`HashMap::contains_key` probe before the `entry().or_insert_with()`
call and pushes to `tool_order` exactly when the entry is new.
This is O(1) per event; the earlier implementation used
`Vec::contains` which was O(n) and would have degraded streams
with many tool calls.

## Consequences

- **Zero boilerplate at SDK consumers.** Chat UIs can be written as:
  ```rust
  let mut agg = StreamAggregator::new();
  let mut stream = agent.execute_stream(prompt).await?;
  while let Some(event) = stream.next().await {
      agg.apply(&event?);
      ui.render(agg.text(), agg.tools(), agg.usage());
  }
  ```
- **Partial state on error.** The `drain` helper returns the
  aggregator alongside the error so failure handling doesn't lose
  accumulated progress.
- **Library type with explicit consumer surface.** Unlike the W-7
  "zero-consumer library type" anti-pattern, this type exists
  specifically to serve external SDK consumers — the regression
  test suite proves the surface works end-to-end. Internal agent
  runtime code does not use it (it's the emitter, not a consumer).
- **Clone-per-snapshot cost.** `tools()` allocates a `Vec<&ToolCallState>`
  per call, which is fine for UI render loops (< 1 ms per call
  even with hundreds of tool entries). Callers who want
  zero-allocation access can read `tool(&id)` directly.

## Alternatives rejected

- **Expose a mutable-reference iterator instead of typed snapshot.**
  Would let UIs mutate internal state, which defeats the point.
  Read-only accessors keep the aggregator ownership clean.
- **Async event-driven renderer trait.** Would couple the
  aggregator to a specific rendering framework (tokio / async-std
  callback contracts). The current design is framework-agnostic —
  the caller drives both iteration and rendering.
- **Integrate the aggregator into the runtime's streaming emit
  path.** Would duplicate state (runtime already tracks text /
  thinking / tool_calls internally). The aggregator is for
  consumers, not the producer.
