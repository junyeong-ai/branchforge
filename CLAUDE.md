# CLAUDE.md

Rust-native agent runtime. Graph-first sessions, provider-neutral IR, native structured outputs across all codecs.

## Development Principles

Every change must satisfy ALL of the following. These are non-negotiable.

1. **Evidence-based decisions only** — Every finding, proposal, or rename must cite `file:line` with byte-exact code. "Feels wrong" is not evidence. `Read` the file before claiming anything about it.
2. **Long-term, not patchwork** — Analyze root causes. Never apply a band-aid that defers the real fix. If a pattern is broken, fix the pattern — not the symptom.
3. **No backwards compatibility** — Design as if the codebase was always this way. Delete legacy immediately in the same PR. No `// deprecated`, no shims, no re-exports of removed items.
4. **Flexible, extensible, maintainable** — Prefer trait objects over enum dispatch for extension points. Prefer `*Config` structs over magic constants. Prefer `Default + builder()` over multiple constructors.
5. **Naming consistency** — Follow `.claude/rules/naming.md` taxonomy (Manager/Registry/Tracker/Catalog/Store/Set/Engine/Aggregator/Snapshot/Payload). Check the open/closed enum list before proposing any rename.
6. **Minimize AI context waste** — Do not send the model information it already knows (Rust syntax, what SSoT means). Do not duplicate data across files. Every token in a system prompt or tool description must earn its place.
7. **Architecture invariants are law** — The 10 invariants in `.claude/rules/architecture.md` auto-load on `src/**` edits. A proposal contradicting any invariant is invalid by construction.

## Commands

```bash
cargo build --release
cargo test --all-features
cargo nextest run --all-features                # CI only
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps
```

All six gates must be green before shipping.

## Feature Flags

```bash
cargo build                                     # default: coding-tools
cargo build --no-default-features               # pure SDK core (zero cloud/DB deps)
cargo build --features "full"                   # all features except multimedia
cargo build --all-features                      # full + multimedia
```

Groups: `coding-tools`, `cli-auth`, `mcp`, `scheduling`, `multimedia`, `cloud-all` (aws/gcp/azure/openai/gemini), `persistence-all` (jsonl/postgres/redis), `plugins`, `otel`.

## Error Conventions

- Typed enums (`SessionError`, `McpError`, `GraphError`) for module-internal errors.
- `Error::Provider { kind, hint }` carries an actionable hint for well-known vendor failures.
- `Error::InvalidRequest(String)` for encode-time preflight rejections.
- `Error::Config(String)` for developer-facing configuration mistakes.

## Lock Ordering

- Never hold a lock across `.await` on a user-supplied future.
- Multi-lock order: `session` > `task_registry` > `orchestrator`.
- `McpManager`: `servers` > `tool_cache` > `degraded`.

## Progressive Disclosure

Module-specific rules in `.claude/rules/` auto-load when editing files matching their `paths` frontmatter:

| File | Scope |
| :--- | :--- |
| `architecture.md` | 10 invariants — loads on all `src/**` edits |
| `client.md` | Provider stack, codecs, transports, presets |
| `schema.md` | `SchemaPolicy` pipeline, walker, cycle detection |
| `ir.md` | Provider-neutral IR types, `JsonSchemaSpec`, warnings |
| `graph-session.md` | `SessionGraph` SSoT, event replay, fork semantics |
| `tools.md` | `Tool` trait, `ExecutionContext`, naming, cancellation |
| `auth.md` | `CredentialProvider`, OAuth refresh, token storage |
| `security.md` | `SecureFs`, `BashAnalyzer`, sandbox, resource limits |
| `naming.md` | Type-suffix taxonomy, FSM terminology, "no dual systems" |
| `events.md` | EventBus fire-and-forget contract, StreamAggregator |

## Review Protocol

Design reviews use `/design-review <axis>`. Ground truth lives in `.claude/review/` (git-committed, not per-user memory). See `.claude/skills/design-review/SKILL.md` for the full procedure.
