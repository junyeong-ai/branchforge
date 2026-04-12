# axis · tools-naming

## Defect patterns

### Taxonomy violations (against `naming.md`)
- `Manager` without lifecycle (no spawn/shutdown/reconnect)
- `Tracker` that is not a map keyed by dynamic ids
- `Registry` owning sockets or live resources
- `Context` suffix on a mutable shared-state holder
- `State` suffix on a classifier (should be `Status`)
- Closed-list FSM marked `#[non_exhaustive]`, or open-list enum missing it

### Dual systems
- Two registration paths for the same tool category (static `ToolRegistry::new` vs `builder`)
- Two permission sources (`authorization/rules.rs` + `authorization/dsl.rs`)
- Two error classifiers for the same vendor
- Two prompt-assembly paths for system prompt

### MCP / tool plumbing
- `McpToolWrapper` dropping `readOnlyHint` / `destructiveHint` / `idempotentHint` (defaults to destructive)
- Subagent category hardcoded as `match` on an enum instead of a trait
- Hook taking closed `HookEvent` as the only dispatch key (no interest manifest)
- `Skill` and `Plugin` overlapping — one concept named twice
- `Tool::validate_input` default `Ok(())` for non-`SchemaTool` (silent permissive)

### Legacy residue
- `#[allow(dead_code)]` on production code
- `// removed`, `// deprecated`, `// TODO migrate`
- Ghost `pub use` of deleted types
- Example / doctest referencing types that no longer exist

## Rules to load additionally

- `.claude/rules/tools.md`
- `.claude/rules/naming.md`
- `.claude/rules/security.md` (for tool ↔ security coupling)

## Stopping criterion

Every `pub struct`/`pub trait`/`pub enum` matching `Manager|Registry|Tracker|Catalog|Store|Set|Engine|Aggregator|Snapshot|Payload|State|Status|Context|Config|Options|Builder` audited once.
