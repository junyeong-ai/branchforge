# Observability

The runtime provides three complementary observability layers.

## 1. Agent Events (Real-Time Streaming)

`AgentEvent` variants emitted during `execute_stream()`. All variants implement `Serialize`/`Deserialize` with `#[serde(tag = "type", rename_all = "snake_case")]`:

| Event | Data | Purpose |
|-------|------|---------|
| `Text { delta }` | Incremental text chunk | Streaming output |
| `Thinking { content }` | Model reasoning | Thinking/reasoning display |
| `ToolStart { id, name, input }` | Tool about to execute | Progress indicator |
| `ToolReview { id, name, input }` | Tool needs user approval | Human-in-the-loop (Supervised mode) |
| `ToolComplete { id, name, output, is_error, duration_ms }` | Tool finished | Duration tracking |
| `ToolBlocked { id, name, reason }` | Tool denied by policy/hook | Security audit |
| `TurnUsage { input/output/cache tokens, totals }` | Per-turn token consumption | Cost tracking |
| `Complete(AgentResult)` | Final result with `AgentMetrics` | Summary |

Use `event.event_type()` for the type string (`"text"`, `"tool_start"`, etc.) and `serde_json::to_value(&event)` for canonical JSON serialization.

## 2. Agent Metrics (Aggregated Result)

`AgentMetrics` in the final `AgentResult` includes:

- `tool_stats: HashMap<String, ToolStats>` — per-tool call count, total time, errors
- `tool_call_records: Vec<ToolCallRecord>` — individual call details
- `model_usage: HashMap<String, ModelUsage>` — per-model token breakdown
- `total_cost_usd: Decimal` — cumulative cost across all providers
- `cache_read_tokens`, `cache_creation_tokens` — prompt caching efficiency
- `api_calls`, `api_time_ms` — API call count and latency

## 3. Event Bus (Non-Blocking Observability)

`EventBus` provides fire-and-forget event dispatch for metrics/logging:

| EventKind | Emitted By Runtime | Payload |
|-----------|-------------------|---------|
| `ToolExecuted` | Yes — tool completes | tool_name, duration_ms, is_error |
| `TokensConsumed` | Yes — API response | input/output tokens, model |
| `SessionCompacted` | Yes — compaction | summary text, saved_tokens |
| `BranchForked` | Available | branch_name, ancestor |
| `CheckpointCreated` | Available | label, note |
| `RequestSent` | Available | request metadata |
| `ResponseReceived` | Available | response metadata |
| `StreamChunk` | Available | chunk data |
| `Error` | Available | error details |
| `SessionChanged` | Available | session state |
| `BudgetAlert` | Available | budget threshold |
| `Custom(&str)` | User-defined | Any JSON |

Events marked "Available" are defined for subscriber use but not yet emitted by the runtime. Use `EventBus::emit()` in hooks or custom code to produce them.

Subscriptions return a `SubscriptionId` for manual removal, or use `subscribe_with_handle()` for RAII auto-unsubscribe:

```rust
let bus = Arc::new(EventBus::default());

// Manual: returns SubscriptionId
let id = bus.subscribe(EventKind::ToolExecuted, Arc::new(|event| { /* ... */ }));
bus.unsubscribe(EventKind::ToolExecuted, id);

// RAII: auto-unsubscribes when handle is dropped
let handle = bus.subscribe_with_handle(EventKind::TokensConsumed, Arc::new(|event| { /* ... */ }));
drop(handle); // subscriber removed
```

EventBus subscribers never block agent execution. Use for Prometheus metrics, structured logging, or vector store indexing.

## 4. Hook-Based Observation

Non-blocking hooks (`PostToolUse`, `PostMessage`, `PostStreamChunk`) provide policy-compatible observation points. See [Hooks](hooks.md).

## Optional OpenTelemetry

Use the `otel` feature for OpenTelemetry integration.

## Related Guides

- [Hooks](hooks.md)
- [Budget](budget.md)
- [Tokens](tokens.md)
