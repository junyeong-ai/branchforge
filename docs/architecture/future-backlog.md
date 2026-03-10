# Future Backlog

This backlog captures the highest-value long-term follow-up work after the graph-first redesign.

## P0 — Graph Integrity and Durability

### ISSUE-GRAPH-021 — GraphValidator

- validate branch head consistency
- validate bookmark/checkpoint targets
- detect orphan nodes
- validate parent-child graph shape
- validate provenance consistency where required

### ISSUE-ARCHIVE-001 — Archive Import and Restore Contract

- formalize import path for `SessionArchiveBundle`
- define queue and summary inclusion policy
- add cross-backend restore verification

### ISSUE-SURREAL-029 — Longer Soak and Failure Matrix

- longer save/load/search cycles
- broader auth/bootstrap/migration failure classes
- repeated restore-after-soak checks

## P1 — Provenance and Audit Expansion

### ISSUE-PROV-013 — Explicit Task Lineage

- strengthen task provenance fields
- connect delegated task lineage to graph records

### ISSUE-PROV-014 — Provenance-Aware UX

- expose provenance digests across more graph exploration surfaces
- add provenance-aware filters to more user-facing entry points

## P2 — Compaction and Cache Observability

### ISSUE-COMPACT-001 — Structured Compaction Summaries

- make summary output preserve explicit slots such as objective, risks, and next steps

### ISSUE-CACHE-001 — Cache Observability

- segment-level cache effectiveness
- invalidation reasons
- saved-token reporting

## P3 — UX and Product Polish

### ISSUE-GRAPH-022 — Branch Activity Grouping

- group active, delegated, compacted, and dormant branches

### ISSUE-GRAPH-023 — Exploration Walkthroughs

- graph exploration guide
- audit/export guide extensions

## P4 — Search Evolution

### ISSUE-SEM-001 — Semantic Search Design Spike

- define semantic units
- compare storage/index options
- determine whether implementation is justified
