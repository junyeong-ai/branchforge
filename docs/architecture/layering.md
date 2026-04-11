# Architecture: Four-Layer Separation

> Status: **Phase 0 blueprint** for 1.0 release.
> This document is the single source of truth for layer boundaries, dependency rules, and module placement. All Phase 1 work (tasks #2–#10, #41, #42) is governed by this contract.
>
> **Revision note (2026-04-10)**: The initial blueprint used a three-layer model that collapsed all file-touching concerns into a single `coding-tools` feature. Review of the module graph (tree-sitter is localised to `src/security/bash/`; Read/Write/Edit/Glob/Grep carry no bash dependency) plus a legitimate use-case concern (local-machine research, knowledge-management, and data-analysis agents benefit from filesystem tools without ever running shell commands) showed that the right granularity is **four layers**: a new `local-fs` tier sits between pure core and coding-agent. `coding-tools` now depends on `local-fs` transitively. This document reflects the post-review model.

## 1. Motivation

BranchForge targets the full spectrum of agent workloads, from **pure API agents** (customer support bots, chat UIs, server-side automation) through **local-machine general agents** (research assistants reading local PDFs, knowledge workers grepping notes, data analysts inspecting CSV/JSON files, reporting pipelines writing to local disk) to **coding agents** (Claude Code-class tooling with bash execution, git awareness, CLAUDE.md conventions).

A single undifferentiated codebase cannot honestly serve all three:

- **Pure API agents** do not need filesystem access at all, let alone bash analysis. Forcing `working_dir`, `SecureFs`, or `SecurityContext` into `AgentConfig` means server deployments pay for unused code and face an attack surface they never intended.
- **Local general agents** need safe file operations (Read/Write/Edit/Glob/Grep + TOCTOU-protected filesystem paths + FS sandboxing) but do **not** need bash AST analysis, process rlimits, or git state — those are coding-specific concerns that carry their own dependencies (`tree-sitter`, `tree-sitter-bash`, process schedulers).
- **Coding agents** need everything above plus bash execution with layered safety, command-namespace sandboxing (unshare), and opinionated conventions like CLAUDE.md discovery.

The right granularity is **four layers with enforced dependency direction**. Each layer is a distinct concern, carries its own opt-in cost, and matches a real deployment topology.

## 2. The Four Layers

```
┌─────────────────────────────────────────────────────────────────┐
│  Layer 3 — CLOUD PROVIDERS                                       │
│  Feature-gated transport/auth adapters for AWS/GCP/Azure.        │
│  (aws / gcp / azure / cloud-all)                                │
└──────────────────────────────┬──────────────────────────────────┘
                               │ depends on Layer 1
┌──────────────────────────────▼──────────────────────────────────┐
│  Layer 2b — CODING AGENT EXTENSION (feature "coding-tools")      │
│  Bash with AST validation, process scheduler, unshare command   │
│  sandbox, ResourceLimits, git context, CLAUDE.md conventions,   │
│  bash subagent. Depends on Layer 2a.                            │
└──────────────────────────────┬──────────────────────────────────┘
                               │ depends on Layer 2a
┌──────────────────────────────▼──────────────────────────────────┐
│  Layer 2a — LOCAL FILESYSTEM TOOLS (feature "local-fs")          │
│  Workspace concept, SecureFs (TOCTOU-safe), Read/Write/Edit,    │
│  Glob/Grep, Landlock/Seatbelt path sandbox, explore/plan        │
│  subagents, generic markdown memory loader. Useful for ANY      │
│  local-machine agent: research, knowledge, data analysis.       │
└──────────────────────────────┬──────────────────────────────────┘
                               │ depends on Layer 1
┌──────────────────────────────▼──────────────────────────────────┐
│  Layer 1 — PURE CORE (always-on)                                 │
│  Agent runtime, IR, provider client stack, session graph,       │
│  tool trait, execution context (Extensions TypeMap), hooks,    │
│  memory trait, authorization, budget, observability, errors,   │
│  NetworkSandbox (HTTP egress control).                           │
└─────────────────────────────────────────────────────────────────┘
```

### Deployment topologies this supports

| Topology | Features | Example |
|---|---|---|
| **Pure API agent** | `anthropic-direct` | Slack support bot. No filesystem. Custom tools only. |
| **Local research agent** | `anthropic-direct`, `local-fs` | Reads local PDFs/markdown notes, greps a knowledge base, writes a report. No shell. |
| **Local data agent** | `anthropic-direct`, `local-fs` | Inspects CSV/JSON files, writes summaries. No shell. |
| **Knowledge-worker agent** | `anthropic-direct`, `local-fs`, `multimedia` | Second-brain / digital-garden agent over local markdown + PDF. No shell. |
| **Coding agent** | `anthropic-direct`, `coding-tools` (transitively `local-fs`) | Full Claude Code-class tool surface. |
| **Self-hosted coding agent** | `coding-tools`, `aws`/`gcp`, `persistence-all`, `otel` | Production coding service. |

The local-fs tier is the point of the whole exercise: **a local general agent pays for filesystem tools but not for `tree-sitter`, `BashAnalyzer`, process rlimits, or git scanning**. Its attack surface and dependency footprint are strictly smaller than a coding agent's.

### 2.1 Layer 1 — Pure Core (unconditional)

**Goal**: `cargo build --no-default-features --features "anthropic-direct"` produces a minimal, general-purpose agent SDK with **zero** filesystem, bash, git, or workspace assumptions. It drives a pure-API agent (customer support, chat) without pulling in a single file-touching line.

**Modules (`src/`)**
| Module | Purpose | Notes |
|---|---|---|
| `agent/` | `Agent`, `AgentBuilder`, `AgentRuntime`, `AgentConfig`, `AgentEvent`, `AgentResult`, turn loop, streaming | `AgentConfig` must NOT contain `working_dir`. Extensions TypeMap carries layer-2+ config. |
| `client/` | `ModelCodec`, `ModelTransport`, `EndpointShape`, `ProviderClient`, `LlmCall`, `Preset`, `RetryingClient`, `CircuitBrokenClient`, `FallingBackClient` | 3-axis provider stack. |
| `ir/` | `ModelRequest`, `ModelResponse`, `ContentPart`, `Message`, `Role`, `ModelSettings`, `ProviderCapabilities`, `ProviderOptions`, `SystemPrompt`, `ToolDefinition`, `Usage`, warnings | Provider-neutral canonical domain model. |
| `graph/` | `SessionGraph`, `GraphEvent`, `Branch`, `Checkpoint`, `Bookmark`, `GraphValidator`, `GraphMaterializer`, `NodeId`, `BranchId` | Append-only event log + materializer. SSoT for session state. |
| `session/` | `Session`, `SessionManager`, `SessionState`, `SessionId`, `MemoryStore`, `InMemoryStore`, compaction, archival, fork | No workspace concept; pure graph orchestration. |
| `tools/` | `Tool` trait, `SchemaTool` trait, `ToolRegistry`, `ExecutionContext` (Extensions TypeMap), `ProgressBuilder`, `ToolSurface` (core variant only: `Plan`, `TodoWrite`, `GraphHistory`, `Skill`) | See §4 for Extensions pattern. |
| `hooks/` | `Hook` trait, `HookManager`, `HookEvent`, `HookContext`, `HookOutput` | In-process and shell-command hooks both live here. Shell hooks do not validate bash — that is a Layer 2b concern. |
| `authorization/` | `ToolPolicy`, `ExecutionMode`, HITL approval channel, permission rule DSL (Phase 6+) | Generic tool-call authorization; no domain assumptions. |
| `budget/` | `BudgetTracker`, `CostSummary`, `BudgetConfig`, preflight estimator (task #39) | Preflight + post-hoc accounting. |
| `observability/` | Span definitions, metrics, OTEL bridge (feature-gated), lifecycle tracking, FailureCategory | OTEL export behind `otel` feature; core has always-on `tracing` spans. |
| `context/` | `MemoryLoader` trait, `MemoryProvider` trait, `PromptOrchestrator`, `RuleSet`, `StaticContext`, generic `EnvironmentSource` trait | Trait-only for extension points. Concrete implementations live in upper layers. |
| `prompts/` | Generic system prompt builder, minimal environment block (model, date, platform only — no cwd, no git, no file scanning) | Layer 2a adds cwd + workspace facts; Layer 2b adds git facts. |
| `common/` | `CircuitBreaker`, `TokenCount`, `uuid_id!`/`string_id!`, `Extensions` (TypeMap), generic indices | Foundational utilities. |
| `auth/` | `Credential`, `CredentialProvider`, OAuth refresh | Generic; `ClaudeCliProvider` lives behind `cli-auth` feature and is Layer 2b. |
| `mcp/` | MCP client, `McpManager`, lifecycle phases (post task #45) | Feature `mcp`. The protocol itself is domain-neutral. |
| `models/` | `ModelCatalog`, pricing, context-window specs | Generic model metadata. |
| `events/` | Internal event bus | Generic. |
| `tokens/` | Token counter trait + per-provider implementations, reconciliation (task #44) | Generic. |
| `skills/` | `SkillCatalog`, `SkillRuntime` — generic metadata + progressive disclosure | Core: catalog is domain-neutral. Upper layers can register skills. |
| `subagents/` | `SubagentRegistry`, `SubagentConfig` — generic delegation | Core: no builtin list. `explore`/`plan`/`bash` move to upper layers. |
| `orchestration/` | Subagent execution, delegation policies, FSM (task #27, 1.1) | Generic. |
| `output_style/` | `OutputStyle`, `SystemPromptGenerator` | Generic. |
| `config/` | Configuration loader, `ConfigError` | Generic. |
| `security/sandbox/network.rs` | `NetworkSandbox` — HTTP egress allow/deny list | Verified decoupled (V5 audit). Stays in core for general web agents. |

**Excluded from Layer 1 (moved in Phase 1):**
- `security/fs/`, `security/path/`, `security/sandbox/{landlock,macos,config,mod}.rs`, `security/policy/`, `security/guard.rs`, `security/error.rs` → Layer 2a.
- `security/bash/`, `security/limits/` → Layer 2b.
- `tools/{read,write,edit,glob,grep}.rs` → Layer 2a.
- `tools/{bash,kill,process}.rs` → Layer 2b.
- `context/memory_loader.rs` CLAUDE.md discovery → Layer 2b. Generic markdown loader → Layer 2a.
- `prompts/environment.rs` git detection → Layer 2b.
- `subagents/builtin.rs` explore/plan → Layer 2a. bash → Layer 2b.

### 2.2 Layer 2a — Local Filesystem Tools (feature `local-fs`)

**Goal**: Enable any local-machine agent — research, knowledge management, data analysis, reporting, digital-garden, documentation — to safely read, search, and write files under a workspace root. **No shell execution. No bash AST. No git. No CLAUDE.md convention.** A strictly smaller surface and attack profile than a coding agent.

**Why separate from `coding-tools`**:
- Read/Write/Edit/Glob/Grep have zero dependency on `tree-sitter` or `BashAnalyzer` (verified grep).
- File access and shell execution are **categorically different security postures**. A user enabling `local-fs` has not consented to arbitrary command execution.
- This matches claw-code's de facto pattern (their `explore` subagent is file-exploration with Bash; our `explore` uses only Read/Grep/Glob and is richer for it).
- Dependency savings: `local-fs` avoids pulling in `tree-sitter` (~1 MB), `tree-sitter-bash`, and process-scheduling machinery.

**Module root**: `src/local_fs/` (new consolidated module; also used by Layer 2b).

**Submodules (`#[cfg(feature = "local-fs")]`)**:
| Path | Content | Migrated from |
|---|---|---|
| `local_fs/workspace.rs` | `Workspace` struct (root PathBuf, metadata), `WorkspaceExtension` for ExecutionContext | NEW |
| `local_fs/fs/` | `SecureFs` (TOCTOU-safe `openat` + `O_NOFOLLOW`), `SafePath`, path resolution | `security/fs/`, `security/path/` |
| `local_fs/sandbox/` | `Sandbox` assembly, `SandboxConfig`, `SandboxStatus`, Landlock (Linux), Seatbelt (macOS) — **path-based FS restrictions only** | `security/sandbox/{landlock,macos,config,mod,error}.rs` |
| `local_fs/policy/` | `SecurityPolicy`, `PermissionDecision`, per-tool limits (FS tools) | `security/policy/`, `security/guard.rs` |
| `local_fs/context.rs` | `LocalFsExtension` — carries Workspace, SecureFs, FS Sandbox, SecurityPolicy | Extracted from `security/mod.rs::SecurityContext` |
| `local_fs/tools/read.rs` | `Read` tool (reads any text/markdown/pdf with budget) | `tools/read.rs` |
| `local_fs/tools/write.rs` | `Write` tool | `tools/write.rs` |
| `local_fs/tools/edit.rs` | `Edit` tool (exact string replace + structured patch) | `tools/edit.rs` |
| `local_fs/tools/glob.rs` | `Glob` tool (file pattern matching) | `tools/glob.rs` |
| `local_fs/tools/grep.rs` | `Grep` tool (regex content search, ripgrep) | `tools/grep.rs` |
| `local_fs/subagents_builtin.rs` | `explore_subagent` (Read+Grep+Glob+TodoWrite), `plan_subagent` (Read+Grep+Glob+TodoWrite) — **no Bash** | `subagents/builtin.rs` (minus bash) |
| `local_fs/memory/markdown_loader.rs` | Generic markdown file scanner — find `.md` files under a directory, budget-cap, dedup by content hash | NEW (generalized from claw-code CLAUDE.md loader) |
| `local_fs/environment.rs` | Workspace environment facts (cwd, file count summary) — no git, no CLAUDE.md | NEW |
| `local_fs/prelude.rs` | Re-exports for `use branchforge::local_fs::prelude::*` | NEW |

**Dependencies**:
- `rustix = { version = "1.1", features = ["fs", "process"], optional = true }` — `local-fs` feature enables this (currently unconditional).
- `glob`, `regex` — already unconditional; used by `Glob`/`Grep` tools.

**Which builtin subagents live here**:
- `explore` — Read+Grep+Glob+TodoWrite. Purpose: discover information in a filesystem tree. Useful for **any** local agent.
- `plan` — Read+Grep+Glob+TodoWrite. Purpose: analyse files and produce a task list.
- `general_purpose` — uses whatever tools are registered. Lives in Layer 1 so it works with any feature combination.

### 2.3 Layer 2b — Coding Agent Extension (feature `coding-tools`)

**Goal**: Enable a Claude Code-class coding agent. Builds on top of Layer 2a by adding shell execution with production-grade safety (bash AST analysis, layered command validation, namespace sandboxing), process resource limits, git awareness, and opinionated Claude Code conventions (CLAUDE.md, `.claude/` directory layout).

**Feature declaration**: `coding-tools = ["local-fs", "tree-sitter", "tree-sitter-bash"]`.
**Transitive dependency**: enabling `coding-tools` automatically enables `local-fs`.

**Module root**: `src/coding/` (new).

**Submodules (`#[cfg(feature = "coding-tools")]`)**:
| Path | Content | Migrated from |
|---|---|---|
| `coding/bash/` | `BashAnalyzer` (tree-sitter parse) + layered validation pipeline (claw-code port: `classify_command`, `validate_read_only`, `check_destructive`, `validate_mode`, `validate_sed`, `validate_paths`, `extract_first_command`) | `security/bash/` + task #17 (S1) |
| `coding/sandbox/unshare.rs` | Linux unshare-based command sandbox (namespace isolation for spawned shells) | NEW — task #18 (S2) |
| `coding/limits/` | `ResourceLimits` (setrlimit: CPU time, memory, open files, file size, processes) | `security/limits/` |
| `coding/context.rs` | `CodingExtension` — carries BashAnalyzer, ResourceLimits, unshare config | Extracted from `security/mod.rs::SecurityContext` |
| `coding/tools/bash.rs` | `Bash` tool (uses `coding::bash::BashAnalyzer` for validation, `coding::sandbox::unshare` for isolation) | `tools/bash.rs` |
| `coding/tools/kill_shell.rs` | `KillShell` tool | `tools/kill.rs` |
| `coding/tools/process.rs` | `ProcessScheduler` — background process management | `tools/process.rs` |
| `coding/instruction_files.rs` | Opinionated `CLAUDE.md` / `CLAUDE.local.md` / `.claude/rules/*.md` discovery, content-hash dedup, budget cap | `context/memory_loader.rs` `find_claude_files` |
| `coding/git_context.rs` | `is_git_repository`, `git_status`, `git_diff`, `git_recent_commits`; `GitEnvironmentSource` implements `EnvironmentSource` | `prompts/environment.rs` git bits |
| `coding/subagents_builtin.rs` | `bash_subagent` (Bash only) | `subagents/builtin.rs` (bash half) |
| `coding/environment.rs` | Coding-specific environment facts on top of Layer 2a workspace facts | NEW |
| `coding/prelude.rs` | Re-exports for `use branchforge::coding::prelude::*` | NEW |

**Companion features** (Claude Code ecosystem interop):
- `cli-auth` (`ClaudeCliProvider`, `.claude/` credential layout) — opt-in separately for users of the Claude CLI auth flow. Not required by `coding-tools`.
- `file-resources` (`DocumentLoader`, `FileProvider`, skill/subagent file loaders) — file-backed resources. Depends on `local-fs` (trait impls reside there) and is pulled in by `coding-tools` by default.
- `multimedia` (PDF/image support for the Read tool) — depends on `local-fs`; opt-in.

### 2.4 Layer 3 — Cloud Providers (feature-gated)

Already well-isolated. No changes needed beyond what Phase 4 (Dialect parameterization, task #30) adds.

| Feature | Scope |
|---|---|
| `aws` | Bedrock transport + SigV4 signing |
| `gcp` | Vertex transport + ADC |
| `azure` | Foundry transport + Entra ID |
| `cloud-all` | All of the above |

**Observation**: `openai`/`gemini` feature flags currently gate nothing — `openai_chat`/`openai_responses`/`gemini_generate` codecs live in `src/client/codec/` at Layer 1 and compile unconditionally. After Phase 4, OpenAI-compat dialect parameterization (Grok/Groq/Mistral/Together/…) remains in Layer 1 codec; feature flags stay as no-ops for backwards signaling or are removed entirely.

## 3. Dependency Rules

These are **enforced by feature-gating and module structure**, not merely documented:

1. **Layer 1 has zero `#[cfg(feature = "local-fs")]` or `#[cfg(feature = "coding-tools")]` imports**. A pure-core build (`cargo build --no-default-features --features "anthropic-direct"`) must succeed with every Layer 1 symbol accessible. Verified by `tests/pure_core_compile.rs` (task #36).
2. **Layer 2a (`local-fs`) may import any Layer 1 symbol**, but never Layer 2b (`coding-tools`) or Layer 3.
3. **Layer 2b (`coding-tools`) may import any Layer 1 or Layer 2a symbol**, but never Layer 3. `coding-tools` declares `local-fs` as a transitive dependency in `Cargo.toml`.
4. **Layer 3 (cloud providers) may import any Layer 1 symbol**, but not Layer 2a/2b (no cloud provider depends on filesystem or coding tools).
5. **Layer 1 traits are extension points**. `Tool`, `MemoryLoader`, `EnvironmentSource`, `Hook`, `CredentialProvider`, `ModelCodec`, `ModelTransport` — each is a stable trait in Layer 1; upper layers provide concrete implementations behind feature gates. Users can provide their own implementations at any layer via the Extensions TypeMap (§4).
6. **Testing matrix** (`V1`, task #36) must include at least:
   - `--no-default-features --features "anthropic-direct"` — pure core only
   - `--no-default-features --features "anthropic-direct local-fs"` — local general agent
   - `--no-default-features --features "anthropic-direct coding-tools"` — full coding agent
   - `--all-features` — everything

## 4. ExecutionContext: Extensions TypeMap

**The central architectural decision that enables the 3-layer split.**

### Problem
Current `ExecutionContext` (`tools/context.rs:49-57`) hard-owns `Arc<SecurityContext>`, which itself hard-owns `SecureFs`, `Sandbox`, `ResourceLimits`, `NetworkSandbox`, `SecurityPolicy`. A general-purpose Tool cannot construct `ExecutionContext` without pulling in all of coding-tools.

### Decision
**Adopt the `http::Extensions` TypeMap pattern** (well-established in the Rust ecosystem: `http`, `axum::Extension`, `reqwest` middleware). Extensions is a heterogeneous type-keyed container — each extension is stored under its own type as the key.

```rust
// In src/tools/context.rs (Layer 1)
pub struct ExecutionContext {
    session: Option<Arc<Session>>,
    progress: ProgressSender,
    cancel: Option<CancellationToken>,
    hooks: Arc<HookManager>,
    extensions: Extensions, // type-keyed storage
}

impl ExecutionContext {
    pub fn extensions(&self) -> &Extensions { &self.extensions }
    pub fn extensions_mut(&mut self) -> &mut Extensions { &mut self.extensions }
    // ... session/progress/cancel/hooks accessors
}
```

**Implementation decision** (task #41, completed): we wrote our own ~300-LOC `Extensions` type in `src/common/extensions.rs` rather than taking a dependency on `http::Extensions`. Rationale: `http` crate is currently optional (only behind `mcp` feature); making it unconditional would add 40 KB to pure core for a feature we can implement in 80 lines of core logic with our own control over the API. The design follows the same `TypeId`-keyed pattern as `http::Extensions`, `axum::Extension`, `tower` middleware, and `reqwest` extensions.

### Extension types per layer

Extensions are defined by the layer that owns the concern. Each layer inserts its own extensions when relevant tools are registered; downstream tools read them back via type-keyed lookup.

**Layer 2a (`local-fs`)** provides:
```rust
// src/local_fs/workspace.rs
pub struct WorkspaceExtension {
    pub root: Arc<Path>,
    // workspace metadata (read from disk once at builder time)
}

// src/local_fs/context.rs
pub struct LocalFsExtension {
    pub fs: SecureFs,                    // TOCTOU-safe file operations
    pub sandbox: Arc<Sandbox>,           // Landlock / Seatbelt path rules
    pub policy: SecurityPolicy,          // per-tool limits for file tools
}
```

**Layer 2b (`coding-tools`)** provides additionally:
```rust
// src/coding/context.rs
pub struct CodingExtension {
    pub bash: BashAnalyzer,              // tree-sitter bash parse
    pub limits: ResourceLimits,          // setrlimit: CPU/mem/procs
    pub command_sandbox: CommandSandbox, // unshare wrapper (Linux)
}

// src/coding/git_context.rs
pub struct GitExtension {
    pub repo_root: PathBuf,
    // cached git state (status, recent commits, diff)
}
```

**Layer 1** has no extensions of its own — it only ships the container.

### Tools consume extensions type-safely

```rust
// src/local_fs/tools/read.rs (Layer 2a)
#[async_trait]
impl SchemaTool for ReadTool {
    async fn execute(&self, input: Self::Input, ctx: &ExecutionContext) -> ToolResult {
        // ReadTool is only registered when `local-fs` is enabled, and the
        // builder inserts LocalFsExtension + WorkspaceExtension as a pair.
        // The registration boundary enforces this invariant.
        let fs = ctx.extensions().get::<LocalFsExtension>()
            .expect("LocalFsExtension present whenever ReadTool is registered");
        let workspace = ctx.extensions().get::<WorkspaceExtension>()
            .expect("WorkspaceExtension present whenever ReadTool is registered");
        // ... resolve + read
    }
}

// src/coding/tools/bash.rs (Layer 2b)
#[async_trait]
impl SchemaTool for BashTool {
    async fn execute(&self, input: Self::Input, ctx: &ExecutionContext) -> ToolResult {
        // BashTool requires BOTH local-fs extensions (for workspace boundary)
        // AND coding extensions (for bash analysis + rlimits). Because
        // `coding-tools` transitively enables `local-fs`, and the builder
        // inserts all three extensions together, all three lookups succeed.
        let fs = ctx.extensions().get::<LocalFsExtension>().unwrap();
        let workspace = ctx.extensions().get::<WorkspaceExtension>().unwrap();
        let coding = ctx.extensions().get::<CodingExtension>().unwrap();
        // ... analyze + validate + sandbox + execute
    }
}
```

The `unwrap`/`expect` calls are sound because **the invariant is enforced at the registration boundary**, not at each tool call. A builder helper is the only way to register these tools, and it always inserts the matching extensions. Users cannot accidentally register `BashTool` without `CodingExtension`.

### AgentBuilder ergonomics

```rust
// Pure core (Layer 1) — no filesystem, no workspace, no shell
let agent = Agent::builder()
    .model("claude-sonnet-4-5")
    .with_tool(my_custom_api_tool)  // user-provided Tool impl
    .build().await?;

// Local general agent (Layer 2a) — Read/Write/Edit/Glob/Grep
#[cfg(feature = "local-fs")]
let agent = Agent::builder()
    .model("claude-sonnet-4-5")
    .with_workspace("./notes")            // WorkspaceExtension
    .with_local_fs_tools()                // LocalFsExtension + Read/Write/Edit/Glob/Grep
    .build().await?;

// Coding agent (Layer 2b) — full coding surface
#[cfg(feature = "coding-tools")]
let agent = Agent::builder()
    .model("claude-sonnet-4-5")
    .with_workspace("./project")
    .with_local_fs_tools()                // local-fs layer
    .with_coding_tools()                  // CodingExtension + Bash/KillShell
    .with_git_context()                   // GitExtension
    .with_instruction_files()             // CLAUDE.md loader
    .build().await?;

// Or the convenience single-shot helper that composes local-fs + coding-tools:
#[cfg(feature = "coding-tools")]
let agent = Agent::builder()
    .model("claude-sonnet-4-5")
    .as_coding_agent("./project")         // workspace + local-fs + coding + git + instruction_files
    .build().await?;

// User-provided extension (works at any layer)
let agent = Agent::builder()
    .model("claude-sonnet-4-5")
    .with_extension(TelemetryExtension::new(otel_tracer))
    .with_extension(TenantExtension { tenant_id: "acme".into() })
    .build().await?;
```

### Benefits
- **Type safety**: Each extension has its own type key; no stringly-typed lookups.
- **Zero-cost for absent features**: Extensions not inserted do not exist in memory.
- **User-extensible**: Third-party crates can define their own extensions (`TelemetryExtension`, `AuditExtension`, `MultiTenantExtension`) without upstream changes.
- **Follows Rust ecosystem conventions**: `http::Extensions`, `axum::Extension`, `tower` middleware all use this pattern.
- **No dynamic dispatch overhead** beyond the one `HashMap` lookup.

### Trade-off
- Runtime lookup instead of compile-time proof. Mitigated by the registration-boundary invariant: extensions are inserted exactly when the tools that need them are registered, so a correctly-constructed agent never panics.

## 5. AgentConfig changes

Current:
```rust
pub struct AgentConfig {
    pub model: String,
    pub working_dir: Option<PathBuf>,  // ❌ Layer 2a concept in Layer 1
    pub security: SecurityConfig,      // ❌ Layer 2a+b concept in Layer 1
    pub budget: BudgetConfig,
    pub execution: ExecutionConfig,
    pub prompt: PromptConfig,
    pub cache: CacheConfig,
    pub model_config: AgentModelConfig,
}
```

Target:
```rust
// Layer 1
pub struct AgentConfig {
    pub model: String,
    pub budget: BudgetConfig,
    pub execution: ExecutionConfig,
    pub prompt: PromptConfig,      // generic — no workspace, no git, no CLAUDE.md
    pub cache: CacheConfig,
    pub model_config: AgentModelConfig,
    pub extensions: Extensions,    // ← carries Layer 2a/2b/user config
}

// Layer 2a — inserted by AgentBuilder::with_local_fs_tools / with_workspace
#[cfg(feature = "local-fs")]
pub struct LocalFsConfig {
    pub workspace: Workspace,       // root PathBuf
    pub fs_policy: FsPolicy,        // SecureFs + Landlock/Seatbelt paths
    pub tool_limits: FileToolLimits,
}

// Layer 2b — inserted by AgentBuilder::with_coding_tools
#[cfg(feature = "coding-tools")]
pub struct CodingConfig {
    pub bash_policy: BashPolicy,                  // validation rules, command allowlist
    pub resource_limits: ResourceLimits,          // setrlimit for child processes
    pub command_sandbox: CommandSandboxConfig,    // unshare config (Linux)
    pub git: Option<GitConfig>,                   // optional git context
    pub instruction_files: Option<InstructionFilesConfig>,  // CLAUDE.md discovery
}
```

Both configs carry into `AgentConfig::extensions` via the Extensions pattern, preserving Layer 1 purity. The builder helpers (`with_local_fs_tools`, `with_coding_tools`, `as_coding_agent`) wrap the insertion.

## 6. Prelude scoping

```rust
// src/prelude.rs (Layer 1 — unconditional)
pub use crate::{Agent, AgentBuilder, AgentEvent, AgentResult, Error, Result};
pub use crate::client::{ModelCodec, ModelTransport, Preset, ProviderClient, LlmCall};
pub use crate::ir::{ModelRequest, ModelResponse, ContentPart, Message, Role, Usage, ModelSettings, ProviderOptions, SystemPrompt, ToolDefinition};
pub use crate::tools::{Tool, SchemaTool, ToolRegistry, ExecutionContext, ToolSurface};
pub use crate::session::{Session, SessionId, SessionManager};
pub use crate::hooks::{Hook, HookManager, HookEvent};
pub use crate::authorization::{ToolPolicy, ExecutionMode};
pub use crate::graph::{SessionGraph, NodeId, BranchId, Branch, Checkpoint};
pub use crate::common::Extensions;

// Local-fs layer (2a): workspace + filesystem tools for non-coding local agents.
#[cfg(feature = "local-fs")]
pub use crate::local_fs::prelude::*;
// -> Workspace, WorkspaceExtension, LocalFsExtension, ReadTool, WriteTool,
//    EditTool, GlobTool, GrepTool, explore_subagent, plan_subagent,
//    MarkdownMemoryLoader

// Coding layer (2b): adds bash, process scheduling, git, CLAUDE.md.
#[cfg(feature = "coding-tools")]
pub use crate::coding::prelude::*;
// -> CodingExtension, BashTool, KillShellTool, GitExtension,
//    InstructionFilesLoader, bash_subagent

#[cfg(feature = "mcp")]
pub use crate::mcp::{McpManager, McpServer};
```

Users of `use branchforge::prelude::*;` get exactly the surface matching their enabled features. Three valid prelude states exist:
1. **Pure core**: no FS tools, no workspace, no shell.
2. **Core + local-fs**: file tools, workspace, path sandbox. No shell.
3. **Core + coding-tools**: everything in (2) plus bash, process scheduler, git, CLAUDE.md.

## 7. Default feature reconsidered

Current: `default = ["coding-tools"]` — contradicts general-purpose SDK claim and forces coding-specific dependencies on every consumer.

Target: `default = ["anthropic-direct"]` — a minimal working slice. `anthropic-direct` is a meta-feature that pulls in nothing extra (Anthropic Messages codec and Direct transport are already in Layer 1), but signals intent and gives `cargo add branchforge` users a usable default.

```toml
[features]
default = ["anthropic-direct"]

# Meta-feature: signals Anthropic direct API readiness. No extra deps.
anthropic-direct = []

# Local filesystem tools — safe file operations for local-machine agents.
# Depends on `rustix` for TOCTOU-safe file ops and Landlock/Seatbelt sandbox.
local-fs = ["dep:rustix"]

# Full coding agent extension — depends on local-fs for filesystem primitives,
# adds tree-sitter for bash AST analysis.
coding-tools = ["local-fs", "dep:tree-sitter", "dep:tree-sitter-bash"]

# Claude Code ecosystem interop
cli-auth = []                          # ClaudeCliProvider, `.claude/` credential layout
file-resources = ["local-fs"]          # DocumentLoader, FileProvider, skill/subagent file loaders
multimedia = ["local-fs", "dep:pdf-extract"]  # PDF/image support for Read tool

# Transport/auth adapters
aws = ["dep:aws-config", "dep:aws-credential-types", "dep:aws-sigv4", "dep:aws-smithy-runtime-api"]
gcp = ["dep:gcp_auth"]
azure = ["dep:azure_identity", "dep:azure_core"]
cloud-all = ["aws", "gcp", "azure"]

# Persistence, observability, plugins, scheduling — unchanged
jsonl = []
postgres = ["dep:sqlx"]
redis-backend = ["dep:redis"]
persistence-all = ["jsonl", "postgres", "redis-backend"]
otel = ["dep:opentelemetry", "dep:opentelemetry_sdk", "dep:opentelemetry-otlp", "dep:opentelemetry-semantic-conventions", "dep:tracing-opentelemetry", "dep:tracing-subscriber"]
mcp = ["dep:rmcp", "dep:http"]
plugins = ["file-resources"]
scheduling = ["dep:cron"]

# Full everything
full = ["coding-tools", "cloud-all", "persistence-all", "otel", "plugins", "cli-auth", "mcp", "scheduling"]
```

**Dependency migration**:
- `rustix` moves from unconditional to `local-fs` feature.
- `tree-sitter` / `tree-sitter-bash` stay under `coding-tools` (already correct).
- `pdf-extract` stays under `multimedia` (already correct).

**Usage examples**:
```toml
# Pure API agent
branchforge = "1.0"

# Local research / knowledge / data agent
branchforge = { version = "1.0", features = ["local-fs"] }

# Coding agent
branchforge = { version = "1.0", features = ["coding-tools"] }

# Coding agent on AWS Bedrock with persistent session state and OTEL
branchforge = { version = "1.0", features = ["coding-tools", "aws", "postgres", "otel"] }
```

This is **intentional friction**: each opt-in corresponds to a conscious choice about capabilities, dependencies, and attack surface.

## 8. Error taxonomy: current vs target

### Current (`src/lib.rs:347-362`)
```rust
pub enum ErrorCategory {
    Authorization, Configuration, Transient, Stateful, Internal, ResourceLimit,
}
```
Only 6 variants. Too coarse for OTEL `error.category` vocabulary, does not distinguish hook failures from MCP failures from content policy blocks.

### Target (NEW-6, Phase 2)
Replace with `FailureCategory`:
```rust
pub enum FailureCategory {
    Auth,               // Authentication | Provider{Auth}
    Transport,          // Network | Provider{Network}
    RateLimit,          // RateLimit | Provider{RateLimit}
    Quota,              // Provider{Quota}
    ContextWindow,      // ContextWindowExceeded | Provider{PayloadTooLarge}
    ContentPolicy,      // Provider{ContentFilter}
    SchemaMismatch,     // NEW-3 response validation
    ToolRuntime,        // Tool
    HookFailure,        // HookFailed | HookTimeout
    McpHandshake,       // Mcp{handshake phase}
    McpInvocation,      // Mcp{invocation phase}
    Persistence,        // Session
    Config,             // Config | Parse | Env | InvalidRequest | InvalidComposition
    Cancelled,          // Provider{Cancelled}
    Budget,             // BudgetExceeded
    Resource,           // Timeout | ResourceExhausted
    CircuitOpen,        // CircuitOpen
    Internal,           // Io | Json | NotSupported
}
```

`Error::category(&self) -> FailureCategory` is the single mapping function. `ErrorCategory` is deleted. OTEL spans carry `error.category = {variant_name}`.

**Guarantee**: exhaustive match — new `Error` variants must update `category()` or the compiler rejects.

## 9. Phase 1 execution order

The following order minimizes churn and compile failures during migration. Each step leaves the workspace in a compiling state with all tests passing.

1. **Task #41 (NEW-4) — DONE.** Introduce `Extensions` on `ExecutionContext`. Non-breaking addition; existing `SecurityContext` field kept alongside until step 6.
2. **Task #2 (C1)** — Remove `working_dir` from `AgentConfig`. Move to `WorkspaceExtension` in `src/local_fs/workspace.rs`. Update `Agent::new`/`AgentBuilder` to accept workspace only via `with_workspace()` feature-gated helper. `working_dir` mentions in `lib.rs` doctests move to coding example block.
3. **Task #5 (C4)** — Create new `src/local_fs/` and `src/coding/` module roots. Feature-gate `rustix` behind `local-fs`.
   - Move `security/fs/`, `security/path/`, `security/policy/`, `security/guard.rs`, `security/error.rs`, `security/sandbox/{landlock,macos,config,mod}.rs` → `src/local_fs/` (`#[cfg(feature = "local-fs")]`).
   - Move `security/bash/`, `security/limits/` → `src/coding/` (`#[cfg(feature = "coding-tools")]`).
   - `security/sandbox/network.rs` stays where it is and is re-exported as `src/network/` or stays under `src/security/network/` at Layer 1.
   - Old `src/security/mod.rs` is deleted; `SecurityContext` is split into `LocalFsExtension` + `CodingExtension`.
4. **Task #3 (C2)** — Remove `security: Arc<SecurityContext>` field from `ExecutionContext`. All `ctx.fs.*`, `ctx.sandbox.*`, `ctx.limits.*`, `ctx.bash.*`, `ctx.policy.*` helper methods are deleted. Tools read via `ctx.extensions().get::<LocalFsExtension>()` / `get::<CodingExtension>()` at the call site. `NetworkSandbox` stays accessible via a core method `ctx.network()` (it is not an extension — it is universally available).
5. **Task #4 (C3)** — Redefine `ToolSurface`:
   - `ToolSurface::core()` → `["Skill", "Plan", "TodoWrite", "GraphHistory"]`
   - `ToolSurface::local_fs()` → core + `["Read", "Write", "Edit", "Glob", "Grep"]` (feature `local-fs`)
   - `ToolSurface::coding()` → local_fs + `["Bash", "KillShell"]` (feature `coding-tools`)
   - `ToolSurface::all()` → composes based on active features
6. **Task #6 (C5)** — Create `src/local_fs/memory/markdown_loader.rs` (generic markdown scanner) and `src/coding/instruction_files.rs` (opinionated CLAUDE.md/.claude/ discovery). `MemoryLoader` trait stays in Layer 1.
7. **Task #7 (C6)** — Move `prompts/environment.rs::is_git_repository` and surrounding git detection to `src/coding/git_context.rs`. Introduce `EnvironmentSource` trait in `src/context/` (Layer 1). Layer 1 `prompts/environment.rs` emits only model/date/platform. Layer 2a adds cwd + workspace facts via `WorkspaceEnvironmentSource`. Layer 2b adds git facts via `GitEnvironmentSource`.
8. **Task #8 (C7)** — Split `subagents/builtin.rs`:
   - `general_purpose_subagent` stays in core (Layer 1) — composes from whatever tools are registered.
   - `explore_subagent`, `plan_subagent` move to `src/local_fs/subagents_builtin.rs` (feature `local-fs`) and **use only Read/Grep/Glob/TodoWrite** (no Bash).
   - `bash_subagent` moves to `src/coding/subagents_builtin.rs` (feature `coding-tools`).
9. **Task #42 (NEW-5)** — Split `src/prelude.rs` into conditional sections per the block in §6. Add `src/local_fs/prelude.rs` and `src/coding/prelude.rs`.
10. **Task #9 (C8)** — Change `default = ["coding-tools"]` to `default = ["anthropic-direct"]`. Add `anthropic-direct` meta-feature. Update `rustix` to be optional under `local-fs`.
11. **Task #10 (C9)** — Add range of range-of-scope examples:
    - `examples/research_agent.rs` — requires `local-fs` only. Reads markdown notes, writes report.
    - `examples/customer_support_agent.rs` — requires default only (pure core). Custom KB tool.
    - `examples/data_analysis_agent.rs` — requires `local-fs` only. Reads CSV/JSON, produces summary.
    - `examples/multi_provider_router.rs` — requires default only. Shows 5 codec routing.

Each task is independently committable and leaves the workspace in a compiling state.

## 10. Verification contract (task #36, Phase 6)

Phase 6 ships a four-configuration test matrix enforced in CI:

```bash
# 1. Pure core — agent runtime, IR, client, sessions, generic tools
cargo build --no-default-features --features "anthropic-direct"
cargo test  --no-default-features --features "anthropic-direct" --test pure_core_*

# 2. Local general agent — adds filesystem tools
cargo build --no-default-features --features "anthropic-direct local-fs"
cargo test  --no-default-features --features "anthropic-direct local-fs" --test local_fs_*

# 3. Coding agent — adds bash, git, CLAUDE.md conventions
cargo build --no-default-features --features "anthropic-direct coding-tools"
cargo test  --no-default-features --features "anthropic-direct coding-tools" --test coding_*

# 4. Everything on
cargo build --all-features
cargo test  --all-features
```

`tests/pure_core_compile.rs` (task #36) is a guardrail:

```rust
// Must compile with:
//   cargo test --test pure_core_compile --no-default-features --features "anthropic-direct"
use branchforge::prelude::*;

#[test]
fn pure_core_has_no_filesystem_or_coding_symbols() {
    // These names must NOT be in scope. A leak causes compile failure:
    //   let _: branchforge::local_fs::Workspace = todo!();
    //   let _: branchforge::coding::BashTool = todo!();
    //   let _: branchforge::common::Extensions = todo!();  // ← actually OK, Extensions is core
    //   let _ = ReadTool;   // ← fail: ReadTool is Layer 2a
    //   let _ = BashTool;   // ← fail: BashTool is Layer 2b

    // These MUST be in scope at pure core:
    let _agent_builder = Agent::builder();
    let _: Option<Box<dyn Tool>> = None;
    let _ext = Extensions::new();
}
```

A complementary `tests/local_fs_compile.rs` adds `local-fs` feature and asserts that `ReadTool` etc. are in scope while `BashTool` still is not. A `tests/coding_compile.rs` does the symmetric check for `coding-tools`.

## 11. Invariants summary

These must hold at all times during and after migration. Phase 1 must not break any; Phase 2+ builds on them.

1. `cargo build --no-default-features --features "anthropic-direct"` succeeds.
2. `cargo build --no-default-features --features "anthropic-direct local-fs"` succeeds.
3. `cargo build --no-default-features --features "anthropic-direct coding-tools"` succeeds.
4. `cargo build --all-features` succeeds.
5. No Layer 1 file imports from `src/local_fs/` or `src/coding/`.
6. No Layer 2a (`src/local_fs/`) file imports from `src/coding/`.
7. No Layer 3 (cloud provider) file imports from `src/local_fs/` or `src/coding/`.
8. No Layer 1 file hardcodes the strings `"CLAUDE.md"`, `".claude"`, or `"working_dir"` (as a field name). `"git"` may appear only in Layer 2b.
9. `ExecutionContext` has no field whose type is gated behind `local-fs` or `coding-tools`. Feature-specific state lives in `Extensions` only.
10. `AgentConfig` has no field whose type is gated behind `local-fs` or `coding-tools`. Feature-specific config lives in `Extensions` only.
11. `src/prelude.rs` re-exports no symbol gated behind `local-fs` or `coding-tools` outside a `#[cfg(feature = ...)]` guard.
12. `default` feature does not include `local-fs` or `coding-tools`.
13. `rustix`, `tree-sitter`, and `tree-sitter-bash` are all declared `optional = true` in Cargo.toml and pulled in only by `local-fs` or `coding-tools`.
14. `tests/pure_core_compile.rs`, `tests/local_fs_compile.rs`, and `tests/coding_compile.rs` all pass on every PR (blocking CI gate).
10. `tests/pure_core_compile.rs` passes on every PR.
