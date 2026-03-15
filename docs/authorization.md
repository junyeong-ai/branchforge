# Authorization

Authorization policy controls which tools may run and under what conditions.

## Modes

- `allowAll`
- `readOnly`
- `autoApproveFiles`
- `rules`

## Rule Model

Policies combine:

- mode defaults
- allow rules
- deny rules
- optional tool limits

Deny rules take precedence over allow rules.

## Example

```rust
use branchforge::{AuthorizationMode, AuthorizationPolicy};

let policy = AuthorizationPolicy::builder()
    .mode(AuthorizationMode::Rules)
    .allow("Read")
    .allow("Bash(git:*)")
    .deny("Write(*.env)")
    .build();
```

Scoped rules also work for skills:

```rust
use branchforge::{AuthorizationMode, AuthorizationPolicy};

let policy = AuthorizationPolicy::builder()
    .mode(AuthorizationMode::Rules)
    .allow("Skill")
    .deny("Skill(internal)")
    .build();
```

This allows general skill execution while blocking a specific skill name.

## Related Guides

- `security.md`
- `tools.md`
