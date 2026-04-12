---
paths:
  - "src/events/**"
  - "src/agent/common.rs"
  - "src/agent/stream_aggregator.rs"
---

# Events Module Rules

Non-blocking observability bus. Complements `HookRegistry` (blocking,
security-critical) — events here are fire-and-forget and must never
block or cancel execution.

## Dispatch layers

- **Untyped core** (`EventBus::emit` / `subscribe`) delivers
  `Event { kind, data: serde_json::Value }` to per-kind subscribers
  via bounded mpsc drainers. Each subscriber owns one drainer task.
- **Typed facade** (`EventBus::emit_typed<D>` / `subscribe_typed<D>`)
  wraps the untyped path with a Serde-backed `EventPayload` trait.
  Typed and untyped subscribers coexist on the same kind — typed
  emits are visible to raw-JSON subscribers and vice versa.

Rules:

- **Prefer the typed path for all built-in event kinds.** Every
  event kind declared in `EventKind` should have a matching
  `*Payload` struct implementing `EventPayload` with
  `const KIND = EventKind::Foo`.
- **`Custom(&'static str)` stays untyped.** The trait's `const KIND`
  cannot express a parameterised discriminant. Applications that
  want typed custom events serialise their own struct and call
  `emit_simple(Custom("name"), value)`.
- **Never hand-roll `serde_json::json!({…})` at an emit site for a
  built-in kind.** Build the typed payload struct and call
  `emit_typed`. Grep for `emit_simple` in non-test code should
  surface only `Custom(…)` sites.
- **Overflow is counted, not fatal.** `EmitStats.delivered /
  dropped` surface lag; subscribers configure `OverflowPolicy::Drop`
  or `WarnAndDrop`. Never block the emitter on a slow subscriber.

## StreamAggregator (consumer facade)

`StreamAggregator` is the canonical consumer for `AgentEvent`
streams produced by `Agent::execute_stream()`. UI / CLI / dashboard
callers should consume via `apply(&event)` or `drain(stream)`
instead of re-implementing tool-call state tracking.

- `tool_order` uses an `ensure_tool(id, name)` helper that probes
  the `tool_map` before insertion. O(1) per event. **Never use
  `Vec::contains` inside `apply`** — it creates O(n²) per-event cost.
- `drain(stream)` returns `(Self, Option<Error>)` — partial state
  is preserved when the stream errors mid-flight. Do not change
  this signature to throw away the aggregator on error.
- The aggregator is **consumer-facing**. Internal runtime code
  (streaming.rs, execution.rs) is the emitter, not a consumer —
  do not wire `StreamAggregator` into the runtime itself.

## BudgetAlertPayload wire format

- `used_usd` / `limit_usd` / `remaining_usd` are `Decimal`
  (string-serialized via `rust_decimal`'s `serde-with-str` feature).
- `utilization: f64` is a `[0.0, 1.0]` ratio, not a percentage
  string. Clients that want `"75%"` format on the string side.

Decimal fields are string-serialized for precision; `utilization` is a ratio, not a percentage.
