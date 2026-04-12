# ADR-008: Typed `EventPayload` trait for built-in event kinds

**Status:** Accepted  •  **Date:** 2026-04-11

## Context

`EventBus` delivers `Event` values to subscribers. Each `Event`
carries a `kind: EventKind` discriminator and an untyped
`data: serde_json::Value` payload. The `data` field was convenient
for external observability integrations (OTel, dashboards) but was
the source of two persistent problems inside the runtime:

1. **Every emit site hand-rolled a `serde_json::json!({…})` literal.**
   The same `TokensConsumed` shape appeared in three places with
   three slightly-different field orderings. Adding a field was a
   grep-and-hope operation.

2. **Every subscriber decoded fields by hand.** Consumers that
   cared about, say, `duration_ms` on a `ToolExecuted` event had
   to pull it out of the JSON value, handle the `Option`, and
   hope the shape matched what the emitter was currently writing.

Worse, `EventKind::StreamChunk` had **three sub-shapes**
(`chunk_type: "text" | "thinking" | "tool_use"`) inside a single
untyped payload — a stringly-typed mini-enum that no static check
could verify.

## Decision

Introduce an `EventPayload` trait that pins a Serde-backed struct
to a specific `EventKind`:

```rust
pub trait EventPayload:
    Clone + Debug + Serialize + DeserializeOwned + Send + Sync + 'static
{
    const KIND: EventKind;
}
```

Each built-in event kind has a matching payload:

| Payload | `KIND` |
| :--- | :--- |
| `TokensConsumedPayload` | `EventKind::TokensConsumed` |
| `ToolExecutedPayload` | `EventKind::ToolExecuted` |
| `ToolProgressPayload` | `EventKind::ToolProgress` |
| `BudgetAlertPayload` | `EventKind::BudgetAlert` |
| `BranchForkedPayload` | `EventKind::BranchForked` |
| `CheckpointCreatedPayload` | `EventKind::CheckpointCreated` |
| `StreamChunkPayload` (contains `StreamChunkKind` enum) | `EventKind::StreamChunk` |
| `SessionCompactedPayload` | `EventKind::SessionCompacted` |

`EventBus` gains two dispatch methods:

```rust
pub fn subscribe_typed<D: EventPayload, F>(&self, cb: F) -> SubscriptionId
    where F: Fn(D) + Send + Sync + 'static;

pub fn emit_typed<D: EventPayload>(&self, data: D) -> EmitStats;
```

The typed emit path serializes `data` to `serde_json::Value` and
dispatches through the existing `emit` path, so **typed and
untyped subscribers coexist on the same event kind** — consumers
that want raw JSON access keep working alongside consumers that
want decoded structs.

### `StreamChunkKind` as a tagged union

The worst offender was the `StreamChunk` event. Instead of a flat
payload with a stringly-typed `chunk_type` field, the typed
payload wraps a proper Rust enum:

```rust
#[serde(tag = "chunk_type", rename_all = "snake_case")]
pub enum StreamChunkKind {
    Text { length: usize },
    Thinking { length: usize },
    ToolUse { tool_name: String },
}
```

Serde's internally-tagged representation produces the same wire
shape as the old hand-rolled JSON, so external dashboards that
parsed the old format keep working.

### `Custom` events stay untyped

`EventKind::Custom(&'static str)` is the extensibility escape hatch
and parameterises its discriminant at runtime. The `EventPayload`
trait's `const KIND` contract cannot express a parameterised kind,
so Custom events intentionally retain the untyped `emit_simple`
path. Applications that want type-safe custom events can serialise
their own struct to JSON and call `emit_simple(Custom("name"), value)`.

### `BudgetAlertPayload` wire-format cleanup

The original `BudgetAlert` payload stored every monetary field as
a string (`"7.50"`) and an extra `percentage` display string. The
typed payload uses `Decimal` directly (serialized as a string via
`rust_decimal`'s `serde-with-str` feature, preserving precision)
and replaces `percentage: String` with `utilization: f64` in the
`[0.0, 1.0]` range. This is an intentional wire-format break — no
consumer depended on the old format, and numeric values make
dashboards easier to graph without client-side parsing.

## Consequences

- **Emit-site boilerplate eliminated.** Six `agent/common.rs` and
  `src/graph/session_graph.rs` helpers now build typed structs
  instead of `json!({…})` literals. New fields are added to the
  payload struct and caught at compile time everywhere it's used.
- **Consumer-side type safety.** UIs and dashboards can register
  `bus.subscribe_typed::<TokensConsumedPayload>(|data| …)` and
  receive the decoded struct with no JSON poking.
- **Typed/untyped coexistence.** The raw `subscribe(kind, Fn(Event))`
  path is unchanged. Integrations that already consume `Event.data`
  directly keep working with the new wire shapes.
- **One escape hatch.** Custom events stay untyped; this is called
  out in the `events::typed` module docs so future contributors
  don't try to force them through the trait.

## Alternatives rejected

- **Enum payload instead of trait.** A single `EventPayload` enum
  with one variant per kind would force pattern-matching in every
  subscriber closure, losing the ergonomic "one handler per kind"
  shape. Rejected.
- **Box&lt;dyn Serialize&gt; as the canonical payload type.** Would
  preserve structure at the emit site but lose it at the
  subscriber (no downcasting back to the original type). Rejected.
- **Breaking the untyped API.** Would force every existing
  consumer to migrate immediately. Rejected — the typed API is
  additive, and the untyped path is a legitimate integration
  surface for OTel exporters.
