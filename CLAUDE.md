# CLAUDE.md

Rust-native agent SDK. `SessionGraph` is the canonical state; message lists are derived projections.

## Commands

```bash
cargo build --release
cargo test --all-features
cargo nextest run --all-features                # CI
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps
```

## Feature Flags

```bash
cargo build                                     # default: coding-tools
cargo build --no-default-features               # pure SDK core (zero cloud/DB deps)
cargo build --features "full"                   # all features
cargo build --all-features                      # full + multimedia
```

Feature groups: `coding-tools`, `cli-auth`, `mcp`, `scheduling`, `cloud-all` (aws/gcp/azure/openai/gemini), `persistence-all` (jsonl/postgres/redis).

## Design Invariants

These are **compiler-enforced** constraints. Violating them breaks the build.

- **SSoT**: `SessionGraph` is the single source of truth. `Session::current_branch_messages()` rebuilds from graph on every call. `Session.graph` is `pub(crate)` — external mutation impossible.
- **Type safety**: Consumed token counts use `TokenCount(u64)` newtype. IDs use `uuid_id!`/`string_id!` macros — `NodeId` and `BranchId` cannot be mixed at compile time.
- **Provider 3-axis**: `ModelCodec` (wire format) x `ModelTransport` (auth+endpoint) x `EndpointShape` (URL bridge). Never collapse — orthogonality enables free composition (e.g. Gemini-on-Vertex).
- **Capability honesty**: Each codec declares `ProviderCapabilities` with `Unsupported` defaults. If a codec advertises a capability, it must implement it.
- **OCP on errors**: `ModelTransport::classify_error()` is the extension point. Adding a new transport never requires editing the central `classify_response_error`.
- **IR is provider-neutral**: `src/ir/` types (`ModelRequest`, `ModelResponse`, `ContentPart`, `Usage`) are the canonical representation. Codecs translate between IR and wire format, emitting `ModelWarning` for lossy encodes.
- **RAII shutdown**: `AgentRuntime._shutdown_guard: DropGuard` auto-cancels on last Arc drop.

## Key Conventions

- **Errors**: typed enums (`SessionError`, `McpError`, `GraphError`). `Error::Provider { kind, hint }` carries actionable hints. `Error::Config(String)` is intentional for developer-facing messages.
- **Token types**: `ir::Usage` fields are `u64`. `TokenCount` wraps `u64` for consumed-count fields. Wire-format limits (`max_output_tokens`, `max_tokens`) remain `u32` per provider API contracts.
- **Tool call linkage**: `tool_call_id` flows end-to-end (`ToolCallRecord` → graph node → OTel span).
- **Naming**: Tool names are PascalCase (`Read`, `Bash`). Skill names are kebab-case (`commit`, `review-pr`). Manager types renamed to role-specific: `HookRegistry`, `ToolSearchEngine`, `CredentialResolver`, `ProcessScheduler`, `PluginLoader`.
- **Permissions**: `PermissionDecision { Allow, Ask, Deny }` with structured `PermissionDeniedReason`. `ExecutionMode::Plan { allowed_tools }` is parameterized.

## Lock Ordering

- Never hold a registry/engine lock across `.await` on a user-supplied future.
- When multiple locks needed: session > task_registry > orchestrator.
- Release data locks before async operations on removed items (MCP pattern from D10).

## Custom Providers

Users can register custom providers without modifying the crate:

```rust
let codec = Arc::new(OpenAiChatCodec::new());
let transport = Arc::new(DirectTransport::new(url, DirectAuth::Bearer(key)));
let client = ProviderClient::new(codec, transport);
let agent = Agent::builder().provider_client(client).build().await?;
```
