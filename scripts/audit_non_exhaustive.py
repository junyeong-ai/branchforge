#!/usr/bin/env python3
"""
`#[non_exhaustive]` hygiene audit (Phase 0-2 + Phase 0-7 gate).

Walks every `pub enum` under `src/` and checks it against the closed-list
declared in `.claude/rules/naming.md`:

- **Closed list** (FSM transition graphs, mathematical binaries, SSoT
  design commitments) — MUST NOT be marked `#[non_exhaustive]`. Exhaustive
  matches on the consumer side are the point.
- **Everything else** — MUST be marked `#[non_exhaustive]` so new variants
  never silently break downstream exhaustive matches.

Exits non-zero with a report when the audit finds a closed-list enum
that gained the attribute or an open-list enum that is missing it.
"""
from __future__ import annotations

import os
import re
import sys
from pathlib import Path

# Closed-list enums per `.claude/rules/naming.md`. Updating this list
# requires an accompanying edit to naming.md and a reviewer sign-off.
CLOSED_LIST: set[str] = {
    "SessionState",
    "PlanState",
    "QueueItemState",
    "McpClientState",
    "AgentState",
    "SchemaVersionMismatchDirection",
    "Role",
}


def audit() -> tuple[list[tuple[str, str, str]], int]:
    violations: list[tuple[str, str, str]] = []
    total = 0
    for root, _, files in os.walk("src"):
        for fname in files:
            if not fname.endswith(".rs"):
                continue
            path = Path(root) / fname
            try:
                lines = path.read_text().splitlines()
            except OSError:
                continue
            for i, line in enumerate(lines):
                m = re.match(r"\s*pub enum (\w+)", line)
                if not m:
                    continue
                name = m.group(1)
                total += 1
                # Look back up to 10 lines for the attribute.
                has_non_exhaustive = False
                for j in range(max(0, i - 10), i):
                    if "#[non_exhaustive]" in lines[j]:
                        has_non_exhaustive = True
                        break
                loc = f"{path}:{i + 1}"
                if name in CLOSED_LIST:
                    if has_non_exhaustive:
                        violations.append(
                            (
                                name,
                                loc,
                                "closed-list enum has #[non_exhaustive] "
                                "— closed enums must NOT carry the attribute",
                            )
                        )
                else:
                    if not has_non_exhaustive:
                        violations.append(
                            (
                                name,
                                loc,
                                "open-list enum missing #[non_exhaustive]",
                            )
                        )
    return violations, total


def main() -> int:
    if not Path("src").is_dir():
        print("error: run from repo root (src/ not found)", file=sys.stderr)
        return 2
    violations, total = audit()
    if not violations:
        print(f"non_exhaustive audit: {total} pub enums, 0 violations")
        return 0
    print(f"non_exhaustive audit: {total} pub enums, {len(violations)} violation(s)")
    for name, loc, msg in violations:
        print(f"  {loc}  {name}: {msg}")
    print()
    print("Fix options:")
    print("  1. Closed-list violation: remove #[non_exhaustive] and verify the")
    print("     enum genuinely belongs to the closed list in .claude/rules/naming.md.")
    print("  2. Open-list violation: add #[non_exhaustive] above the enum.")
    print("  3. If this is a new closed-list member, update CLOSED_LIST above")
    print("     AND add it to naming.md in the same PR.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
