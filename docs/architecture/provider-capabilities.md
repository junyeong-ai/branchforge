# Provider Capabilities

Providers are modeled by explicit capability profiles.

## Why

A single generic request type is useful only when the runtime knows which parts are guaranteed, optional, degradable, or unsupported.

## Capability Areas

- Streaming
- Tool calling
- Structured outputs
- Reasoning or thinking controls
- Context management
- Max context window
- Authentication modes
- Rate-limit semantics
- Cost model

## Policy

1. The runtime declares required and preferred capabilities per execution.
2. The gateway lowers a normalized request into a provider request.
3. Missing required capabilities fail fast.
4. Degradable capabilities must be downgraded explicitly and surfaced in execution metadata.
