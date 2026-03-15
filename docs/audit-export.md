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
- parent session lineage
- graph state
- branch export
- graph stats
- optional compact history
- optional pending queue snapshot

Archive bundles can also be imported back into session storage, then validated with `RestoreVerifier` before the restored session is treated as trusted recovery state.

Restore imports are intentionally conservative:

- existing sessions are not overwritten implicitly
- pending queue restore is verified against the imported bundle
- failed restore attempts are rolled back before the bundle is considered recovered

## Restore Verification

Use `RestoreVerifier` after restore to validate:

- session identity
- tenant/principal identity
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
- `backend-selection.md`
