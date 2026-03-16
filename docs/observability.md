# Observability

The runtime provides three complementary observability layers.

## 1. Agent Events (Real-Time Streaming)

`AgentEvent` variants emitted during `execute_stream()`:

| Event | Data | Purpose |
|-------|------|---------|
| `Text(String)` | Incremental text | Streaming output |
| `Thinking(String)` | Model reasoning | Thinking/reasoning display |
| `ToolStart { id, name, input }` | Tool about to execute | Progress indicator |
| `ToolReview { id, name, input }` | Tool needs user approval | Human-in-the-loop (Supervised mode) |
| `ToolComplete { id, name, output, is_error, duration_ms }` | Tool finished | Duration tracking |
| `ToolBlocked { id, name, reason }` | Tool denied by policy/hook | Security audit |
| `TurnUsage { input/output/cache tokens, totals }` | Per-turn token consumption | Cost tracking |
| `Complete(AgentResult)` | Final result with `AgentMetrics` | Summary |

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

| EventKind | Emitted When | Payload |
|-----------|-------------|---------|
| `ToolExecuted` | Tool completes | tool_name, duration_ms, is_error |
| `TokensConsumed` | API response received | input/output tokens, model |
| `SessionCompacted` | Compaction completes | summary text, saved_tokens |
| `BranchForked` | Branch created | branch_name, ancestor |
| `CheckpointCreated` | Checkpoint created | label, note |
| `Custom(&str)` | User-defined | Any JSON |

EventBus subscribers never block agent execution. Use for Prometheus metrics, structured logging, or vector store indexing.

## 4. Hook-Based Observation

Non-blocking hooks (`PostToolUse`, `PostMessage`, `PostStreamChunk`) provide policy-compatible observation points. See [Hooks](hooks.md).

## Optional OpenTelemetry

Use the `otel` feature for OpenTelemetry integration.

## Related Guides

- [Hooks](hooks.md)
- [Budget](budget.md)
- [Tokens](tokens.md)
