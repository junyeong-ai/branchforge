# Memory System

The memory system loads project guidance from resource files such as `CLAUDE.md`, rules, and skills.

## Resource Levels

- enterprise
- user
- project
- local

Later levels override earlier levels.

## Core Inputs

- `CLAUDE.md`
- `CLAUDE.local.md`
- `.claude/rules/`
- `.claude/skills/`
- `.claude/commands/`

## Imports

`@import` can be used inside memory documents to include other files.

The loader handles:

- relative imports
- home-relative imports
- depth limits
- circular import protection

## Related Guides

- `session.md`
- `skills.md`
- `architecture.md`
