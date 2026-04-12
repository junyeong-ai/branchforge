# Open Findings Ledger — Round-4 frozen 2026-04-12

Pending findings indexed by phase task. This file only tracks lifecycle state; the Round-4 work plan is the authoritative content source. Closing a finding happens in the phase PR and moves the entry to `findings_resolved.md` with commit sha, in the same PR.

## Lifecycle transitions (binding)

1. **open → resolved** — Phase PR merges the fix. PR author moves the finding to `findings_resolved.md` under `# Resolved Findings` with `commit <sha>`. Same PR.
2. **open → rejected** — Evidence invalidates the finding. Author adds it to `findings_resolved.md` under `# Rejected Findings` with refutation + lesson.
3. **open → invariant-added** — The finding generalizes into a new rule. Author appends to `.claude/rules/architecture.md` and closes the finding citing the invariant number.

Re-opening a resolved/rejected finding requires a new `file:line` with byte-exact quote refuting the prior decision.

## Frozen axis list (v1.0)

`architecture` · `provider-graph` · `tools-naming` · `agent-loop` · `info-hygiene-heuristics`

Discovering a new axis during a review → stop the review and open a skill-update PR for `.claude/skills/design-review/`. Silent axis expansion is forbidden.

## Phase → finding mapping

| Phase | Finding set | Content source |
|---|---|---|
| R   | R-1..R-12 | Round-4 plan · Phase R section |
| 0   | F-P0-1..F-P0-7 | Round-4 plan · Phase 0 |
| 1   | F-P1-1..F-P1-5 | Round-4 plan · Phase 1 |
| 2   | F-P2-1..F-P2-8 | Round-4 plan · Phase 2 |
| 3   | F-P3-1..F-P3-5 | Round-4 plan · Phase 3 |
| 4   | F-P4-1..F-P4-8 | Round-4 plan · Phase 4 |
| 5   | F-P5-1..F-P5-8 | Round-4 plan · Phase 5 |
| 5.5 | F-P5.5-1..F-P5.5-8 | Round-4 plan · Phase 5.5 |
| 6   | F-P6-1..F-P6-6 | Round-4 plan · Phase 6 |
| 7   | F-P7-1..F-P7-6 | Round-4 plan · Phase 7 |
| 7.5 | F-P7.5-1..F-P7.5-7 | Round-4 plan · Phase 7.5 |
| 8   | F-P8-1..F-P8-5 | Round-4 plan · Phase 8 |

The plan content is not mirrored here — that would be the exact dual-system smell the protocol prevents. This ledger is the state tracker; the plan is the content source.

## Convergence targets

- **Monotone decrease** of the `open` set across PRs
- **Zero** new axes between Phase R and Phase 8
- **Zero** re-submissions of `F-rej-001`..`007` (or future rejections)
