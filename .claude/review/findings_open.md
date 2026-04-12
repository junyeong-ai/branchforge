# Open Findings Ledger

## Lifecycle transitions (binding)

1. **open → resolved** — PR merges the fix. Move entry to `findings_resolved.md` under `# Resolved Findings` with `commit <sha>`.
2. **open → rejected** — Evidence invalidates the finding. Move to `findings_resolved.md` under `# Rejected Findings` with refutation.
3. **open → invariant-added** — The finding generalizes. Append to `.claude/rules/architecture.md` and close citing the invariant number.

## Frozen axis list (v1.0)

`architecture` · `provider-graph` · `tools-naming` · `agent-loop` · `info-hygiene-heuristics`

## Open findings

### F-standing-001 · Stale task-id references in code comments

~190 pre-existing `Phase X-Y` references in `src/**` and `tests/**` comments. Each phase PR that touches a file containing a stale reference must rewrite the comment to describe the current invariant directly, with no work-item id. Bulk regex sweeps are forbidden (prior attempt destroyed valid Rust syntax). Per-file manual edits only.
