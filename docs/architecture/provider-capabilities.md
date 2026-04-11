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

## Structured outputs support matrix (as of 0.9.0)

All five codecs ship native structured output support through a shared
`SchemaPolicy` infrastructure (`src/client/schema/`). The public API is
`SchemaPolicy`, `PreparedSchema`, `prepare_schema`, and `schema_for<T>`;
each codec declares a `const SCHEMA_POLICY: SchemaPolicy = SchemaPolicy::X()`
next to its other constants. See the rustdoc on `SchemaPolicy::anthropic()`
for the full Anthropic subset specification and the `CHANGELOG.md` 0.9.0
entry for the design rationale.

| Codec              | `json_schema` | `json_object` | `strict` | Wire field |
| :----------------- | :-----------: | :-----------: | :------: | :--------- |
| anthropic-messages | Native        | Emulated      | true     | `output_config.format.schema` |
| openai-chat        | Native        | Native        | true     | `response_format.json_schema.schema` |
| openai-responses   | Native        | Native        | true     | `text.format.schema` |
| gemini-generate    | Native        | Native        | false    | `generationConfig.responseSchema` |
| bedrock-converse   | Native        | Emulated      | true     | `outputConfig.textFormat.structure.jsonSchema.schema` (string-encoded) |

Capability honesty is enforced by
`tests/codec_contract.rs::capability_honesty_response_format` which
verifies every codec either emits a wire-level schema reference (Native)
or surfaces a `response_format*` CapabilityEmulated warning (Emulated).
