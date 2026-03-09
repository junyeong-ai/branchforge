# Skills

Skills are reusable workflow instructions loaded from project or user resources.

## Where Skills Come From

- project-level `.claude/skills/`
- user-level `~/.claude/skills/`

## Supported Forms

- single markdown file
- directory-based skill with `SKILL.md`

## What Skills Can Define

- name and description
- tool restrictions
- triggers
- model override
- optional command-style arguments

## Example

```markdown
---
name: deploy
description: Deployment workflow
allowed-tools:
  - Bash
  - Read
---

Deploy to $ARGUMENTS.
```

## Related Concepts

- skills provide reusable workflow context
- subagents provide isolated execution contexts

## Related Guides

- `memory-system.md`
- `subagents.md`
