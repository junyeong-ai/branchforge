# Permissions

Permission policy controls which tools may run and under what conditions.

## Modes

- `BypassPermissions`
- `Plan`
- `AcceptEdits`
- `Default`

## Rule Model

Policies combine:

- mode defaults
- allow rules
- deny rules
- optional tool limits

Deny rules take precedence over allow rules.

## Example

```rust
use claude_agent::{PermissionMode, PermissionPolicy};

let policy = PermissionPolicy::builder()
    .mode(PermissionMode::Default)
    .allow("Read")
    .allow("Bash(git:*)")
    .deny("Write(*.env)")
    .build();
```

## Related Guides

- `security.md`
- `tools.md`
