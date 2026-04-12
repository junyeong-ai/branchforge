---
paths:
  - "src/**"
---

# Architecture Invariants

1. **SSoT, no cache** — `SessionGraph` is the only source of truth. `Session::current_branch_messages()` rebuilds from the graph every call. No `cached_messages`, no message-mirror field, no versioned snapshot. See `graph-session.md`.

2. **Fire-and-forget event** — `EventBus::emit` never `.await`s and never blocks on a subscriber. `OverflowPolicy` may only be `Drop` or `WarnAndDrop`. See `events.md`.

3. **3-axis orthogonality** — `ModelCodec × ModelTransport × EndpointShape`. Axis collapse (e.g. a codec doing auth, a transport doing encoding) is forbidden. See `client.md`.

4. **Capability honesty** — A codec declaring `json_schema: Native` must emit a wire-level schema on the send path. Enforced by `tests/codec_contract.rs::capability_honesty_*`.

5. **No dual systems** — Introducing a new abstraction requires deleting the legacy in the **same PR**. No `// deprecated`, no feature-flagged shim, no parallel implementations.

6. **Structured error classification** — HTTP status + parsed JSON body. `body.contains("ExceptionName")` on the raw body is forbidden: tool outputs containing the substring produce misclassification. Field-scoped substring matching (e.g. `parsed["__type"]`) is permitted when no structured code field exists.

7. **Exact identity matching** — Model ids, command names, tool names. Substring fallbacks for identity resolution are forbidden.

8. **Thresholds are Config fields** — Every tunable lives on a `*Config` struct with doc and default. Inline magic numbers in execution paths are forbidden.

9. **Typed domain newtypes** — `TokenCount`, `NodeId`, `BranchId`, `Cost`. Raw `u64`/`String` for domain quantities is forbidden.

10. **Closed-list FSM transitions** — `SessionState`, `PlanState`, `QueueItemState`, `McpClientState`, `AgentState` mutate **only** via `transition_to(_) -> Result<_, TransitionError>`. Direct field assignment outside the helper is forbidden. Construction and persistence rehydration are carved out with `// fsm-init:` or `// fsm-rebuild:` markers. Enforced by `cargo test --lib audit_fsm_bypass`.
