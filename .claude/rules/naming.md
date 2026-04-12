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
- `McpClientState` — MCP client handshake FSM (renamed from
  `LifecyclePhase` in W-17).
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

- **Closed** — domain is mathematically fixed. Adding a variant is
  a major redesign (e.g., FSM transition graph rewrite). The enum
  is **not** marked `#[non_exhaustive]` because consumers are
  expected to exhaustively match it and compile failures on
  redesign are a feature, not a bug.

- **Open** — domain evolves over time. Adding a variant is a
  minor bump. The enum **is** marked `#[non_exhaustive]` so new
  variants never silently break downstream exhaustive matches.

### Closed list (MUST NOT have `#[non_exhaustive]`)

- **FSMs** — mutation goes through `transition_to -> Result`:
  `SessionState`, `PlanState`, `QueueItemState`, `McpClientState`,
  `AgentState`.

  **Construction vs mutation.** The `transition_to` rule applies to
  *runtime mutation* — advancing a live FSM from one state to the
  next. It does **not** apply to:

  1. **Struct initialization / fork / clone-then-reset** — creating
     a new owned value in the initial state. Mark the assignment
     with `// fsm-init: <rationale>`.
  2. **Event-log rehydration** in persistence backends — rebuilding
     state from a durable event log is replay, not mutation. Files
     under `src/session/persistence_*.rs` and `src/session/archive.rs`
     are excluded from the audit. In-file sites elsewhere use
     `// fsm-rebuild: <rationale>`.
  3. **Validated reset** — a semantic that is not a forward DAG move
     (e.g. terminal → Created) MUST be added as its own method on
     the FSM type (e.g. `SessionState::try_reset`) and return a
     `TransitionError` on illegal preconditions. Callers use the
     method, not direct assignment.

  Enforced by `scripts/audit_fsm_bypass.py`.
- **Mathematical binary** — two-variant symmetry that cannot grow:
  `SchemaVersionMismatchDirection` (`TooOld` / `TooNew`).
- **SSoT design commitment** — variants are fixed by a project
  invariant: `Role` (provider-neutral 3-role unification — System
  lives as a top-level field on `ModelRequest`, not a message role).

### Open list (MUST have `#[non_exhaustive]`)

Every other `pub enum`. The most common categories:

- Error enums (`Error`, `SessionError`, `McpError`, `GraphError`,
  `ConfigError`, `PluginError`, `SecurityError`, `SandboxError`,
  `PermissionDslError`, `SchemaValidationError`, `DecodeError`,
  `QueueError`, …)
- Category / kind / reason: `FailureCategory`, `ProviderErrorKind`,
  `EventKind`, `ModelWarning`, `FinishReason`, `NodeKind`,
  `HookEvent`, `HookSource`, `HookConfig`, `CompactSkipReason`,
  `CompactTrigger`, `PermissionDeniedReason`, …
- Output / content / format: `ToolOutput`, `ToolOutputBlock`,
  `ContentPart`, `MediaSource`, `ToolResultContent`,
  `ReasoningContent`, `ReasoningKind`, `ReasoningEffort`,
  `ResponseFormat`, `ToolChoice`, `StreamFraming`, `ModelStreamChunk`,
  `StreamChunkKind`, `Continuation`, `SystemBlockRole`,
  `SystemPrompt`.
- Decision / policy / status: `PermissionDecision`,
  `ToolRuleDecision`, `RuleDecisionKeyword`, `ExecutionMode`,
  `BudgetStatus`, `WindowStatus`, `TodoStatus`,
  `DirectoryEntryStatus`, `BudgetExceedPolicy`, `OverflowPolicy`,
  `CacheStrategy`, `SystemPromptMode`, `SessionExecutionMode`,
  `RecoveryAction`, `RecipeDecision`, `RecoveryOutcome`,
  `CircuitState`, `PreflightResult`, `PricingTier`,
  `FallbackTrigger`, `Support`, `ToolIdSemantics`,
  `CacheGranularity`, `SystemPromptShape`, `TreeRenderMode`,
  `GraphReference`, `TreeRenderMode`, `ContainerRuntime`,
  `PathContext`, `ObjectClosure`, `RequiredHandling`,
  `MinItemsPolicy`, `CredentialKind`, `Credential`, `Auth`,
  `DirectAuth`, `CredentialHint`, `GeminiCacheAuth`, `SyncMode`,
  `JsonlEntry`, `SchemaIssue`, `McpContent`, `McpServerConfig`,
  `ContentSource`, `SourceType`, `DirAction`, `SkillExecutionKind`,
  `GraphEventBody`, `AgentEvent`, `ToolCallStatus`, `MockResponse`,
  `ServerTool`, `ToolSearchTool`, `SubjectPattern`, `SessionType`,
  `CompactionPlan`, `PreparedCompact`, `CompactResult`,
  `ToolApprovalResponse`, `HumanInteractionError`, `QueueOperation`,
  `ModelFamily`, `ModelRole`, `ProviderKind`, `ValueType`,
  `SettingsSource`, `HeaderValue`.

When adding a new `pub enum`, append it to the correct list above
in the same PR. Reviewers MUST verify the classification matches
the domain semantics.

## "No dual systems" rule

When introducing a new abstraction, either **fully integrate** it
into the existing surface or **delete the legacy**. Do not leave
parallel systems living side by side — that is how the W-7 audit
found three lifecycle enums and one zero-consumer `SubagentState`.

Concretely:

- If you add a new `Validator` type, audit every call site of the
  old validator in the same PR. Migrate or delete.
- If you add a new `Registry` pattern, migrate every hand-rolled
  lookup map. Do not leave "legacy" and "new" side-by-side.
- A library type with **zero production consumers** is a smell.
  Delete it or wire it up in the same PR that introduces it.

See ADR-002 for the canonical example of a "delete, do not merge"
refactor that was worth doing.
