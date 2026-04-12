# ADR-006: 4-layer feature gating

**Status:** Accepted  •  **Date:** 2026-04-10

## Context

BranchForge is a general-purpose agent SDK. Different deployment
topologies have radically different trust and dependency profiles:

- **Chat / customer-support bots** run on any host; they never
  touch the local filesystem or shell. They need codecs + session
  management only.
- **Research / knowledge-management agents** need filesystem reads
  for `Read` / `Glob` / `Grep` but must not execute shell commands.
- **Coding agents** need both file I/O and shell execution with
  AST-level bash safety analysis and process-level resource limits.
- **Cloud and persistence integrations** pull in SDK-sized native
  dependencies (sqlx, aws-sdk-*, opentelemetry-otlp) that most
  deployments do not want.

Bundling all of the above into the default build meant the smallest
reasonable agent transitively depended on `tree-sitter`,
`rustix`, `sqlx`, and the entire AWS SDK. `cargo add branchforge`
pulled in ~60 MB of compiled code for a chatbot that just wants to
call the Anthropic API.

## Decision

Four orthogonal feature layers with strict dependency directionality:

| Layer | Feature flag | Contents | Deps added |
| :--- | :--- | :--- | :--- |
| 1 — core | (always on) | codecs, transports, session graph, IR, permission DSL, budget, recovery | minimal |
| 2a — local-fs | `local-fs` | `Read`/`Write`/`Edit`/`Glob`/`Grep`, `SecureFs`, Landlock/Seatbelt sandbox, research + plan subagents | `rustix` |
| 2b — coding-tools | `coding-tools` (⊇ `local-fs`) | `Bash`/`KillShell`, `BashAnalyzer`, `ResourceLimits`, coding subagent | `tree-sitter`, `tree-sitter-bash` |
| 3 — cloud/persistence/obs | `cloud-all`, `persistence-all`, `otel`, `mcp`, … | one feature per external service family | vendor SDKs |

Layer 1 is unconditional and unflaggable — the SDK always compiles
to a minimal chat-capable agent with no filesystem or shell access.
Layer 2a is distinct from 2b because **file access and shell
execution are categorically different security postures**: knowledge-
management and data-analysis agents need read/write access without
ever wanting a subprocess. Layer 2b depends on Layer 2a because
shell tools always imply filesystem access.

The layer contract is enforced by:

- **Compiler**: every module in `src/tools/file/*`,
  `src/security/*`, `src/tools/bash.rs`, etc. is `#[cfg]`-gated on
  its layer's feature flag.
- **CI**: the build matrix runs the four corners (pure, local-fs,
  coding-tools, all-features) and must pass all of them.
- **Docs**: `docs/architecture/layering.md` carries the authoritative
  dependency directionality diagram.

## Consequences

- **Smallest builds stay small**: pure-core compile has zero tool
  filesystem or shell dependencies. A chatbot ships clean.
- **Categorical security boundaries**: a research agent cannot
  accidentally be compiled with shell tools because the layer
  gate prevents it.
- **Explicit cloud cost**: AWS/GCP/Azure stay opt-in. Callers that
  never use Bedrock never pay for its SDK.
- **CI cost**: the 4-configuration matrix takes ~4× as long as a
  single default build. Accepted — build time is cheap; shipping
  a bloated default is expensive.
- **Migration cost** (one-time): all existing `src/tools/*.rs`,
  `src/security/*.rs`, and subagent modules needed `#[cfg]`
  annotations. Done in the Phase 1 foundation work.

## Alternatives rejected

- **Single monolithic build with runtime-disabled features.** Would
  still pay the binary-size cost; defeats the purpose.
- **3 layers (pure / local / cloud)**. Bundles file tools with
  shell execution, conflating two distinct security postures.
  Rejected on the explicit ground that knowledge-management agents
  need file access but must not run subprocesses.
- **One feature per tool.** Combinatorial explosion; unmaintainable.
