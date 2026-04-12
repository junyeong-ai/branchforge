---
paths:
  - "src/**"
---

# Naming Conventions

Load-bearing naming rules that keep the codebase consistent. Follow
them in new code; violations will block review.

## Type suffix taxonomy

Each suffix has a specific meaning. Mixing them is how
inconsistency accumulates — always pick the suffix that describes
the **role**, not the data shape.

| Suffix | Meaning | Example |
| :--- | :--- | :--- |
| `Manager` | Owns runtime lifecycle — spawn, shutdown, reconnect. Holds live objects with mutable state (typically `Arc<RwLock<…>>`). | `McpManager`, `SessionManager`, `ToolSearchManager` |
| `Registry` | Init-time keyed lookup table **or** observer/subscription aggregator. Populated from config or via `register`/`unregister` at runtime, but **without** owning a lifecycle (no spawn/shutdown/reconnect). Pure data + dispatcher. | `ProfileRegistry`, `RecipeRegistry`, `McpToolsetRegistry`, `HookRegistry` |
| `Tracker` | Runtime state map keyed by dynamic ids (not init-time keys). Mutated as work begins and ends. | `TaskTracker`, `BudgetTracker` |
| `Catalog` | Dynamic, externally-sourced entries discovered at runtime. | `ToolCatalog` |
| `Store` | Persistent storage interface. | `MemoryStore`, `InMemoryStore` |
| `Set` | Read-only grouping. | `PermissionSet` |
| `Engine` | Pure compute / deterministic decision engine. No mutable Arc fields. | `SearchEngine` (wraps regex/BM25), `RecoveryExecutor` (acts, but deterministic) |
| `Aggregator` | Consumer-facing state reducer that consumes an event/stream sequence into a typed snapshot. | `StreamAggregator` |
| `Snapshot` | Point-in-time immutable view returned by value. Name signals "read-only clone, safe to send". | `SessionSnapshot`, `McpServerSnapshot` |
| `Payload` | Typed event/message body paired with a discriminant via a trait `const`. | `TokensConsumedPayload`, `BranchForkedPayload` |

**Rule**: when adding a new type, read its primary responsibility
out loud. If you find yourself saying "it registers X at build time"
it is a `Registry`; "it manages live X connections" → `Manager`; "it
tracks X as tasks come and go" → `Tracker`.

**Registry vs Manager disambiguation**: the presence of `register` /
`unregister` methods alone does **not** make a type a `Manager`. The
disambiguator is whether the type **owns** lifecycle (spawn/shutdown/
reconnect of live resources). `HookRegistry` has `register`,
`unregister`, and rebuilds an internal dispatch cache — but it never
spawns tasks, opens sockets, or manages a connection pool, so it
stays a `Registry`. `McpManager`, by contrast, maintains live
transport connections through `RunningService` handles and is
unambiguously a `Manager`.

## State machine terminology

FSM state types use the `State` suffix, not `Phase`, `Status`, or
`Kind`:

- `SessionState` — canonical session lifecycle FSM.
- `McpClientState` — MCP client handshake FSM.
- `ExecutionState`, `AgentState` — execution-loop state types.
- `PlanState` — 6-variant plan FSM with `transition_to`
  validation (Draft → Approved → Executing → terminal).
- `QueueItemState` — 4-variant queue item FSM
  (Pending → Processing → terminal).

Transition errors use the `<Subject>TransitionError` suffix:

- `SessionTransitionError` — illegal `SessionState` move.
- `PlanTransitionError` — illegal `PlanState` move.
- `QueueItemTransitionError` — illegal `QueueItemState` move.

### State vs Status

| Suffix | Meaning | Examples |
| :--- | :--- | :--- |
| `State` | Validated forward-only FSM with `transition_to -> Result<_, TransitionError>`. Mutation must go through the transition helper. | `SessionState`, `PlanState`, `QueueItemState`, `McpClientState` |
| `Status` | Pure classifier / snapshot (return-of-check, tagged result). No transition semantics, liberal mutation allowed. | `BudgetStatus`, `WindowStatus`, `ProgressStatus`, `TodoStatus`, `DirectoryEntryStatus` |

**Rule**: if the enum is mutated through named methods like
`approve()` / `start()` / `complete()` and order matters, it is a
`State` and the mutation must go through a validated
`transition(next)` method. If the enum is a classifier returned
from a `check()` function and nobody mutates it in place, it is a
`Status`.

## Error type naming

Two patterns, both valid:

- **`<Module>Error`** for the module's catch-all error enum.
  Examples: `SessionError`, `McpError`, `GraphError`, `ConfigError`.
- **`<Module><Op>Error`** for a tightly-scoped error that describes
  one specific failure surface. Examples: `SchemaValidationError`,
  `PermissionDslError`, `SessionTransitionError`.

Avoid: abbreviated forms (`ValidErr`), generic suffixes (`Issue`,
`Problem`, `Failure`), or scoped types without a prefix (`ParseError`
without saying what parses).

## Public enum evolution contract

Every `pub enum` in `src/` MUST declare its evolution contract as
either **closed** or **open**, and the enum MUST be marked
accordingly:

- **Closed** (no `#[non_exhaustive]`) — domain is mathematically
  fixed. Adding a variant is a major redesign. Consumers
  exhaustively match; compile failures on redesign are a feature.
  Examples: FSMs (`SessionState`, `PlanState`, `QueueItemState`,
  `McpClientState`, `AgentState`), `SchemaVersionMismatchDirection`,
  `Role`.

- **Open** (`#[non_exhaustive]`) — domain evolves over time.
  Adding a variant is a minor bump. Every other `pub enum`.

When adding a new `pub enum`, read `docs/architecture/enum-evolution.md`
for the canonical registry and append it to the correct list in the
same PR.
