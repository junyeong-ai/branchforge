# SurrealDB Upgrade Policy

This document defines the release-process expectations for upgrading the SurrealDB backend.

## Scope

Use this guide when changing either:

- the SurrealDB server version
- the session-store schema version used by `SurrealPersistence`

## Version Policy

- pin the SurrealDB image version in deployment manifests
- treat version bumps as release-managed changes, not ad hoc runtime changes
- require schema-version checks and integration validation before rollout

## Upgrade Checklist

1. confirm the target SurrealDB version is supported by the current release
2. run schema-version tracking checks
3. run backup/restore validation
4. run queue, compaction, and scoped-search integration tests
5. run medium or longer soak validation
6. confirm PostgreSQL fallback remains healthy

## Rollback Policy

- if restore, queue, or compaction validation fails, stop writes and roll back the application/backend pairing
- do not continue with mixed backend semantics after a failed validation gate
- prefer reverting to the previously validated version instead of hot-fixing schema state in place

## Release Discipline

- any schema-affecting change must update the backend migration path
- any release that changes persistence semantics must update the operations guide
- any backend upgrade must be recorded in release notes with validation results
