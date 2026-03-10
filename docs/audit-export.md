# Audit and Export Guide

The runtime supports policy-aware export for debugging, recovery, and audit workflows.

## Main Concepts

- `ExportPolicy`
- `AuditBundle`
- `SessionArchiveBundle`
- `RestoreVerifier`

## Export Policy

`ExportPolicy` controls whether exported data includes:

- identity
- provenance
- tool payloads

Use it when you need to balance debugging value against privacy or sharing constraints.

## Audit Bundle

`AuditBundle` is intended for inspection and review.

It contains:

- session identity
- branch identity
- graph statistics
- policy-shaped export content

## Session Archive Bundle

`SessionArchiveBundle` is intended for application-level durability and restore verification.

It contains:

- bundle version
- graph state
- branch export
- graph stats
- optional compact history

## Restore Verification

Use `RestoreVerifier` after restore to validate:

- session identity
- branch count
- node count
- bookmark count
- checkpoint count
- replay/export projection consistency

## Practical Guidance

- use backend-level backups for disaster recovery
- use archive bundles for application-level recovery and debugging
- use audit bundles for inspection and traceability

## Related Guides

- `session.md`
- `architecture/surrealdb-operations.md`
- `backend-selection.md`
