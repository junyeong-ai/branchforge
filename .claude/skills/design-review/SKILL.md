---
name: design-review
description: Axis-parameterized design review of Branchforge. Invoke for "프로젝트 심층 분석/리뷰/설계 검토/architecture review" requests. Runs ONE frozen axis against verified invariants and rejects findings contradicting .claude/review/findings_resolved.md or .claude/rules/architecture.md. Argument = axis name.
allowed-tools: Read, Grep, Glob
user-invocable: true
---

# Design Review (axis-parameterized)

**Argument (required)**: one of `architecture` · `provider-graph` · `tools-naming` · `agent-loop` · `info-hygiene-heuristics`

Axes are frozen at v1.0. Discovering a new axis mid-review → **stop**, propose a skill-update PR. No silent expansion.

## 1 · Ground-truth load (in order, no skipping)

1. `CLAUDE.md`
2. `.claude/rules/architecture.md`
3. Axis-specific rules files (see `axes/<axis>.md` · "Rules to load additionally")
4. `.claude/review/verified_structure.md`
5. `.claude/review/findings_resolved.md`
6. `.claude/review/findings_open.md`
7. `.claude/skills/design-review/axes/<axis>.md`

All ground-truth files are git-committed under `.claude/` — per-user auto-memory is deliberately not consulted so review state stays synchronised across collaborators.

## 2 · Evidence schema (every finding)

```yaml
id: F-<axis>-<seq>
file: <path>
line: <start>-<end>
quote: |
  <byte-exact copy from Read — mismatch = auto-discard>
rule_violated: <invariant # or rules-file#section, or null>
evidence: <file:line-based reasoning, required when rule_violated is null>
proposed_fix: <specific, verifiable, not abstract>
severity: critical | major | minor
```

## 3 · Reject before submission

- **Byte mismatch** — `quote` must equal the exact bytes at `Read(file, offset=line)`. If not, discard the finding.
- **No anchor** — Either `rule_violated` or `evidence` must resolve. Hand-waved "feels wrong" findings are discarded.
- **Already rejected** — Grep `.claude/review/findings_resolved.md`; if the finding matches an `F-rej-NNN` entry, discard unless new refuting evidence is attached.
- **Contradicts verified_structure.md** — If `.claude/review/verified_structure.md` says "X exists at Y" and the finding claims X is missing, discard.
- **Contradicts architecture.md** — If the proposed fix violates any of the 10 invariants, discard and redesign.

## 4 · Output format

```
findings:
  - id: F-<axis>-001
    ...
  - id: F-<axis>-002
    ...
summary: <≤3 sentence axis-level judgment>
```

Cap: **15 findings per run**. More than 15 → split axis or indicate saturation.

## 5 · Termination

The checklist in `axes/<axis>.md` defines scope. When every bullet is visited, **stop**. Observations outside the checklist are not findings — they are skill-update proposals.

## 6 · Non-goals (do not do these)

- Do not generate a full report narrative. The YAML block IS the output.
- Do not rank axes against each other. One axis per invocation.
- Do not propose renames without checking `naming.md` open/closed-list.
- Do not propose default-value changes without reading the module's rules file.
- Do not propose caching anything derived from `SessionGraph` (invariant #1).
- Do not re-explain what SSoT / trait / lock ordering mean. Ground-truth readers know.
