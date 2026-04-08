# Changelog

All notable changes to branchforge are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] — Phase 1b complete

This release closes the Phase 1b refactor that moves the agent runtime to a
provider-neutral IR (`src/ir/`), enforces `SessionGraph` as the single source
of truth via the type system, and removes the entire legacy
`client/adapter/**` monolithic Client surface in favour of a 3-axis
`Codec × Transport × EndpointShape` design.

### Highlights

- **Provider stack rebuilt** as `ProviderClient = Arc<dyn ModelCodec> + Arc<dyn ModelTransport>` with composition validation at construction time. 5 codecs (`anthropic-messages`, `openai-chat`, `openai-responses`, `gemini-generate`, `bedrock-converse`) × 4 transports (`direct`, `vertex`, `bedrock`, `foundry`) yield free combinations like Vertex-Gemini and Vertex-Anthropic without writing new code.
- **Capability honesty enforced by tests**: every codec's `ProviderCapabilities.structured_output.json_schema` declaration is now matched against `encode_request` behaviour by `tests/codec_contract.rs::capability_honesty_response_format`. Codecs that declare `Native` must emit a wire-level schema; codecs that declare `Emulated` must emit a `ModelWarning::CapabilityEmulated`. Capability lying is now a compile/test failure.
- **`SessionGraph` is the compiler-enforced source of truth**. The previous `Session.messages` field has been deleted; messages are always derived from the graph via `current_branch_messages()`. 14 of the 19 `Session` fields are now `pub(crate)` with read getters, leaving only immutable identity fields (`id`, `parent_id`, `session_type`, `config`, `authorization`) public.
- **Cancellation propagates end-to-end**: `runtime.shutdown.child_token()` is wired through both the streaming and non-streaming execution paths to `ToolRegistry::execute_with_cancel`. Graceful shutdown aborts in-flight tools instead of waiting for natural completion.
- **OAuth credential refresh** is plugged into `DirectTransport` via an optional `Arc<dyn CredentialProvider>` and the `ModelTransport::refresh()` trait method.

### Added

- **`ir::ResponseFormat`** field on `ModelRequest` (Text / JsonObject / JsonSchema). Implemented natively by OpenAI Responses (`text.format`), OpenAI Chat (`response_format`), and Gemini (`responseMimeType` + `responseSchema`); emitted as `CapabilityEmulated` warning by Anthropic and Bedrock.
- **`ir::ContentPart::ToolResult.tool_name: Option<String>`** so Gemini's `functionResponse.name` round-trips losslessly.
- **`ModelTransport::classify_error(status, body)`** trait method. Each transport now owns its vendor-specific HTTP failure patterns (Vertex quota project, Bedrock throttling, Foundry Entra). The central `provider_client::classify_response_error` only delegates — adding a new transport never requires editing it.
- **`DirectTransport::with_credential_provider(...)`** + `refresh()` override that swaps the cached `DirectAuth` after a 401.
- **Bedrock Converse `cachePoint` translation** — `apply_cache_points()` translates `provider_options.anthropic.cache_control` flags into inline `cachePoint` blocks at the system / tools / conversation positions.
- **`From<&ToolSpec>` and `From<ToolSpec>`** for `ir::ToolDefinition` (borrowed and owned variants).
- **`OrchestrationBundle`** (`coordination` + `agent_directory` + `orchestrator`) groups the multi-agent fields out of `AgentRuntime` so the responsibility boundary is explicit.
- **Pricing table for current models** — Claude Opus 4 / 4.6, Sonnet 4 / 4.5 / 4.6, Haiku 4.5; GPT-5 / 5-mini / 5-nano, o4-mini, o3, o3-mini, GPT-4o, GPT-4o-mini; Gemini 2.5 Pro / Flash / Flash-Lite, Gemini 2.0 Flash / Flash-Lite. Plus a generic `BRANCHFORGE_PRICING_<MODEL>_INPUT` env-var override.
- **Integration test suites** under `tests/`:
  - `cancellation_test.rs` — graceful shutdown aborts in-flight tools (3 tests)
  - `oauth_refresh_test.rs` — `DirectTransport::refresh()` swaps credentials end-to-end (3 tests)
  - `compaction_ssot_test.rs` — `SessionGraph` remains source of truth across compaction (5 tests)
  - `persistence_parity_test.rs` — `MemoryPersistence` and `JsonlPersistence` agree on save/load/list/delete/queue scenarios (2 tests)
  - `tests/codec_contract.rs::capability_honesty_response_format` — every codec's `response_format` declaration matches its emit behaviour (5 tests)
- **38+ new lib regression tests** for `tool_name` round-trip, Bedrock cachePoint partial-flag combinations, OpenAI Responses / Chat / Gemini `response_format` encoding, Anthropic / Bedrock `CapabilityEmulated` emission, Anthropic 4-family / GPT-5 family / Gemini 2.5 family pricing, env-var pricing override, SSE / NDJSON / JsonArray UTF-8 multi-byte boundary, owned `From<ToolSpec>`.
- **`Session::tenant_id()`, `principal_id()`, `state()`, `summary()`, `total_usage()`, `current_input_tokens()`, `error()`, `total_cost_usd()`, `static_context_hash()`, `expires_at()`, `created_at()`, `updated_at()`, `todos()`, `current_plan()`, `compact_history()`, `content_overrides()`, `graph()`, `current_leaf_id()`** — read-only accessors for all encapsulated fields.
- **`Session::set_content_override()` / `clear_content_overrides()`** — public mutators for the micro-compaction overrides table.

### Changed

- **`Session.messages` deleted** — `Session::current_branch_messages()` is now the single way to get the message projection. The `messages: Vec<SessionMessage>` field caused two SSoT bugs (`clear_messages` divergence and stale-projection observation) and is gone for good.
- **`Session.{graph, state, summary, total_usage, current_input_tokens, total_cost_usd, static_context_hash, expires_at, error, todos, current_plan, compact_history, tenant_id, principal_id}`** are now `pub(crate)`. External code must use the read getters (above) and the existing mutator methods (`set_state`, `set_identity`, `update_usage`, `update_summary`, `set_todos`, `enter_plan_mode`, `record_compact`, …). This makes the SSoT invariant compiler-enforceable.
- **`types::tool::ToolDefinition` renamed to `ToolSpec`** so `ir::ToolDefinition` (the wire-format type) is the only `ToolDefinition` in the crate. `ToolSpec` keeps the runtime-only `defer_loading` and token-estimation hooks.
- **`ToolSpec.strict` and `ToolSpec.defer_loading`** are now plain `bool` (not `Option<bool>`). The previous tri-state was a serialization artifact, not a real signal; default is `false`.
- **`Agent::model("foo")` renamed to `Agent::with_model("foo")`** to match the rest of the builder API (`with_*`) and stop reading like a getter.
- **`*Service` namespace structs renamed to action verbs**:
  - `CompactService` → `Compactor`
  - `ReplayService` → `Replayer`
  - `SessionArchiveService` → `SessionArchiver`
  - `GraphSearchService` → `GraphSearcher`
  - `GraphDiffService` → `GraphDiffer`
  - `ProvenanceSummaryService` → `ProvenanceSummarizer`
- **`SessionGraph` fields encapsulated** — all 8 data fields (`id`, `created_at`, `events`, `branches`, `nodes`, `checkpoints`, `bookmarks`, `primary_branch`) are now `pub(crate)` with public read-only getters (`id()`, `created_at()`, `events()`, `branches()`, `nodes()`, `checkpoints()`, `bookmarks()`, `primary_branch()`). External code must use getters; crate-internal persistence code retains direct access.
- **`ModelTransport` trait gained `classify_error(status, body)`** with a default implementation that does generic status-code classification. All 4 transports override it: `DirectTransport` (API key hint), `VertexTransport` (quota project, model not enabled), `BedrockTransport` (ThrottlingException, AccessDenied, ModelNotReady), `FoundryTransport` (Entra token expiry, DeploymentNotFound).
- **`AgentRuntime` field count** went from 17 top-level fields to 14 + one `OrchestrationBundle`. Doc-comment grouping marks the responsibility boundaries (Core / Operations / Resources / Lifecycle / Multi-agent).
- **`AgentBuilder` 43 fields reorganized** into doc-comment groups (Auth / Provider / Resources / Hooks / MCP / ToolSearch / Session / Cloud / Plugins) for navigability.
- **`TaskRegistry`** data types (`PendingTaskTransition`, `TaskRuntime`, `TaskAssistantMetadata`, `TaskExecutionSummary`, `TaskResultSnapshot`) extracted into `task_registry_types.rs` (127 LOC) so `task_registry.rs` is focused on registry behaviour.
- **`provider-side built-in tool configs`** (`WebSearchTool`, `WebFetchTool`, `ToolSearchTool`, `ServerTool`, `UserLocation`, `CitationsConfig`) moved from `src/tools/server_tools.rs` to `src/agent/server_tools.rs`. The new location matches their use site (`AgentConfig::ServerToolsConfig`); they were never local tools.
- **`AuthorizationDenied`** moved from `types::authorization` to `crate::authorization::denied`.
- **`CompactResult`** moved from `types::compaction` to `crate::session::compact::result`.

### Removed

- **The entire legacy `Client + ClientBuilder + ProviderAdapter` stack**:
  - `src/client/adapter/{anthropic,openai,gemini,vertex,bedrock,foundry,base,traits,config,request,token_cache,bedrock_stream}.rs`
  - `src/client/{batch,files,gateway,messages/*,network,provider_profile,recovery,streaming}.rs`
  - `src/ir/compat.rs` (the `ir ↔ legacy types` bridge)
- **Legacy `src/types/{message,response,content/*,document,citations,search}.rs`** — replaced by `src/ir/{model,content,usage,...}.rs`.
- **Legacy public `Session.messages` field** — see "Changed" above.
- **`Session::clear_messages()`** — the field it cleared no longer exists; resetting a session means starting from a fresh `Session::new()`.
- **`Session::refresh_message_projection()`** — projection is now always computed on demand from the graph.
- **`Session::to_graph()` method** — replaced by `Session::graph() -> &SessionGraph`.
- **`Session::messages()` shadow alias** — never used; `current_branch_messages()` is the canonical method.
- **Legacy adapter / Phase 1b migration tests**: `tests/{adapter_integration_tests,live_matrix_tests,refactoring_verification_tests,sdk_core_tests}.rs`.

### Fixed

- **OpenAI Responses `json_schema` capability lying** — the codec declared `Support::Native` but never emitted `response_format`. Now emits the correct `text.format` envelope, including strict-mode schema transformation.
- **`Session.messages` SSoT divergence**: `clear_messages()` zeroed the cached projection without touching the graph, so external observers saw an empty session while the graph still held nodes. Resolved by deleting both `messages` field and `clear_messages()` method.
- **Cancellation token did not reach in-flight tools**: the non-streaming execution loop only checked `runtime.shutdown.is_cancelled()` at iteration boundaries. Now each tool future gets `runtime.shutdown.child_token()` and races against the parent shutdown signal, matching the streaming path.
- **Streaming SSE / NDJSON / JsonArray framing was untested for multi-byte UTF-8**. New tests verify Korean / Japanese / emoji round-trip correctly across chunk boundaries.
- **Bedrock Converse `cachePoint` was emulated in name only** — `prompt_caching: Emulated` was declared but the codec never emitted `cachePoint` blocks. Now `apply_cache_points` translates the IR cache flags and `prompt_caching: Native` is honest.
- **Gemini `functionResponse.name` placeholder** — the codec emitted an empty string because the IR did not carry the tool name. `ContentPart::ToolResult.tool_name` was added to the IR and the agent layer chains `with_tool_name(name)` everywhere a result is constructed.
- **OCP violation in HTTP error classification** — the central `classify_http_error` switched on transport id. Now lives on each transport via `ModelTransport::classify_error`, making new transports require zero edits to existing files.

### Migration notes

- **External readers of `Session.foo`** (where `foo` is now `pub(crate)`) need to switch to the corresponding getter `session.foo()`.
- **External writers of `Session.tenant_id` / `principal_id`** must call `session.set_identity(tenant, principal)`.
- **External writers of `Session.current_input_tokens`** must call `session.update_usage(&ir::Usage { input_tokens, .. })`.
- **External callers of `session.content_overrides.set(...)` / `.clear()`** must use `session.set_content_override(...)` / `session.clear_content_overrides()`.
- **`Agent::model("gpt-4o")`** → **`Agent::with_model("gpt-4o")`**.
- **`types::tool::ToolDefinition`** → **`types::ToolSpec`** for the local registry spec; `ir::ToolDefinition` is the wire format (separate type).
- **`ToolSpec { strict: Some(true), defer_loading: Some(true) }`** → **`ToolSpec { strict: true, defer_loading: true }`**.
- **`CompactService`** → **`Compactor`**, **`ReplayService`** → **`Replayer`**, **`SessionArchiveService`** → **`SessionArchiver`**, **`GraphSearchService`** → **`GraphSearcher`**, **`GraphDiffService`** → **`GraphDiffer`**, **`ProvenanceSummaryService`** → **`ProvenanceSummarizer`**.
- **`SessionGraph.events`** field access → **`graph.events()`**; same for `branches()`, `nodes()`, `checkpoints()`, `bookmarks()`, `primary_branch()`, `id()`, `created_at()`.
- **`Session::messages`** field access → **`Session::current_branch_messages()`**.
- **`Session::clear_messages()` / `refresh_message_projection()` / `to_graph()`** are gone. Reset a session by constructing a fresh `Session::new()`; read the graph via `Session::graph()`.
- **`use crate::tools::{WebSearchTool, WebFetchTool, ServerTool, ToolSearchTool, UserLocation, CitationsConfig}`** → **`use crate::agent::{...}`** (or via `Agent::server_tools::*`).
- **`use crate::types::AuthorizationDenied`** → **`use crate::authorization::AuthorizationDenied`**.
- **`use crate::types::CompactResult`** → **`use crate::session::compact::CompactResult`**.

### Verification

```
cargo build --all-features              ✅
cargo build --no-default-features       ✅
cargo clippy --all-features --all-targets -- -D warnings  ✅ 0 warnings
cargo test --all-features               ✅ ~1600 tests, 0 failed
```
