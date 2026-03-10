# SurrealDB Operations

This guide covers the current operational expectations for the SurrealDB backend.

## Current Positioning

SurrealDB is a live optional backend and a strong graph-native production candidate.

PostgreSQL remains the safest default production backend because its operational story is still more mature.

## Required Configuration

- SurrealDB SQL endpoint
- namespace
- database
- username
- password

## Bootstrap and Migration

`SurrealPersistence` performs these steps on startup:

1. ensure namespace and database exist
2. ensure the `schema_version` table exists
3. read the current session-store schema version
4. run forward migrations until the current schema version is reached

Current migration assumptions:

- migrations are additive and idempotent
- the current schema version is defined in code
- schema version state lives inside SurrealDB

## Preflight Checks

Before enabling SurrealDB in a production-like deployment, verify:

- the SQL endpoint is reachable
- credentials work against the configured namespace and database
- `schema_version` exists and matches the expected version
- a save/load/list smoke test succeeds
- queue and summary paths succeed
- backup/restore round-trip verification succeeds

## Health Checks

Recommended health checks:

- SQL endpoint reachability
- read access to `schema_version`
- ability to load a known session snapshot
- ability to list sessions for a known tenant

## Incident Playbook

### Save or Load Failures

Check in order:

1. endpoint reachability
2. credentials
3. namespace and database selection
4. current schema version
5. recent migration output

If failures persist, stop writes to SurrealDB and fall back to PostgreSQL if the deployment supports backend fallback.

### Schema Version Mismatch

- compare runtime `CURRENT_SCHEMA_VERSION` with the stored `schema_version`
- if the database is behind, run the application migration path in a controlled environment first
- if the database is ahead, do not continue writes with the older binary

### Queue Inconsistency

- check whether repeated dequeue or cancel operations are producing duplicate or stale records
- validate queue state by listing pending records for a single known session
- if queue inconsistency is suspected, export the affected session archive before remediation

### Restore Verification Failure

- load the restored session
- run `RestoreVerifier`
- compare branch count, node count, bookmarks, checkpoints, and replay projection
- if verification fails, do not treat the restore as complete

## Backup and Restore Strategy

Use two layers:

### Backend-Level Backup

Use the database's backup mechanism for disaster recovery.

### Application-Level Archive

Use session archive bundles for targeted recovery, debugging, and audit use cases.

After restore, validate:

- session load succeeds
- `RestoreVerifier` passes
- replay works on the restored branch
- graph search returns expected known nodes
- export output is still well-formed

## Upgrade Policy

- pin SurrealDB version in deployment manifests
- validate migrations and backend integration tests before upgrading
- verify save/load/list, queue, compaction, and restore checks against the target version
- keep PostgreSQL as the fallback production backend until SurrealDB operational maturity is fully accepted

Recommended release checklist:

1. run schema-version tracking test
2. run backup/restore round-trip validation
3. run queue and compaction integration checks
4. run medium/longer soak validation
5. confirm fallback PostgreSQL deployment path is still healthy

Recommended rollback rule:

- if the upgraded SurrealDB version fails any restore, queue, or compaction validation, stop writes and roll back the application/backend pairing before resuming traffic

## Rollback and Fallback

- if a SurrealDB upgrade fails validation, roll back the application before resuming writes
- if runtime behavior becomes unstable, switch traffic back to PostgreSQL where available
- avoid dual-write rollout unless a dedicated consistency plan is defined

## Validation Checklist

The current repository already validates:

- graph and identity round-trips
- concurrent session writes
- compaction round-trips
- scoped graph search
- queue stress behavior
- large graph baseline behavior
- invalid endpoint failure mode
- backup/restore round-trip with archive verification

Longer soak and broader failure matrices should still be treated as ongoing operational maturity work.
