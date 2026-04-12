# CLAUDE.md

## Principles

1. **Cite `file:line`** — Read the code before claiming anything. "Feels wrong" is not evidence. Every finding must reference byte-exact code.
2. **Fix root causes** — Never defer a real fix with a band-aid. If a pattern is broken, fix the pattern — not the symptom.
3. **No backwards compatibility** — Delete legacy in the same PR. No `// deprecated`, no shims, no re-exports.
4. **Trait objects over enum dispatch** for extension points. `*Config` structs over magic constants. `Default + builder()` over multiple constructors.
5. **Naming consistency** — Follow `.claude/rules/naming.md` taxonomy. Check the open/closed enum list before proposing any rename.
6. **Context efficiency** — Do not duplicate data across files. Every token in a system prompt or tool description must earn its place.
7. **Architecture invariants are law** — The 10 invariants in `.claude/rules/architecture.md` auto-load on `src/**` edits. A proposal contradicting any invariant is invalid.

## Commands

```bash
cargo build --all-features
cargo test --all-features
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps
cargo build --lib --no-default-features          # pure-core gate
cargo test --lib audit_ --all-features           # FSM + enum audit
```

All seven gates must be green before shipping.

## Feature Flags

```bash
cargo build                                     # default: anthropic-direct (pure Layer 1)
cargo build --features "coding-tools"           # local-fs + bash + tree-sitter
cargo build --features "full"                   # all features except multimedia
cargo build --all-features                      # full + multimedia
```

## Error Conventions

- Typed enums (`SessionError`, `McpError`, `GraphError`) for module-internal errors.
- `Error::Provider { kind, hint }` for vendor failures with actionable hint.
- `Error::InvalidRequest(String)` for encode-time preflight rejections.
- `Error::Config(String)` for configuration mistakes.

## Lock Ordering

- Never hold a lock across `.await` on a user-supplied future.
- Multi-lock order: `session` > `task_registry` > `orchestrator`.

## Progressive Disclosure

Module-specific rules in `.claude/rules/` auto-load when editing files matching their `paths` frontmatter:

| File | Scope |
| :--- | :--- |
| `architecture.md` | 10 invariants — loads on all `src/**` edits |
| `client.md` | Provider stack, codecs, transports, ProfileRegistry |
| `schema.md` | `SchemaPolicy` pipeline, walker, cycle detection |
| `ir.md` | Provider-neutral IR types, `JsonSchemaSpec`, warnings |
| `graph-session.md` | `SessionGraph` SSoT, event replay, fork semantics |
| `tools.md` | `Tool` trait, `ExecutionContext`, naming, cancellation |
| `auth.md` | `CredentialProvider`, OAuth refresh, token storage, preamble SSoT |
| `security.md` | `SecureFs`, `BashAnalyzer`, sandbox, resource limits |
| `naming.md` | Type-suffix taxonomy, FSM terminology, enum evolution |
| `events.md` | EventBus fire-and-forget contract, StreamAggregator |
