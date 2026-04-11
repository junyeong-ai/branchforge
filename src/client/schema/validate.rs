//! Minimal JSON Schema validator for structured-output response bodies.
//!
//! Native structured-output providers CLAIM to honor the declared
//! schema, but reality is messier: streaming interruptions, degraded
//! models, mis-routed requests, and plain provider bugs all produce
//! responses that do not match the schema. `validate_structured_output`
//! is the post-decode guard that catches these cases before they reach
//! application code.
//!
//! # Philosophy: minimal and conservative
//!
//! This is **not** a full JSON Schema validator. It intentionally
//! covers the constraint subset that matters in practice for
//! structured outputs:
//!
//! - JSON parseability
//! - `type` (string / number / integer / boolean / object / array / null,
//!   including `type` arrays)
//! - `required` on objects
//! - `properties` (recursive)
//! - `additionalProperties: false`
//! - `items` (single schema — tuple `prefixItems` is conservatively
//!   treated as unconstrained)
//! - `enum` (exact deep equality)
//!
//! Anything not in that list is silently **skipped** — the validator
//! never produces a false positive for a constraint it does not
//! understand. The goal is to detect plainly-wrong shapes, not to
//! re-implement Draft 2020-12.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuredOutputValidationError {
    /// The response body was not even a syntactically-valid JSON
    /// document. `reason` is the `serde_json` parser message.
    NotJson { reason: String },

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

impl std::fmt::Display for StructuredOutputValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson { reason } => {
                write!(f, "response body is not valid JSON: {reason}")
            }
            Self::Constraint { pointer, reason } => {
                let at = if pointer.is_empty() { "root" } else { pointer };
                write!(f, "structured output at {at}: {reason}")
            }
        }
    }
}

impl std::error::Error for StructuredOutputValidationError {}

/// Validate a raw response body string against a [`JsonSchemaSpec`].
///
/// See the module docs for the supported constraint subset and
/// conservative-skip policy.
pub fn validate_structured_output(
    body: &str,
    spec: &JsonSchemaSpec,
) -> Result<(), StructuredOutputValidationError> {
    let value: Value = serde_json::from_str(body.trim()).map_err(|e| {
        StructuredOutputValidationError::NotJson {
            reason: e.to_string(),
        }
    })?;
    validate_value(&value, &spec.schema, "")
}

fn validate_value(
    value: &Value,
    schema: &Value,
    pointer: &str,
) -> Result<(), StructuredOutputValidationError> {
    let schema_obj = match schema.as_object() {
        Some(o) => o,
        // Non-object schemas (e.g. the literal `true`) impose no
        // constraints. The literal `false` rejects everything, but
        // that is a degenerate case we do not expect from
        // structured-output providers.
        None => return Ok(()),
    };

    // `enum` — exact deep equality against one of the listed values.
    if let Some(Value::Array(variants)) = schema_obj.get("enum")
        && !variants.iter().any(|v| v == value)
    {
        return Err(StructuredOutputValidationError::Constraint {
            pointer: pointer.to_string(),
            reason: format!(
                "value does not match any enum variant ({} choices)",
                variants.len()
            ),
        });
    }

    // `const` — exact deep equality against a single value.
    if let Some(expected) = schema_obj.get("const")
        && expected != value
    {
        return Err(StructuredOutputValidationError::Constraint {
            pointer: pointer.to_string(),
            reason: "value does not match const".into(),
        });
    }

    // `type` — string or array of strings. Any one match satisfies.
    if let Some(type_field) = schema_obj.get("type") {
        let matched = match type_field {
            Value::String(t) => matches_type(value, t),
            Value::Array(ts) => ts
                .iter()
                .filter_map(|t| t.as_str())
                .any(|t| matches_type(value, t)),
            _ => true, // malformed schema — skip
        };
        if !matched {
            return Err(StructuredOutputValidationError::Constraint {
                pointer: pointer.to_string(),
                reason: format!(
                    "expected type {}, got {}",
                    type_field,
                    json_type_name(value)
                ),
            });
        }
    }

    // Object constraints.
    if let Some(obj) = value.as_object() {
        // `required`
        if let Some(Value::Array(reqs)) = schema_obj.get("required") {
            for req in reqs.iter().filter_map(|v| v.as_str()) {
                if !obj.contains_key(req) {
                    return Err(StructuredOutputValidationError::Constraint {
                        pointer: pointer.to_string(),
                        reason: format!("missing required property `{req}`"),
                    });
                }
            }
        }

        // `properties` — recurse into declared children.
        let properties = schema_obj.get("properties").and_then(Value::as_object);
        if let Some(props) = properties {
            for (name, child_schema) in props {
                if let Some(child) = obj.get(name) {
                    let child_pointer = format!("{pointer}/{}", escape_ptr(name));
                    validate_value(child, child_schema, &child_pointer)?;
                }
            }
        }

        // `additionalProperties: false` — reject unknown keys. A
        // sub-schema value (object) is conservatively skipped here;
        // structured-output providers almost always use the bool form.
        if let Some(Value::Bool(false)) = schema_obj.get("additionalProperties") {
            let declared = properties.map(|p| p.keys().collect::<std::collections::HashSet<_>>());
            if let Some(declared) = declared {
                for key in obj.keys() {
                    if !declared.contains(key) {
                        return Err(StructuredOutputValidationError::Constraint {
                            pointer: pointer.to_string(),
                            reason: format!("unexpected additional property `{key}`"),
                        });
                    }
                }
            } else {
                // No declared properties at all — every key is extra.
                if let Some(key) = obj.keys().next() {
                    return Err(StructuredOutputValidationError::Constraint {
                        pointer: pointer.to_string(),
                        reason: format!("unexpected additional property `{key}`"),
                    });
                }
            }
        }
    }

    // Array `items` — single schema form only. Tuple form
    // (`prefixItems`) is conservatively skipped.
    if let Some(arr) = value.as_array()
        && let Some(items_schema) = schema_obj.get("items")
        && items_schema.is_object()
    {
        for (i, item) in arr.iter().enumerate() {
            let child_pointer = format!("{pointer}/{i}");
            validate_value(item, items_schema, &child_pointer)?;
        }
    }

    Ok(())
}

fn matches_type(value: &Value, ty: &str) -> bool {
    match ty {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => true, // unknown type keyword — conservative skip
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// RFC 6901 token escaping for JSON pointer construction.
fn escape_ptr(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
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
        assert!(matches!(
            err,
            StructuredOutputValidationError::NotJson { .. }
        ));
    }

    #[test]
    fn rejects_wrong_root_type() {
        let s = spec(json!({"type": "object"}));
        let err = validate_structured_output("[1,2,3]", &s).unwrap_err();
        match err {
            StructuredOutputValidationError::Constraint { pointer, reason } => {
                assert_eq!(pointer, "");
                assert!(reason.contains("expected type"));
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
            StructuredOutputValidationError::Constraint { ref reason, .. } if reason.contains("age")
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
            StructuredOutputValidationError::Constraint { ref reason, .. } if reason.contains("sneaky")
        ));
    }

    #[test]
    fn rejects_property_type_mismatch() {
        let s = spec(json!({
            "type": "object",
            "properties": {"age": {"type": "integer"}},
        }));
        let err = validate_structured_output(r#"{"age": "thirty"}"#, &s).unwrap_err();
        match err {
            StructuredOutputValidationError::Constraint { pointer, .. } => {
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
            StructuredOutputValidationError::Constraint { pointer, .. } => {
                assert_eq!(pointer, "/2");
            }
            other => panic!("expected Constraint at /2, got {other:?}"),
        }
    }

    #[test]
    fn accepts_integer_as_whole_float() {
        // Providers sometimes emit `30.0` for integer fields; as long
        // as the value is whole, treat it as an integer.
        let s = spec(json!({"type": "integer"}));
        validate_structured_output("30.0", &s).unwrap();
    }

    #[test]
    fn rejects_enum_mismatch() {
        let s = spec(json!({"enum": ["red", "green", "blue"]}));
        let err = validate_structured_output("\"purple\"", &s).unwrap_err();
        assert!(matches!(
            err,
            StructuredOutputValidationError::Constraint { .. }
        ));
    }

    #[test]
    fn skips_unknown_keywords_conservatively() {
        // `pattern`, `format`, `allOf` etc. are not implemented — the
        // validator must not error on schemas that use them.
        let s = spec(json!({
            "type": "string",
            "pattern": "^[A-Z]+$",
            "format": "uuid",
            "allOf": [{"type": "string"}],
        }));
        validate_structured_output("\"whatever\"", &s).unwrap();
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
                    }
                }
            }
        }));
        let err = validate_structured_output(r#"{"user": {"age": "oops"}}"#, &s).unwrap_err();
        match err {
            StructuredOutputValidationError::Constraint { pointer, .. } => {
                assert_eq!(pointer, "/user/age");
            }
            other => panic!("expected /user/age, got {other:?}"),
        }
    }

    #[test]
    fn escapes_json_pointer_special_chars() {
        let s = spec(json!({
            "type": "object",
            "properties": {
                "a/b": {"type": "integer"}
            }
        }));
        let err = validate_structured_output(r#"{"a/b": "no"}"#, &s).unwrap_err();
        match err {
            StructuredOutputValidationError::Constraint { pointer, .. } => {
                assert_eq!(pointer, "/a~1b");
            }
            other => panic!("expected escaped pointer, got {other:?}"),
        }
    }
}
