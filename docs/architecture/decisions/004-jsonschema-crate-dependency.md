# ADR-004: `jsonschema` crate for structured-output validation

**Status:** Accepted  •  **Date:** 2026-04-11

## Context

`client::schema::validate::validate_structured_output` guards the
post-decode response body against the declared `JsonSchemaSpec` for
strict-mode structured output. The original implementation was a
hand-rolled walker that covered a subset of JSON Schema:

- ✅ `type`, `required`, `properties`, `additionalProperties: false`,
  `items` (single-schema form), `enum`, `const`
- ❌ `pattern`, `format`, `allOf` / `anyOf` / `oneOf`,
  `if`/`then`/`else`, numeric constraints (`minimum`, `maximum`,
  `multipleOf`), length constraints (`minLength`, `maxLength`,
  `minItems`, `maxItems`), `$ref` resolution

The unsupported keywords were **silently skipped** — the walker's
conservative policy was "never produce a false positive for a
constraint I do not understand". In practice this meant native
structured-output providers could emit a response that violated
`pattern` / `oneOf` / numeric bounds and the validator would
accept it, letting the failure reach the caller instead of being
caught at the boundary.

## Decision

Delegate to the `jsonschema` crate (version 0.46 at the time of
writing). The replacement is a thin wrapper:

```rust
pub fn validate_structured_output(
    body: &str,
    spec: &JsonSchemaSpec,
) -> Result<(), SchemaValidationError> {
    let instance: Value = serde_json::from_str(body.trim())?;
    let validator = jsonschema::validator_for(&spec.schema)?;
    if let Err(error) = validator.validate(&instance) {
        return Err(SchemaValidationError::Constraint {
            pointer: error.instance_path().as_str().to_string(),
            reason: error.to_string(),
        });
    }
    Ok(())
}
```

`SchemaValidationError` now has three variants: `NotJson`,
`InvalidSchema` (declared schema does not compile — developer bug),
and `Constraint` (instance violates a constraint). The JSON pointer
is sourced from the crate's `error.instance_path()` so pointer
attribution is preserved.

## Consequences

- **Full Draft 2020-12 semantics**: `pattern`, `format`, `oneOf`,
  numeric bounds, length bounds, and reference resolution all work
  out of the box. The contract test matrix gained 5 new regression
  cases proving it.
- **New error variant**: `SchemaValidationError::InvalidSchema`
  distinguishes "caller passed garbage" from "provider returned
  garbage". The public `Error::StructuredOutputInvalid` translates
  it into a user-visible message.
- **Binary-size cost**: ~1 MB of extra Wasm/native code for the
  crate and its transitive `fancy-regex`, `url`, `fraction`
  dependencies. Accepted universally — pure-core users who opt
  into structured output get full validation.
- **Wire error text change**: type-mismatch errors now read
  `"<value>" is not of type "<expected>"` instead of
  `"expected type ... got ..."`. One provider-client regression
  test updated its assertion; no other surface change.

## Alternatives rejected

- **Keep the hand-rolled walker and expand coverage over time.**
  The coverage gap is not ours to close — `jsonschema` is a
  maintained, spec-compliant validator. Rolling our own is pure
  yak-shaving.
- **Put the crate behind a `schema-validation` feature.** Would
  save 1 MB for callers who disable structured-output features
  entirely, at the cost of a silent false-negative path on the
  default build. Rejected — the failure mode is too quiet.
- **Compile-time pre-validation via `schemars`.** Only works for
  schemas derived from Rust types, not for schemas passed as raw
  JSON — which is the most common source for structured output.
