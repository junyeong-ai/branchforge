# SurrealDB Validation Lanes

This document separates normal integration checks from longer-running SurrealDB validation.

## Default Integration Lane

Run in normal validation flows:

- graph and identity round-trip
- concurrent session writes
- compaction round-trip
- scoped graph search
- queue stress
- backup/restore round-trip with archive verification

## Soak Lane

Run in a separate lane with a larger timeout budget:

- medium soak
- longer soak with queue and compaction interleaving
- repeated save/load/search loops over larger graphs

## Failure Matrix Lane

Run in a separate lane from normal CI so transient backend conditions do not destabilize the default quality gate.

Recommended cases:

- invalid endpoint
- invalid credentials
- schema mismatch
- namespace/database mismatch
- restore-after-failure
- repeated retry after transient failure

## Why Separate Lanes Matter

- default quality gates stay fast and reliable
- backend maturity checks can evolve independently
- operational validation becomes explicit instead of implicit
