# CLAUDE.md

Rust-native agent runtime. Graph-first sessions, provider-neutral IR, native structured outputs across all codecs.

## Commands

```bash
cargo build --release
cargo test --all-features
cargo nextest run --all-features                # CI only
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps
```

All six gates must be green before shipping. `cargo test` covers doctests and the example in `examples/structured_output.rs`.

## Feature Flags

```bash
cargo build                                     # default: coding-tools
cargo build --no-default-features               # pure SDK core (zero cloud/DB deps)
cargo build --features "full"                   # all features except multimedia
cargo build --all-features                      # full + multimedia
```

Groups: `coding-tools`, `cli-auth`, `mcp`, `scheduling`, `multimedia`, `cloud-all` (aws/gcp/azure/openai/gemini), `persistence-all` (jsonl/postgres/redis), `plugins`, `otel`.

## Design Invariants

Compiler-enforced. Violating them breaks the build.

- **SSoT**: `SessionGraph` is the single source of truth. `Session::current_branch_messages()` rebuilds from the graph on every call; there is no cached `messages` field. `Session.graph` is `pub(crate)` so external mutation is impossible.
- **Type safety**: Consumed token counts use the `TokenCount(u64)` newtype. IDs use the `uuid_id!`/`string_id!` macros — `NodeId` and `BranchId` cannot be mixed at compile time.
- **Provider 3-axis**: `ModelCodec` × `ModelTransport` × `EndpointShape`. Never collapse the axes — orthogonality is what lets Vertex-Gemini, Vertex-Anthropic, Bedrock-Anthropic, and Foundry-Anthropic fall out for free. Rules in `.claude/rules/client.md`.
- **Capability honesty**: Each codec returns `&'static ProviderCapabilities` with `Unsupported` defaults. If a codec advertises `json_schema: Native` it must emit a wire-level schema; the `capability_honesty_response_format` matrix in `tests/codec_contract.rs` enforces this.
- **OCP on errors**: `ModelTransport::classify_error()` is the single extension point for vendor-specific HTTP error patterns. Adding a new transport never requires editing `provider_client::classify_response_error`.
- **IR is provider-neutral**: `src/ir/` types are the canonical representation. Codecs translate between IR and wire format, emitting `ModelWarning::LossyEncode` for anything dropped. Rules in `.claude/rules/ir.md`.
- **Structured outputs are native on every codec**: All five codecs use the shared `SchemaPolicy` / `prepare_schema` pipeline in `src/client/schema/`. Each codec holds a `const SCHEMA_POLICY: SchemaPolicy = SchemaPolicy::X()` next to its other constants. Rules in `.claude/rules/schema.md`.
- **RAII shutdown**: `AgentRuntime._shutdown_guard: DropGuard` auto-cancels on last `Arc` drop.

## Error Conventions

- Use typed enums (`SessionError`, `McpError`, `GraphError`) for module-internal errors.
- `Error::Provider { kind, hint }` carries an actionable hint for well-known vendor failures.
- `Error::InvalidRequest(String)` is the right variant for encode-time preflight rejections (e.g. Anthropic prefilling + structured outputs).
- `Error::Config(String)` is intentional for developer-facing configuration mistakes.

## Lock Ordering

- Never hold a registry/engine lock across `.await` on a user-supplied future.
- Multi-lock order: `session` > `task_registry` > `orchestrator`.
- Release data locks before `.await` on removed items (MCP pattern).

## Progressive Disclosure

Module-specific rules live in `.claude/rules/` and auto-load when Claude reads files matching their `paths` frontmatter:

| File | Scope | Load when editing |
| :--- | :--- | :--- |
| `client.md` | Provider stack, codecs, transports, presets | `src/client/**` |
| `schema.md` | `SchemaPolicy` pipeline, walker, cycle detection | `src/client/schema/**` |
| `ir.md` | Provider-neutral IR types, `JsonSchemaSpec`, warnings | `src/ir/**` |
| `graph-session.md` | `SessionGraph` SSoT, event replay, fork semantics | `src/graph/**`, `src/session/**` |
| `tools.md` | `Tool` trait, `ExecutionContext`, naming, cancellation | `src/tools/**` |
| `auth.md` | `CredentialProvider`, OAuth refresh, token storage | `src/auth/**` |
| `security.md` | `SecureFs`, `BashAnalyzer`, sandbox, resource limits | `src/security/**` |

When editing across module boundaries, multiple rule files load automatically.
