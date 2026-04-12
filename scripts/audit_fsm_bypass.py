#!/usr/bin/env python3
"""
FSM bypass audit.

Scans src/ for assignments of the form `<lvalue> = <Fsm>::Variant` where
<Fsm> is one of the five closed-list FSMs declared in .claude/rules/naming.md.
Legitimate construction/rehydration sites are excluded:

  - persistence backends (persistence_*.rs, archive.rs) — event-log replay
    rebuilds state from the durable event log, which is not a runtime mutation
  - transition_to / try_reset method bodies — the validated helpers themselves
  - lines preceded by a `// fsm-init:` or `// fsm-rebuild:` marker — explicit
    carve-out with rationale documented inline

Anything else is a bypass: runtime mutation of FSM state without going through
the validated transition helper. Exits non-zero with a report when violations
are found.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

FSMS = [
    "AgentState",
    "SessionState",
    "PlanState",
    "QueueItemState",
    "McpClientState",
]

# Files whose direct assignments are event-log rehydration, not mutation.
REHYDRATION_FILES = {
    "persistence.rs",
    "persistence_jsonl.rs",
    "persistence_redis.rs",
    "persistence_postgres.rs",
    "archive.rs",
}

CARVE_OUT_PREFIXES = ("// fsm-init:", "// fsm-rebuild:")

ASSIGN_PATTERN = re.compile(
    r"\.\w+\s*=\s*(" + "|".join(FSMS) + r")::"
)


def is_in_transition_method(lines: list[str], idx: int) -> bool:
    """Heuristic: walk back from idx, if we find `fn transition_to` or
    `fn try_reset` before leaving the enclosing brace scope, we're inside."""
    depth = 0
    for j in range(idx, -1, -1):
        line = lines[j]
        depth += line.count("}")
        depth -= line.count("{")
        if depth < 0:
            # left the enclosing block
            return False
        if re.search(r"fn (transition_to|try_reset)\b", line):
            return True
    return False


def has_carveout(lines: list[str], idx: int) -> bool:
    """Scan the contiguous comment block immediately above `idx` for a
    carve-out marker. A multi-line `//` comment block counts as a single
    rationale — as long as any line in the block starts with the marker."""
    for j in range(idx - 1, -1, -1):
        stripped = lines[j].strip()
        if not stripped:
            continue
        if not stripped.startswith("//"):
            return False
        if stripped.startswith(CARVE_OUT_PREFIXES):
            return True
    return False


def main() -> int:
    root = Path("src")
    if not root.is_dir():
        print("error: run from repo root (src/ not found)", file=sys.stderr)
        return 2

    violations: list[tuple[str, int, str]] = []
    for path in root.rglob("*.rs"):
        if path.name in REHYDRATION_FILES:
            continue
        try:
            lines = path.read_text().splitlines()
        except OSError:
            continue
        for i, line in enumerate(lines):
            if "==" in line:
                continue
            m = ASSIGN_PATTERN.search(line)
            if not m:
                continue
            stripped = line.strip()
            if stripped.startswith(("//", "*")):
                continue
            if "=>" in line:
                continue
            if is_in_transition_method(lines, i):
                continue
            if has_carveout(lines, i):
                continue
            violations.append((str(path), i + 1, stripped[:120]))

    if not violations:
        print("FSM bypass audit: 0 violations")
        return 0

    print(f"FSM bypass audit: {len(violations)} violation(s)")
    for p, ln, txt in violations:
        print(f"  {p}:{ln}  {txt}")
    print()
    print("Fix options:")
    print("  1. Call <Fsm>::transition_to(next) to go through the validated helper.")
    print("  2. If this is construction or event-log rehydration, add")
    print("     `// fsm-init:` or `// fsm-rebuild:` with rationale on the line above.")
    print("  3. If this needs a new validated semantic, add a method on the FSM")
    print("     type (e.g. SessionState::try_reset) and call it here.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
