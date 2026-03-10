# SurrealDB Operations

This guide covers the current operational expectations for the SurrealDB backend.

## Current Positioning

SurrealDB is a live optional backend and a strong graph-native production candidate.

PostgreSQL remains the safest default production backend because its operational story is still more mature.

## Bootstrap and Migration

`SurrealPersistence` performs these steps on startup:

1. ensure namespace and database exist
2. ensure the `schema_version` table exists
3. read the current session-store schema version
4. run forward migrations until the current schema version is reached

The current migration contract is intentionally additive and idempotent.

## Required Configuration

- SurrealDB SQL endpoint
- namespace
- database
- username
- password

## Operational Checks

Before using SurrealDB in production-like environments, verify:

- save/load/list/delete round-trips
- summary and queue behavior
- concurrent session write behavior
- compaction round-trips
- scoped graph search behavior
- large graph baseline behavior

## Backup and Restore Strategy

Current recommendation:

- use backend-level database backup for disaster recovery
- use session export and audit bundles for application-level recovery and debugging
- validate restore with session load, graph search, and replay checks after recovery

## Upgrade Policy

- pin SurrealDB version in deployment manifests
- validate schema migration and round-trip tests before upgrading
- keep PostgreSQL as the fallback production backend until SurrealDB operational maturity is fully accepted
