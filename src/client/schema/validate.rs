//! JSON Schema validator for structured-output response bodies.
//!
//! Native structured-output providers CLAIM to honor the declared
//! schema, but reality is messier: streaming interruptions, degraded
//! models, mis-routed requests, and plain provider bugs all produce
//! responses that do not match the schema. `validate_structured_output`
//! is the post-decode guard that catches these cases before they reach
//! application code.
//!
//! # Full Draft 2020-12 via `jsonschema` crate
//!
//! This module delegates to the `jsonschema` crate, which implements
//! the full JSON Schema Draft 2020-12 specification: `pattern`,
//! `format`, `allOf` / `anyOf` / `oneOf`, `if` / `then` / `else`,
//! numeric constraints (`minimum`, `maximum`, `multipleOf`), length
//! constraints (`minLength`, `maxLength`, `minItems`, `maxItems`),
//! and all the keywords that the previous hand-rolled walker
//! silently skipped.
//!
//! # Where this runs
//!
//! [`crate::client::provider_client::ProviderClient::send`] calls
//! `validate_structured_output` after decoding the response body when
//! the request carried a [`crate::ir::ResponseFormat::JsonSchema`]. On
//! failure:
//!
//! - `spec.strict == true` → fail the request with
//!   [`crate::Error::StructuredOutputInvalid`].
//! - `spec.strict == false` → attach a
//!   [`crate::ir::ModelWarning::LossyEncode`] to the response and
//!   return normally. The caller decides whether to escalate.

use serde_json::Value;

use crate::ir::JsonSchemaSpec;

/// A specific reason a response text failed validation against a
/// [`JsonSchemaSpec`]. Rendered into `Error::StructuredOutputInvalid`
/// or a `ModelWarning` at the call site in `ProviderClient::send`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaValidationError {
    /// The response body was not even a syntactically-valid JSON
    /// document. `reason` is the `serde_json` parser message.
    NotJson { reason: String },

    /// The declared schema itself failed to compile. This is a
    /// developer bug (invalid JSON Schema passed into the IR) rather
    /// than a provider bug. `reason` is the compiler error message.
    InvalidSchema { reason: String },

    /// A JSON Schema constraint was violated at the given JSON pointer
    /// inside the decoded document.
    Constraint {
        /// RFC 6901 JSON pointer into the response document (e.g.
        /// `"/items/0/name"`). Empty string means the root.
        pointer: String,
        /// Human-readable description of the violation.
        reason: String,
    },
}

impl std::fmt::Display for SchemaValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson { reason } => {
                write!(f, "response body is not valid JSON: {reason}")
            }
            Self::InvalidSchema { reason } => {
                write!(f, "declared schema is not a valid JSON Schema: {reason}")
            }
            Self::Constraint { pointer, reason } => {
                let at = if pointer.is_empty() { "root" } else { pointer };
                write!(f, "structured output at {at}: {reason}")
            }
        }
    }
}

impl std::error::Error for SchemaValidationError {}

/// Validate a raw response body string against a [`JsonSchemaSpec`].
///
/// Delegates to `jsonschema::validator_for(&schema)` for full Draft
/// 2020-12 compliance. If the declared schema itself is invalid,
/// returns [`SchemaValidationError::InvalidSchema`].
/// Otherwise, the first reported constraint violation becomes the
/// return value — the validator reports the earliest failure in
/// document order.
pub fn validate_structured_output(
    body: &str,
    spec: &JsonSchemaSpec,
) -> Result<(), SchemaValidationError> {
    let instance: Value =
        serde_json::from_str(body.trim()).map_err(|e| SchemaValidationError::NotJson {
            reason: e.to_string(),
        })?;

    let validator = jsonschema::validator_for(&spec.schema).map_err(|e| {
        SchemaValidationError::InvalidSchema {
            reason: e.to_string(),
        }
    })?;

    if let Err(error) = validator.validate(&instance) {
        return Err(SchemaValidationError::Constraint {
            pointer: error.instance_path().as_str().to_string(),
            reason: error.to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(schema: Value) -> JsonSchemaSpec {
        JsonSchemaSpec {
            schema,
            name: None,
            description: None,
            strict: true,
        }
    }

    #[test]
    fn accepts_valid_object() {
        let s = spec(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "age": {"type": "integer"}},
            "required": ["name", "age"],
            "additionalProperties": false,
        }));
        let body = r#"{"name": "Ada", "age": 30}"#;
        validate_structured_output(body, &s).unwrap();
    }

    #[test]
    fn rejects_non_json() {
        let s = spec(json!({"type": "object"}));
        let err = validate_structured_output("not json at all", &s).unwrap_err();
        assert!(matches!(err, SchemaValidationError::NotJson { .. }));
    }

    #[test]
    fn rejects_wrong_root_type() {
        let s = spec(json!({"type": "object"}));
        let err = validate_structured_output("[1,2,3]", &s).unwrap_err();
        match err {
            SchemaValidationError::Constraint { pointer, .. } => {
                assert_eq!(pointer, "");
            }
            other => panic!("expected Constraint, got {other:?}"),
        }
    }

    #[test]
    fn rejects_missing_required() {
        let s = spec(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "age": {"type": "integer"}},
            "required": ["name", "age"],
        }));
        let err = validate_structured_output(r#"{"name": "Ada"}"#, &s).unwrap_err();
        assert!(matches!(
            err,
            SchemaValidationError::Constraint { ref reason, .. } if reason.contains("age")
        ));
    }

    #[test]
    fn rejects_additional_properties_when_closed() {
        let s = spec(json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "additionalProperties": false,
        }));
        let err = validate_structured_output(r#"{"name": "Ada", "sneaky": 1}"#, &s).unwrap_err();
        assert!(matches!(
            err,
            SchemaValidationError::Constraint { ref reason, .. } if reason.contains("sneaky")
        ));
    }

    #[test]
    fn rejects_property_type_mismatch() {
        let s = spec(json!({
            "type": "object",
            "properties": {"age": {"type": "integer"}},
            "required": ["age"],
        }));
        let err = validate_structured_output(r#"{"age": "thirty"}"#, &s).unwrap_err();
        match err {
            SchemaValidationError::Constraint { pointer, .. } => {
                assert_eq!(pointer, "/age");
            }
            other => panic!("expected Constraint at /age, got {other:?}"),
        }
    }

    #[test]
    fn rejects_array_item_type() {
        let s = spec(json!({
            "type": "array",
            "items": {"type": "integer"}
        }));
        let err = validate_structured_output("[1, 2, \"three\"]", &s).unwrap_err();
        match err {
            SchemaValidationError::Constraint { pointer, .. } => {
                assert_eq!(pointer, "/2");
            }
            other => panic!("expected Constraint at /2, got {other:?}"),
        }
    }

    #[test]
    fn accepts_integer_as_whole_float() {
        // Draft 2020-12 treats `30.0` as an integer because the value is
        // a whole number, matching the behaviour providers expect.
        let s = spec(json!({"type": "integer"}));
        validate_structured_output("30.0", &s).unwrap();
    }

    #[test]
    fn rejects_enum_mismatch() {
        let s = spec(json!({"enum": ["red", "green", "blue"]}));
        let err = validate_structured_output("\"purple\"", &s).unwrap_err();
        assert!(matches!(err, SchemaValidationError::Constraint { .. }));
    }

    #[test]
    fn nested_pointer_attribution() {
        let s = spec(json!({
            "type": "object",
            "properties": {
                "user": {
                    "type": "object",
                    "properties": {
                        "age": {"type": "integer"}
                    },
                    "required": ["age"]
                }
            },
            "required": ["user"]
        }));
        let err = validate_structured_output(r#"{"user": {"age": "oops"}}"#, &s).unwrap_err();
        match err {
            SchemaValidationError::Constraint { pointer, .. } => {
                assert_eq!(pointer, "/user/age");
            }
            other => panic!("expected /user/age, got {other:?}"),
        }
    }

    // ── W-16 new coverage: keywords the hand-rolled walker skipped ─

    /// `pattern` — string regex constraint.
    #[test]
    fn pattern_constraint_enforced() {
        let s = spec(json!({
            "type": "object",
            "properties": {"code": {"type": "string", "pattern": "^[A-Z]{3}$"}},
            "required": ["code"]
        }));
        validate_structured_output(r#"{"code": "ABC"}"#, &s).unwrap();
        let err = validate_structured_output(r#"{"code": "abc"}"#, &s).unwrap_err();
        match err {
            SchemaValidationError::Constraint { pointer, .. } => {
                assert_eq!(pointer, "/code");
            }
            other => panic!("expected Constraint at /code, got {other:?}"),
        }
    }

    /// `oneOf` — exactly one branch must match.
    #[test]
    fn one_of_constraint_enforced() {
        let s = spec(json!({
            "oneOf": [
                {"type": "string"},
                {"type": "integer"}
            ]
        }));
        validate_structured_output("\"hello\"", &s).unwrap();
        validate_structured_output("42", &s).unwrap();
        let err = validate_structured_output("true", &s).unwrap_err();
        assert!(matches!(err, SchemaValidationError::Constraint { .. }));
    }

    /// Numeric `minimum` / `maximum` constraints.
    #[test]
    fn numeric_bounds_enforced() {
        let s = spec(json!({
            "type": "object",
            "properties": {"pct": {"type": "number", "minimum": 0, "maximum": 100}},
            "required": ["pct"]
        }));
        validate_structured_output(r#"{"pct": 50}"#, &s).unwrap();
        let err = validate_structured_output(r#"{"pct": 150}"#, &s).unwrap_err();
        match err {
            SchemaValidationError::Constraint { pointer, .. } => {
                assert_eq!(pointer, "/pct");
            }
            other => panic!("expected /pct, got {other:?}"),
        }
    }

    /// `minLength` / `maxLength` string length constraints.
    #[test]
    fn string_length_constraints_enforced() {
        let s = spec(json!({
            "type": "string",
            "minLength": 3,
            "maxLength": 10
        }));
        validate_structured_output("\"hello\"", &s).unwrap();
        assert!(validate_structured_output("\"hi\"", &s).is_err());
        assert!(validate_structured_output("\"this is too long\"", &s).is_err());
    }

    /// `allOf` — every branch must match.
    #[test]
    fn all_of_constraint_enforced() {
        let s = spec(json!({
            "allOf": [
                {"type": "object", "required": ["a"]},
                {"type": "object", "required": ["b"]}
            ]
        }));
        validate_structured_output(r#"{"a": 1, "b": 2}"#, &s).unwrap();
        assert!(validate_structured_output(r#"{"a": 1}"#, &s).is_err());
    }

    /// Invalid declared schema is surfaced as `InvalidSchema`, not
    /// panicked through.
    #[test]
    fn invalid_schema_is_reported() {
        // `type` must be a string or array of strings, not a number.
        let s = spec(json!({"type": 42}));
        let err = validate_structured_output("{}", &s).unwrap_err();
        assert!(matches!(err, SchemaValidationError::InvalidSchema { .. }));
    }
}
