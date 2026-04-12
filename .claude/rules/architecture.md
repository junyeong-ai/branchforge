---
description: Long-term architecture invariants. Design-review findings contradicting this list auto-reject. Past violations logged in memory/findings_resolved.md.
paths: "src/**"
---

# Architecture Invariants

1. **SSoT, no cache** — `SessionGraph` is the only source of truth. `Session::current_branch_messages()` rebuilds from the graph every call. No `cached_messages`, no message-mirror field, no versioned snapshot. See `graph-session.md`. (Past violation: F-rej-004)

2. **Fire-and-forget event** — `EventBus::emit` never `.await`s and never blocks on a subscriber. `OverflowPolicy` may only be `Drop` or `WarnAndDrop`. See `events.md`. (Past violation: F-rej-003)

3. **3-axis orthogonality** — `ModelCodec × ModelTransport × EndpointShape`. Axis collapse (e.g. a codec doing auth, a transport doing encoding) is forbidden. See `client.md`.

4. **Capability honesty** — A codec declaring `json_schema: Native` must emit a wire-level schema on the send path. Enforced by `tests/codec_contract.rs::capability_honesty_*`.

5. **No dual systems** — Introducing a new abstraction requires deleting the legacy in the **same PR**. No `// deprecated`, no feature-flagged shim, no parallel implementations. See `naming.md`.

6. **Structured error classification** — HTTP status + parsed JSON body. `body.contains("ExceptionName")` is forbidden: tool outputs containing the substring produce misclassification.

7. **Exact identity matching** — Model ids, command names, tool names. `contains("opus")` matches `tempus-maximus`. Substring fallbacks for identity are forbidden.

8. **Thresholds are Config fields** — No `const DEFAULT_X: f32 = 0.8` embedded in execution paths. Every tunable lives on a `*Config` struct with doc and default.

9. **Typed domain newtypes** — `TokenCount`, `NodeId`, `BranchId`, `Cost`. Raw `u64`/`String` for domain quantities is forbidden.

10. **Closed-list FSM transitions** — `SessionState`, `PlanState`, `QueueItemState`, `McpClientState`, `AgentState` mutate **only** via `transition_to(_) -> Result<_, TransitionError>`. Direct field assignment outside the helper is forbidden. See `naming.md`.
