//! JSON Schema preparation for structured-output codecs.
//!
//! This module is the single source of truth for transforming raw JSON
//! schemas into provider-accepted subsets. Each LLM provider's strict /
//! constrained-decoding endpoint accepts a different JSON Schema subset;
//! [`SchemaPolicy`] captures that subset as a configuration object and
//! [`prepare_schema`] runs a provider-neutral walker to produce a
//! ready-to-send [`PreparedSchema`].
//!
//! # Extensibility
//!
//! Adding support for a new provider is **additive**: define a new `const
//! fn` factory on [`SchemaPolicy`] with the appropriate field values, and
//! the walker handles it with no core code changes. This is the
//! Open-Closed Principle implemented as data rather than branches.
//!
//! # Example
//!
//! ```
//! use branchforge::client::schema::{prepare_schema, SchemaPolicy};
//! use serde_json::json;
//!
//! let raw = json!({
//!     "type": "object",
//!     "properties": {
//!         "name": {"type": "string"},
//!         "age": {"type": "integer", "minimum": 0}
//!     },
//!     "required": ["name", "age"]
//! });
//!
//! let prepared = prepare_schema(raw, &SchemaPolicy::anthropic(), "test");
//! // `minimum: 0` was stripped (Anthropic does not support numeric constraints)
//! // and a lossy warning was emitted.
//! assert!(!prepared.warnings.is_empty());
//! ```

mod cycles;
mod keywords;
mod policy;
mod validate;
mod walker;

pub use policy::{MinItemsPolicy, ObjectClosure, RequiredHandling, SchemaPolicy};
pub use validate::{SchemaValidationError, validate_structured_output};

use walker::{JsonPointer, SchemaWalker};

use crate::ir::{JsonSchemaSpec, ModelWarning};

/// The output of [`prepare_schema`].
///
/// `value` is the rewritten schema ready for the wire. `warnings` lists
/// every keyword the walker stripped, overrode, or rejected, as
/// [`ModelWarning::LossyEncode`] entries. Codecs propagate these warnings
/// into the final `ModelResponse::warnings` so callers can see exactly
/// what was dropped.
#[derive(Clone, Debug)]
pub struct PreparedSchema {
    /// The transformed schema, ready to embed in a wire request body.
    pub value: serde_json::Value,
    /// Non-fatal transformations the walker performed.
    pub warnings: Vec<ModelWarning>,
}

impl PreparedSchema {
    /// Construct a passthrough `PreparedSchema` that carries the given
    /// value unchanged with no warnings. Codecs use this on the
    /// non-strict path where the user has explicitly opted out of
    /// policy application (for example, OpenAI's `"strict": false`
    /// mode on `response_format.json_schema`, which disables the
    /// grammar-constrained decoder entirely).
    pub fn passthrough(value: serde_json::Value) -> Self {
        Self {
            value,
            warnings: Vec::new(),
        }
    }

    /// Consume the prepared schema and return just the `value`, dropping
    /// any warnings. Convenient when the caller does not want to surface
    /// lossy-encode telemetry (for example, in a quick script).
    pub fn into_value(self) -> serde_json::Value {
        self.value
    }

    /// `true` if the walker made no lossy transformations (zero warnings).
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }
}

/// Transform a raw JSON schema into the subset accepted by the given
/// [`SchemaPolicy`].
///
/// The walker recursively visits only schema-valued children
/// (`properties.*`, `items`, `allOf[]`, …) — literal values like `const`,
/// `enum`, `default`, and `examples` are preserved verbatim even if they
/// contain keywords that would be stripped in a schema context.
///
/// Cycles in the `$ref` graph and external `$ref` targets are replaced
/// with empty-object sub-trees when the policy requests it, each
/// accompanied by a warning so the caller can tell what happened.
///
/// `source_path` is the caller's context prefix for warning attribution —
/// every `LossyEncode.field` emitted by the walker is prepended with this
/// string. Conventional values:
///
/// - `"response_format.schema"` for `ResponseFormat::JsonSchema` encoding
/// - `"tool.<name>.input_schema"` for `ToolDefinition.parameters` encoding
///   (built automatically by [`prepare_tool_schema`])
///
/// Supplying the path here — rather than hardcoding it in the walker —
/// is what lets the same pipeline serve both response_format and tool
/// schemas without misattributing warnings.
pub fn prepare_schema(
    schema: serde_json::Value,
    policy: &SchemaPolicy,
    source_path: &str,
) -> PreparedSchema {
    let mut walker = SchemaWalker::new(policy, source_path, &schema);
    let value = walker.visit(schema, JsonPointer::root(), 0);
    let warnings = walker.into_warnings();
    PreparedSchema { value, warnings }
}

/// Derive a JSON schema from a Rust type `T` via `schemars` and prepare
/// it for the given [`SchemaPolicy`].
///
/// This is the recommended entry point for applications that use
/// `#[derive(schemars::JsonSchema)]` on their domain types. Warnings
/// are attributed to `"response_format.schema"` — use
/// [`prepare_schema`] directly if you need a different source path.
pub fn schema_for<T: schemars::JsonSchema>(policy: &SchemaPolicy) -> PreparedSchema {
    let raw = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default();
    prepare_schema(raw, policy, "response_format.schema")
}

/// Prepare a tool's `input_schema` for wire transmission.
///
/// Tools have a different policy contract from response_format schemas:
///
/// - When `is_strict` is `true` (OpenAI and Anthropic strict tool use),
///   apply the codec's full strict `SCHEMA_POLICY` — the provider's
///   grammar compiler or strict validator will enforce the same subset
///   as response_format.
/// - When `is_strict` is `false`, apply [`SchemaPolicy::lenient`] —
///   walker-level metadata strip, cycle detection, external-`$ref`
///   rejection, and nothing else. User-declared constraints like
///   `"minimum": 0` are preserved as documentation.
///
/// Codecs that have no strict/non-strict distinction for tools
/// (Gemini, Bedrock Converse) should pass `is_strict: false`
/// unconditionally — their underlying validator is already accounted
/// for by the walker-level strip.
///
/// The lenient path is what fixes the **Gemini `$schema` bug**:
/// `schemars::schema_for!(T)` emits `$schema` at the top of every
/// tool schema, and Gemini's OpenAPI 3.0 validator rejects it with a
/// 400 — the walker's unconditional metadata strip catches this
/// universally.
///
/// `tool_name` is used to build the walker's source path
/// (`"tool.<name>.input_schema"`) so lossy warnings can be
/// disambiguated from response_format warnings and from warnings on
/// sibling tools in the same request.
pub fn prepare_tool_schema(
    schema: serde_json::Value,
    codec_strict_policy: &SchemaPolicy,
    is_strict: bool,
    tool_name: &str,
) -> PreparedSchema {
    let source_path = format!("tool.{tool_name}.input_schema");
    if is_strict {
        prepare_schema(schema, codec_strict_policy, &source_path)
    } else {
        // `static` lives for the process lifetime; re-used across every
        // non-strict tool call. No runtime allocation.
        static LENIENT: SchemaPolicy = SchemaPolicy::lenient();
        prepare_schema(schema, &LENIENT, &source_path)
    }
}

/// Emit `LossyEncode` warnings for any `JsonSchemaSpec` metadata fields
/// the current [`SchemaPolicy`] says the wire format does **not**
/// support.
///
/// Single source of truth: each policy declares
/// [`SchemaPolicy::wire_supports_name`] /
/// [`SchemaPolicy::wire_supports_description`], and every codec that
/// emits a structured-output envelope runs this function to surface
/// dropped metadata uniformly. Replaces the per-codec
/// `warn_dropped_<provider>_metadata` helpers.
///
/// `codec_id` is used to build a helpful reason string pointing at the
/// specific provider.
pub fn warn_dropped_metadata(
    spec: &JsonSchemaSpec,
    policy: &SchemaPolicy,
    codec_id: &str,
    warnings: &mut Vec<ModelWarning>,
) {
    if !policy.wire_supports_name && spec.name.is_some() {
        warnings.push(ModelWarning::lossy(
            "response_format.name",
            format!("{codec_id} wire format has no schema name field; dropped"),
        ));
    }
    if !policy.wire_supports_description && spec.description.is_some() {
        warnings.push(ModelWarning::lossy(
            "response_format.description",
            format!("{codec_id} wire format has no schema description field; dropped"),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prepare_schema_smoke_anthropic() {
        let raw = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer", "minimum": 0}
            },
            "required": ["name", "age"]
        });
        let prepared = prepare_schema(raw, &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value["additionalProperties"], false);
        assert!(prepared.value["properties"]["age"].get("minimum").is_none());
        assert!(!prepared.is_clean());
    }

    #[test]
    fn prepare_schema_smoke_openai_strict() {
        let raw = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}, "b": {"type": "integer"}}
        });
        let prepared = prepare_schema(raw, &SchemaPolicy::openai_strict(), "test");
        assert_eq!(prepared.value["additionalProperties"], false);
        let req: Vec<&str> = prepared.value["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(req.contains(&"a"));
        assert!(req.contains(&"b"));
    }

    #[test]
    fn prepare_schema_smoke_gemini_preserves_user_constraints() {
        let raw = json!({
            "type": "object",
            "properties": {"x": {"type": "integer", "minimum": 0, "maximum": 100}}
        });
        let prepared = prepare_schema(raw, &SchemaPolicy::gemini(), "test");
        // Gemini keeps numeric constraints.
        assert_eq!(prepared.value["properties"]["x"]["minimum"], 0);
        assert_eq!(prepared.value["properties"]["x"]["maximum"], 100);
        assert!(prepared.is_clean());
    }

    #[test]
    fn prepare_schema_smoke_bedrock_closes_objects() {
        let raw = json!({
            "type": "object",
            "properties": {"x": {"type": "string"}}
        });
        let prepared = prepare_schema(raw, &SchemaPolicy::bedrock_converse(), "test");
        assert_eq!(prepared.value["additionalProperties"], false);
        assert!(prepared.is_clean());
    }

    #[test]
    fn schema_for_rust_type_via_schemars_roundtrips_through_anthropic() {
        use schemars::JsonSchema;

        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct Person {
            name: String,
            email: String,
            age: u64,
        }

        let prepared = schema_for::<Person>(&SchemaPolicy::anthropic());
        // additionalProperties: false was added.
        assert_eq!(prepared.value["additionalProperties"], false);
        // schemars generates `minimum: 0` and `format: "uint64"` for u64 —
        // both should be stripped by the Anthropic policy.
        let age = &prepared.value["properties"]["age"];
        assert!(age.get("minimum").is_none());
        assert!(age.get("format").is_none());
        // Warnings should mention both.
        let has_minimum_warning = prepared.warnings.iter().any(|w| {
            matches!(
                w,
                ModelWarning::LossyEncode { field, .. } if field.contains("minimum")
            )
        });
        let has_format_warning = prepared.warnings.iter().any(|w| {
            matches!(
                w,
                ModelWarning::LossyEncode { field, .. } if field.contains("format")
            )
        });
        assert!(has_minimum_warning);
        assert!(has_format_warning);
    }

    #[test]
    fn prepared_schema_into_value_discards_warnings() {
        let raw = json!({"type": "object", "properties": {"x": {"type": "integer", "minimum": 0}}});
        let value = prepare_schema(raw, &SchemaPolicy::anthropic(), "test").into_value();
        assert_eq!(value["additionalProperties"], false);
    }

    #[test]
    fn prepared_schema_is_clean_when_no_warnings() {
        let raw = json!({"type": "string"});
        let prepared = prepare_schema(raw, &SchemaPolicy::anthropic(), "test");
        assert!(prepared.is_clean());
    }

    #[test]
    fn walker_strips_jsonschema_meta_across_every_policy() {
        // Root-cause test: JSON Schema document metadata is stripped
        // at the walker level, regardless of which policy is active.
        // Every policy should produce output without `$schema`, `$id`,
        // `$comment`, `$anchor`, `$vocabulary` when those appear in
        // the input.
        let raw = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "https://example.com/contact.json",
            "$comment": "A schemars-generated schema",
            "$anchor": "root",
            "$vocabulary": {
                "https://json-schema.org/draft/2020-12/vocab/core": true
            },
            "type": "object",
            "properties": {"name": {"type": "string"}}
        });

        for policy in [
            SchemaPolicy::openai_strict(),
            SchemaPolicy::anthropic(),
            SchemaPolicy::gemini(),
            SchemaPolicy::bedrock_converse(),
            SchemaPolicy::lenient(),
        ] {
            let prepared = prepare_schema(raw.clone(), &policy, "test");
            assert!(prepared.value.get("$schema").is_none());
            assert!(prepared.value.get("$id").is_none());
            assert!(prepared.value.get("$comment").is_none());
            assert!(prepared.value.get("$anchor").is_none());
            assert!(prepared.value.get("$vocabulary").is_none());
            // Structural fields preserved.
            assert_eq!(prepared.value["type"], "object");
        }
    }

    #[test]
    fn walker_preserves_schema_structure_keywords() {
        // Intentional non-strip: `$ref`, `$defs`, `definitions` are
        // schema structure, not metadata. They must NOT be in
        // JSON_SCHEMA_META — verified here by asserting they survive
        // the walker on every policy.
        let raw = json!({
            "$defs": {
                "Name": {"type": "string"}
            },
            "type": "object",
            "properties": {
                "name": {"$ref": "#/$defs/Name"}
            }
        });
        for policy in [
            SchemaPolicy::openai_strict(),
            SchemaPolicy::anthropic(),
            SchemaPolicy::gemini(),
            SchemaPolicy::bedrock_converse(),
            SchemaPolicy::lenient(),
        ] {
            let prepared = prepare_schema(raw.clone(), &policy, "test");
            assert!(prepared.value.get("$defs").is_some());
            assert_eq!(prepared.value["properties"]["name"]["$ref"], "#/$defs/Name");
        }
    }

    #[test]
    fn warn_dropped_metadata_emits_for_unsupported_name() {
        use crate::ir::{JsonSchemaSpec, ModelWarning};
        let spec = JsonSchemaSpec::new(json!({"type": "object"}))
            .with_name("Person")
            .with_description("A person record");
        let policy = SchemaPolicy::anthropic();
        let mut warnings = Vec::new();

        warn_dropped_metadata(&spec, &policy, "anthropic-messages", &mut warnings);

        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ModelWarning::LossyEncode { field, reason }
            if field == "response_format.name" && reason.contains("anthropic-messages")
        )));
        assert!(warnings.iter().any(|w| matches!(
            w,
            ModelWarning::LossyEncode { field, reason }
            if field == "response_format.description" && reason.contains("anthropic-messages")
        )));
    }

    #[test]
    fn warn_dropped_metadata_is_silent_for_supported_wire() {
        use crate::ir::JsonSchemaSpec;
        let spec = JsonSchemaSpec::new(json!({"type": "object"}))
            .with_name("Person")
            .with_description("A record");
        // OpenAI strict supports both name and description on the wire.
        let policy = SchemaPolicy::openai_strict();
        let mut warnings = Vec::new();

        warn_dropped_metadata(&spec, &policy, "openai-chat", &mut warnings);

        assert!(warnings.is_empty());
    }

    #[test]
    fn warn_dropped_metadata_emits_only_for_fields_actually_set() {
        use crate::ir::{JsonSchemaSpec, ModelWarning};
        // Only `name` is set — only one warning should be emitted.
        let spec = JsonSchemaSpec::new(json!({"type": "object"})).with_name("Person");
        let policy = SchemaPolicy::anthropic();
        let mut warnings = Vec::new();

        warn_dropped_metadata(&spec, &policy, "anthropic-messages", &mut warnings);

        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            warnings[0],
            ModelWarning::LossyEncode { ref field, .. }
            if field == "response_format.name"
        ));
    }
}
