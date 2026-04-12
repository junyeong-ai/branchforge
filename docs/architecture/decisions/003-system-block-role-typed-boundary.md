# ADR-003: `SystemBlockRole` typed boundary

**Status:** Accepted  •  **Date:** 2026-04-10

## Context

Several codecs needed to mark certain IR messages as "this is the
system prompt boundary" so downstream encoding could route them
into a wire-level `system`/`systemInstruction` field rather than the
inline message list. The original implementation used a magic
string sentinel (`"__branchforge_system_boundary__"`) stored in
the message's role. That was:

- **Unsafe**: a user message containing the sentinel would be
  silently re-routed. Low probability, non-zero consequence.
- **Opaque**: the sentinel was invisible to every tool in the
  codebase — type-checkers, serde, grep-based refactors. It only
  existed in the author's memory.
- **Leaky**: if the sentinel ever appeared on the wire (e.g. via
  a bug in `encode_request`) no test would catch it.

## Decision

Introduce `SystemBlockRole`, a small typed enum carried on
`SessionMessage` as an optional field, and make "is this the system
boundary?" a compile-time check rather than a string match. Codecs
branch on `role: Some(SystemBlockRole::Boundary)` and emit the
appropriate wire-level system field. The enforcement is captured
by `tests/codec_contract.rs::system_block_boundary_must_not_leak`,
which walks all five codecs and asserts the `SystemBlockRole`
marker never appears on the wire body.

## Consequences

- **Compile-time safety**: you cannot forget the boundary check or
  fat-finger the string.
- **Grep-able**: every call site shows up in an IDE rename and in
  `cargo doc`.
- **Enforced by test matrix**: the contract test prevents accidental
  regression across all codecs in one pass.
- **Slightly larger IR**: one optional enum field per message. Zero
  cost when unused.

## Alternatives rejected

- **Keep the magic string, add a test that greps for it.** Still
  allows a user message containing the sentinel to collide with
  the marker path.
- **Promote "system" to a first-class role alongside `User`,
  `Assistant`, `Tool`.** Would force every consumer to handle
  `Role::System` even in contexts where system messages do not
  exist, and complicates role-based authorization.
- **Per-codec sentinel types.** DRY violation; the boundary concept
  is shared across codecs.
