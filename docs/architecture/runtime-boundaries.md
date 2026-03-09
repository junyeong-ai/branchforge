# Runtime Boundaries

## Subsystems

- `graph`: session graph domain and event model
- `runtime`: execution loop, tool orchestration, replay metadata
- `prompting`: prompt composition and context assembly
- `provider`: capability profiles, routing, request lowering
- `credentials`: credential sources, vault, refresh policy

## Boundaries

- `runtime` may depend on `graph`, `prompting`, `provider`, and `credentials`
- `prompting` may not depend on `credentials` or provider-specific auth behavior
- `credentials` may not inject persona or mutate prompts
- `provider` may not own session state
- `graph` is pure domain logic and persistence-facing serialization

## Migration Rule

Legacy modules stay only until their responsibilities are absorbed by the subsystem above. Once a boundary is replaced, the legacy implementation is deleted rather than wrapped.
