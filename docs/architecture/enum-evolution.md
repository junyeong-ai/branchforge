# Public Enum Evolution Registry

Reference for the closed/open enum classification contract.
Core rules live in `.claude/rules/naming.md` — this file is the
canonical registry of which enums belong to which list.

## Closed list (MUST NOT have `#[non_exhaustive]`)

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

## Open list (MUST have `#[non_exhaustive]`)

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
