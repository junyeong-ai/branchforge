//! Schema-aware walker that applies a [`SchemaPolicy`] to a raw JSON
//! schema.
//!
//! The walker is **schema-aware**: it only recurses into children that are
//! themselves JSON schemas (e.g. `properties.*`, `items`, `allOf[]`), never
//! into literal values (`const`, `enum`, `default`, `examples`). This
//! distinction is what makes `const: {"minimum": 5}` preserve its `minimum`
//! key (user data) while `properties.foo.minimum` gets stripped (schema
//! constraint).

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::cycles::compute_cyclic_defs;
use super::keywords::{
    JSON_SCHEMA_META, SCHEMA_VALUED, SCHEMA_VALUED_ARRAY, SCHEMA_VALUED_MAP, is_internal_ref,
};
use super::policy::{MinItemsPolicy, ObjectClosure, RequiredHandling, SchemaPolicy};
use crate::ir::ModelWarning;

/// Maximum recursion depth for the schema walker. Schemas deeper than
/// this return an empty object plus a warning instead of overflowing the
/// stack. Real-world schemas rarely exceed ~20 levels.
const MAX_DEPTH: usize = 500;

/// Stateful walker that transforms a raw schema into the provider-accepted
/// subset described by the [`SchemaPolicy`].
pub(super) struct SchemaWalker<'a> {
    policy: &'a SchemaPolicy,
    /// Caller-supplied prefix for warning field paths (e.g.
    /// `"response_format.schema"` for response-format schemas or
    /// `"tool.calculator.input_schema"` for a tool input schema). The
    /// walker prepends this to every `LossyEncode.field` it emits so
    /// callers can tell which part of the request carried the
    /// offending keyword without ambiguity.
    source_path: &'a str,
    warnings: Vec<ModelWarning>,
    cyclic_defs: HashSet<String>,
}

impl<'a> SchemaWalker<'a> {
    pub(super) fn new(policy: &'a SchemaPolicy, source_path: &'a str, schema: &Value) -> Self {
        let cyclic_defs = if policy.reject_cycles {
            compute_cyclic_defs(schema)
        } else {
            HashSet::new()
        };
        Self {
            policy,
            source_path,
            warnings: Vec::new(),
            cyclic_defs,
        }
    }

    pub(super) fn into_warnings(self) -> Vec<ModelWarning> {
        self.warnings
    }

    /// Visit a schema node, producing a transformed value.
    pub(super) fn visit(&mut self, value: Value, pointer: JsonPointer, depth: usize) -> Value {
        if depth > MAX_DEPTH {
            self.warn(
                &pointer,
                "depth",
                "schema exceeds maximum nesting depth (500)",
            );
            return empty_object();
        }

        // Only object nodes are schemas. Everything else (booleans for
        // boolean schemas, strings, numbers) passes through untouched.
        let Value::Object(mut obj) = value else {
            return value;
        };

        // (1) $ref checks must happen before walking children, because a
        //     rejected sub-tree is replaced wholesale.
        if let Some(ref_val) = obj.get("$ref").and_then(Value::as_str).map(str::to_string) {
            if !is_internal_ref(&ref_val) && self.policy.reject_external_refs {
                self.warn(
                    &pointer,
                    "$ref",
                    "external $ref is not supported by this provider",
                );
                return empty_object();
            }
            if self.cyclic_defs.contains(&ref_val) {
                self.warn(
                    &pointer,
                    "$ref",
                    "recursive schema is not supported by this provider",
                );
                return empty_object();
            }
        }

        // (2) Apply node-level rules to this schema.
        self.apply_node_rules(&mut obj, &pointer);

        // (3) Recurse into schema-valued children only.
        let keys: Vec<String> = obj.keys().cloned().collect();
        for key in keys {
            let child_pointer = pointer.push(&key);
            let child = obj
                .remove(&key)
                .expect("key came from obj.keys() moments ago");
            let visited = self.visit_child(&key, child, child_pointer, depth + 1);
            obj.insert(key, visited);
        }

        Value::Object(obj)
    }

    /// Dispatch a child value to the right visit method based on the
    /// keyword classification from [`super::keywords`].
    fn visit_child(
        &mut self,
        key: &str,
        child: Value,
        pointer: JsonPointer,
        depth: usize,
    ) -> Value {
        if SCHEMA_VALUED_MAP.contains(&key) {
            self.visit_map_of_schemas(child, pointer, depth)
        } else if SCHEMA_VALUED_ARRAY.contains(&key) {
            self.visit_array_of_schemas(child, pointer, depth)
        } else if SCHEMA_VALUED.contains(&key) {
            // `items` may be an array (tuple validation) or a single
            // schema. `additionalProperties` / `contains` may be a
            // boolean. Handle both.
            if key == "items" && child.is_array() {
                self.visit_array_of_schemas(child, pointer, depth)
            } else if child.is_boolean() {
                child
            } else {
                self.visit(child, pointer, depth)
            }
        } else {
            // `const`, `enum`, `default`, `examples`, metadata — preserve
            // the value verbatim. Recursing here would corrupt user data.
            child
        }
    }

    fn visit_map_of_schemas(&mut self, value: Value, pointer: JsonPointer, depth: usize) -> Value {
        let Value::Object(mut map) = value else {
            return value;
        };
        let keys: Vec<String> = map.keys().cloned().collect();
        for k in keys {
            let child_pointer = pointer.push(&k);
            let child = map
                .remove(&k)
                .expect("key came from map.keys() moments ago");
            map.insert(k, self.visit(child, child_pointer, depth));
        }
        Value::Object(map)
    }

    fn visit_array_of_schemas(
        &mut self,
        value: Value,
        pointer: JsonPointer,
        depth: usize,
    ) -> Value {
        let Value::Array(arr) = value else {
            return value;
        };
        let visited: Vec<Value> = arr
            .into_iter()
            .enumerate()
            .map(|(i, v)| self.visit(v, pointer.push_index(i), depth))
            .collect();
        Value::Array(visited)
    }

    /// Apply the policy's node-level transformation rules to `obj`,
    /// emitting warnings for each lossy operation.
    fn apply_node_rules(&mut self, obj: &mut Map<String, Value>, pointer: &JsonPointer) {
        // Strip JSON Schema document metadata unconditionally. These
        // keywords (`$schema`, `$id`, `$comment`, `$anchor`,
        // `$vocabulary`) are dialect markers / identifiers /
        // annotations / vocabulary declarations — never schema
        // constraints — so they never belong on the wire regardless
        // of provider. Stripping here (instead of in each policy's
        // `strip_keywords`) is a root-cause fix: the responsibility
        // lives with the walker because it is a fact about JSON
        // Schema, not about any particular provider.
        for kw in JSON_SCHEMA_META {
            if obj.remove(*kw).is_some() {
                self.warn(
                    pointer,
                    kw,
                    "JSON Schema document metadata keyword is not a schema constraint; stripped",
                );
            }
        }

        // Strip provider-specific unsupported keywords.
        for kw in self.policy.strip_keywords {
            if obj.remove(*kw).is_some() {
                self.warn(
                    pointer,
                    kw,
                    "keyword is not supported by this provider and was stripped",
                );
            }
        }

        // Filter `format` against the allowlist.
        if let Some(allowed) = self.policy.allowed_formats
            && let Some(format) = obj.get("format").and_then(Value::as_str)
            && !allowed.contains(&format)
        {
            let dropped = format.to_string();
            obj.remove("format");
            let reason =
                format!("format '{dropped}' is not in the provider's allowed list; stripped");
            self.warn(pointer, "format", &reason);
        }

        // Apply `minItems` policy.
        self.apply_min_items_policy(obj, pointer);

        // Override `additionalProperties: true` if the policy demands it.
        if self.policy.override_open_objects
            && obj.get("additionalProperties") == Some(&Value::Bool(true))
        {
            obj.insert("additionalProperties".into(), Value::Bool(false));
            self.warn(
                pointer,
                "additionalProperties",
                "additionalProperties: true overridden to false for this provider",
            );
        }

        // Close `type: object` schemas by adding `additionalProperties:
        // false` when missing.
        let is_object_type = obj.get("type") == Some(&Value::String("object".into()));
        if is_object_type
            && self.policy.object_closure == ObjectClosure::Closed
            && !obj.contains_key("additionalProperties")
        {
            obj.insert("additionalProperties".into(), Value::Bool(false));
        }

        // `required` handling.
        if is_object_type && self.policy.required_handling == RequiredHandling::AllProperties {
            self.auto_fill_required(obj, pointer);
        }
    }

    fn apply_min_items_policy(&mut self, obj: &mut Map<String, Value>, pointer: &JsonPointer) {
        let Some(value) = obj.get("minItems") else {
            return;
        };
        let Some(n) = value.as_u64() else {
            return;
        };
        match self.policy.min_items_policy {
            MinItemsPolicy::Strip => {
                obj.remove("minItems");
                self.warn(
                    pointer,
                    "minItems",
                    "minItems is not supported by this provider and was stripped",
                );
            }
            MinItemsPolicy::KeepUpTo(max) => {
                if n > max {
                    obj.remove("minItems");
                    let reason =
                        format!("minItems = {n} exceeds the provider's maximum ({max}); stripped");
                    self.warn(pointer, "minItems", &reason);
                }
            }
            MinItemsPolicy::Keep => {}
        }
    }

    /// Auto-fill `required` with every property name, recording which
    /// fields were strengthened from optional to required so the caller
    /// can surface a meaningful warning.
    fn auto_fill_required(&mut self, obj: &mut Map<String, Value>, pointer: &JsonPointer) {
        let Some(Value::Object(props)) = obj.get("properties") else {
            return;
        };
        let all_keys: Vec<String> = props.keys().cloned().collect();
        if all_keys.is_empty() {
            return;
        }

        let existing: HashSet<String> = obj
            .get("required")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let added: Vec<String> = all_keys
            .iter()
            .filter(|k| !existing.contains(*k))
            .cloned()
            .collect();

        if added.is_empty() {
            return;
        }

        // Preserve the user's existing `required` order (which may be
        // load-bearing for downstream parsers that iterate it) and append
        // the newly-strengthened keys in property declaration order.
        let mut new_required: Vec<Value> = obj
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for k in &added {
            new_required.push(Value::String(k.clone()));
        }
        obj.insert("required".into(), Value::Array(new_required));

        let added_list = added.join(", ");
        let reason = format!(
            "OpenAI strict mode requires all properties in 'required'; \
             strengthened optional fields to required: {added_list}"
        );
        self.warn(pointer, "required", &reason);
    }

    fn warn(&mut self, pointer: &JsonPointer, keyword: &str, reason: &str) {
        self.warnings.push(ModelWarning::lossy(
            format!("{}{}/{keyword}", self.source_path, pointer.as_str()),
            reason.to_string(),
        ));
    }
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// RFC 6901 JSON Pointer used as a path in warning fields.
///
/// Stored as a pre-escaped string to avoid re-escaping on every `push`.
/// Pointer `""` is the root; pointer `"/properties/foo"` names the `foo`
/// property of the root schema's `properties` map. Segments containing
/// `~` or `/` are escaped as `~0` / `~1` per RFC 6901.
#[derive(Clone, Debug, Default)]
pub(super) struct JsonPointer(String);

impl JsonPointer {
    pub(super) fn root() -> Self {
        Self(String::new())
    }

    pub(super) fn push(&self, segment: &str) -> Self {
        let mut s = String::with_capacity(self.0.len() + segment.len() + 1);
        s.push_str(&self.0);
        s.push('/');
        push_escaped(&mut s, segment);
        Self(s)
    }

    pub(super) fn push_index(&self, index: usize) -> Self {
        let mut s = self.0.clone();
        s.push('/');
        s.push_str(&index.to_string());
        Self(s)
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

fn push_escaped(out: &mut String, segment: &str) {
    for ch in segment.chars() {
        match ch {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::prepare_schema;
    use super::*;
    use serde_json::json;

    fn is_lossy_for(warnings: &[ModelWarning], field_contains: &str) -> bool {
        warnings.iter().any(|w| {
            matches!(
                w,
                ModelWarning::LossyEncode { field, .. } if field.contains(field_contains)
            )
        })
    }

    // =============================================================================
    // JsonPointer
    // =============================================================================

    #[test]
    fn json_pointer_root_is_empty_string() {
        assert_eq!(JsonPointer::root().as_str(), "");
    }

    #[test]
    fn json_pointer_push_prefixes_slash() {
        let p = JsonPointer::root().push("properties").push("foo");
        assert_eq!(p.as_str(), "/properties/foo");
    }

    #[test]
    fn json_pointer_push_index_uses_integer_segment() {
        let p = JsonPointer::root().push("allOf").push_index(2);
        assert_eq!(p.as_str(), "/allOf/2");
    }

    #[test]
    fn json_pointer_escapes_special_characters() {
        let p = JsonPointer::root().push("a/b").push("x~y");
        assert_eq!(p.as_str(), "/a~1b/x~0y");
    }

    // =============================================================================
    // Correctness: value-valued keywords are NOT recursed into
    // =============================================================================

    #[test]
    fn const_inner_keywords_are_preserved() {
        let schema = json!({
            "type": "object",
            "properties": {
                "constraint": {
                    "type": "object",
                    "const": {"minimum": 5, "maximum": 10}
                }
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        let const_val = &prepared.value["properties"]["constraint"]["const"];
        assert_eq!(const_val["minimum"], 5);
        assert_eq!(const_val["maximum"], 10);
        assert!(
            !is_lossy_for(&prepared.warnings, "const"),
            "const inner keys must not trigger a warning"
        );
    }

    #[test]
    fn enum_inner_keywords_are_preserved() {
        let schema = json!({
            "enum": [
                {"minimum": 5},
                {"multipleOf": 3}
            ]
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value["enum"][0]["minimum"], 5);
        assert_eq!(prepared.value["enum"][1]["multipleOf"], 3);
    }

    #[test]
    fn default_inner_keywords_are_preserved() {
        let schema = json!({
            "type": "object",
            "default": {"minLength": 7},
            "properties": {"x": {"type": "string", "minLength": 5}}
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        // default is preserved.
        assert_eq!(prepared.value["default"]["minLength"], 7);
        // But properties.x.minLength is stripped (real schema constraint).
        assert!(prepared.value["properties"]["x"].get("minLength").is_none());
    }

    #[test]
    fn examples_are_preserved() {
        let schema = json!({
            "type": "string",
            "examples": [{"maxLength": 100}, {"pattern": "^foo"}]
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value["examples"][0]["maxLength"], 100);
        assert_eq!(prepared.value["examples"][1]["pattern"], "^foo");
    }

    // =============================================================================
    // ObjectClosure::Closed
    // =============================================================================

    #[test]
    fn closed_policy_adds_additional_properties_false() {
        let schema = json!({"type": "object", "properties": {"x": {"type": "string"}}});
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value["additionalProperties"], false);
    }

    #[test]
    fn closed_policy_does_not_override_existing_additional_properties_false() {
        let schema = json!({
            "type": "object",
            "properties": {"x": {"type": "string"}},
            "additionalProperties": false
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value["additionalProperties"], false);
    }

    #[test]
    fn leave_policy_does_not_add_additional_properties() {
        let schema = json!({"type": "object", "properties": {"x": {"type": "string"}}});
        let prepared = prepare_schema(schema, &SchemaPolicy::gemini(), "test");
        assert!(prepared.value.get("additionalProperties").is_none());
    }

    #[test]
    fn override_open_objects_rewrites_true_to_false_with_warning() {
        let schema = json!({
            "type": "object",
            "additionalProperties": true
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value["additionalProperties"], false);
        assert!(is_lossy_for(&prepared.warnings, "additionalProperties"));
    }

    #[test]
    fn gemini_does_not_override_open_objects() {
        let schema = json!({
            "type": "object",
            "additionalProperties": true
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::gemini(), "test");
        assert_eq!(prepared.value["additionalProperties"], true);
    }

    // =============================================================================
    // RequiredHandling::AllProperties (OpenAI strict mode)
    // =============================================================================

    #[test]
    fn openai_strict_auto_fills_required() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}, "b": {"type": "string"}}
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::openai_strict(), "test");
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
    fn openai_strict_auto_fill_emits_warning_listing_strengthened_fields() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
            "required": ["a"]
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::openai_strict(), "test");
        assert!(prepared.warnings.iter().any(|w| matches!(
            w,
            ModelWarning::LossyEncode { field, reason }
            if field.ends_with("/required") && reason.contains("b")
        )));
    }

    #[test]
    fn openai_strict_auto_fill_preserves_existing_required_order() {
        // The user specified ["c", "a"] — properties b and d are optional.
        // After auto-fill, the user's order must be preserved up front,
        // with the newly-strengthened fields appended in property order.
        let schema = json!({
            "type": "object",
            "properties": {
                "a": {"type": "string"},
                "b": {"type": "string"},
                "c": {"type": "string"},
                "d": {"type": "string"}
            },
            "required": ["c", "a"]
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::openai_strict(), "test");
        let req: Vec<&str> = prepared.value["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(req, vec!["c", "a", "b", "d"]);
    }

    #[test]
    fn anthropic_preserves_user_required_without_auto_fill() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
            "required": ["a"]
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        let req: Vec<&str> = prepared.value["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(req, vec!["a"]);
    }

    // =============================================================================
    // strip_keywords
    // =============================================================================

    #[test]
    fn anthropic_strips_numeric_constraints() {
        let schema = json!({
            "type": "object",
            "properties": {
                "age": {"type": "integer", "minimum": 0, "maximum": 120}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        let age = &prepared.value["properties"]["age"];
        assert!(age.get("minimum").is_none());
        assert!(age.get("maximum").is_none());
        assert!(is_lossy_for(&prepared.warnings, "minimum"));
        assert!(is_lossy_for(&prepared.warnings, "maximum"));
    }

    #[test]
    fn anthropic_strips_string_length_constraints() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "minLength": 1, "maxLength": 100}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        let name = &prepared.value["properties"]["name"];
        assert!(name.get("minLength").is_none());
        assert!(name.get("maxLength").is_none());
    }

    #[test]
    fn gemini_preserves_numeric_constraints() {
        let schema = json!({
            "type": "object",
            "properties": {
                "age": {"type": "integer", "minimum": 0, "maximum": 120}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::gemini(), "test");
        let age = &prepared.value["properties"]["age"];
        assert_eq!(age["minimum"], 0);
        assert_eq!(age["maximum"], 120);
    }

    #[test]
    fn gemini_strips_jsonschema_metadata_from_schemars_output() {
        // Regression for a live Vertex Gemini bug: `schemars::schema_for!`
        // emits top-level `$schema`, `$id`, etc. that Gemini's OpenAPI 3.0
        // validator rejects as unknown fields. The walker must strip them.
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$id": "https://example.com/contact.json",
            "$comment": "A schemars-generated schema",
            "title": "Contact",
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer", "minimum": 0}
            },
            "required": ["name", "age"]
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::gemini(), "test");

        // Meta keywords stripped.
        assert!(prepared.value.get("$schema").is_none());
        assert!(prepared.value.get("$id").is_none());
        assert!(prepared.value.get("$comment").is_none());

        // OpenAPI-valid keywords preserved.
        assert_eq!(prepared.value["title"], "Contact");
        assert_eq!(prepared.value["type"], "object");
        assert!(prepared.value["properties"].is_object());
        assert_eq!(prepared.value["properties"]["age"]["minimum"], 0);
    }

    #[test]
    fn anthropic_also_strips_jsonschema_metadata_from_schemars_output() {
        // Anthropic's grammar compiler tolerates `$schema`, but stripping
        // is consistent with Gemini and saves wire bytes. The Anthropic
        // policy inherits the same metadata strip for uniformity.
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Contact",
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert!(prepared.value.get("$schema").is_none());
        assert_eq!(prepared.value["title"], "Contact");
    }

    // =============================================================================
    // allowed_formats
    // =============================================================================

    #[test]
    fn anthropic_strips_non_allowlisted_formats() {
        let schema = json!({
            "type": "object",
            "properties": {
                "age": {"type": "integer", "format": "uint64"}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert!(prepared.value["properties"]["age"].get("format").is_none());
        assert!(is_lossy_for(&prepared.warnings, "format"));
    }

    #[test]
    fn anthropic_keeps_allowlisted_formats() {
        let schema = json!({
            "type": "object",
            "properties": {
                "email": {"type": "string", "format": "email"},
                "website": {"type": "string", "format": "uri"}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value["properties"]["email"]["format"], "email");
        assert_eq!(prepared.value["properties"]["website"]["format"], "uri");
    }

    #[test]
    fn openai_keeps_all_formats_without_allowlist() {
        let schema = json!({
            "type": "object",
            "properties": {
                "age": {"type": "integer", "format": "uint64"}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::openai_strict(), "test");
        assert_eq!(prepared.value["properties"]["age"]["format"], "uint64");
    }

    // =============================================================================
    // MinItemsPolicy
    // =============================================================================

    #[test]
    fn openai_strict_strips_min_items() {
        let schema = json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": 1
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::openai_strict(), "test");
        assert!(prepared.value.get("minItems").is_none());
    }

    #[test]
    fn anthropic_keeps_min_items_zero_and_one() {
        for value in [0, 1] {
            let schema = json!({
                "type": "array",
                "items": {"type": "string"},
                "minItems": value
            });
            let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
            assert_eq!(prepared.value["minItems"], value);
        }
    }

    #[test]
    fn anthropic_strips_min_items_greater_than_one() {
        let schema = json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": 5
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert!(prepared.value.get("minItems").is_none());
        assert!(is_lossy_for(&prepared.warnings, "minItems"));
    }

    #[test]
    fn gemini_keeps_all_min_items_values() {
        let schema = json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": 100
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::gemini(), "test");
        assert_eq!(prepared.value["minItems"], 100);
    }

    // =============================================================================
    // Cycle detection
    // =============================================================================

    #[test]
    fn anthropic_rejects_self_referential_schema() {
        // The recursive definition lives under `$defs.Node`. The root
        // schema references it via a `root` property (not via a
        // root-level `$ref`, which would cause the entire top-level
        // schema to be replaced). The walker should leave the root
        // intact and blank the cyclic `$ref` inside `$defs.Node.properties.next`
        // and inside `properties.root`.
        let schema = json!({
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {"next": {"$ref": "#/$defs/Node"}}
                }
            },
            "type": "object",
            "properties": {
                "root": {"$ref": "#/$defs/Node"}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        // Inside the recursive def, the `next` property's $ref is replaced.
        let next = &prepared.value["$defs"]["Node"]["properties"]["next"];
        assert!(next.as_object().map(|m| m.is_empty()).unwrap_or(false));
        // The `properties.root` sibling is also replaced (it references
        // the cyclic def).
        let root_prop = &prepared.value["properties"]["root"];
        assert!(root_prop.as_object().map(|m| m.is_empty()).unwrap_or(false));
        assert!(is_lossy_for(&prepared.warnings, "$ref"));
    }

    #[test]
    fn anthropic_rejects_indirect_cycle() {
        let schema = json!({
            "$defs": {
                "A": {"properties": {"b": {"$ref": "#/$defs/B"}}},
                "B": {"properties": {"a": {"$ref": "#/$defs/A"}}}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        let a_b = &prepared.value["$defs"]["A"]["properties"]["b"];
        let b_a = &prepared.value["$defs"]["B"]["properties"]["a"];
        assert!(a_b.as_object().map(|m| m.is_empty()).unwrap_or(false));
        assert!(b_a.as_object().map(|m| m.is_empty()).unwrap_or(false));
    }

    #[test]
    fn openai_does_not_reject_cycles() {
        let schema = json!({
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {"next": {"$ref": "#/$defs/Node"}}
                }
            },
            "$ref": "#/$defs/Node"
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::openai_strict(), "test");
        // OpenAI does not enable reject_cycles. The $ref remains.
        let next = &prepared.value["$defs"]["Node"]["properties"]["next"];
        assert_eq!(next["$ref"], "#/$defs/Node");
    }

    // =============================================================================
    // External $ref rejection
    // =============================================================================

    #[test]
    fn anthropic_rejects_external_refs() {
        let schema = json!({
            "type": "object",
            "properties": {
                "other": {"$ref": "http://example.com/other.json"}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        let other = &prepared.value["properties"]["other"];
        assert!(other.as_object().map(|m| m.is_empty()).unwrap_or(false));
        assert!(is_lossy_for(&prepared.warnings, "$ref"));
    }

    #[test]
    fn anthropic_rejects_relative_refs() {
        let schema = json!({
            "type": "object",
            "properties": {
                "other": {"$ref": "other.json"}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        let other = &prepared.value["properties"]["other"];
        assert!(other.as_object().map(|m| m.is_empty()).unwrap_or(false));
    }

    #[test]
    fn anthropic_allows_internal_refs() {
        let schema = json!({
            "$defs": {
                "Name": {"type": "string"}
            },
            "type": "object",
            "properties": {
                "name": {"$ref": "#/$defs/Name"}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value["properties"]["name"]["$ref"], "#/$defs/Name");
    }

    // =============================================================================
    // Recursion depth guard
    // =============================================================================

    #[test]
    fn max_depth_guard_returns_empty_object_with_warning() {
        let mut nested = json!({"type": "string"});
        for _ in 0..600 {
            nested = json!({
                "type": "object",
                "properties": {"next": nested}
            });
        }
        let prepared = prepare_schema(nested, &SchemaPolicy::anthropic(), "test");
        assert!(is_lossy_for(&prepared.warnings, "depth"));
    }

    // =============================================================================
    // allOf / anyOf / oneOf traversal
    // =============================================================================

    #[test]
    fn all_of_entries_are_recursed_as_schemas() {
        let schema = json!({
            "allOf": [
                {"type": "object", "properties": {"x": {"type": "integer", "minimum": 0}}},
                {"type": "object", "properties": {"y": {"type": "integer", "maximum": 100}}}
            ]
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert!(
            prepared.value["allOf"][0]["properties"]["x"]
                .get("minimum")
                .is_none()
        );
        assert!(
            prepared.value["allOf"][1]["properties"]["y"]
                .get("maximum")
                .is_none()
        );
    }

    #[test]
    fn any_of_and_one_of_are_recursed() {
        let schema = json!({
            "anyOf": [
                {"minLength": 5},
                {"maxLength": 10}
            ],
            "oneOf": [
                {"minimum": 0}
            ]
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert!(prepared.value["anyOf"][0].get("minLength").is_none());
        assert!(prepared.value["anyOf"][1].get("maxLength").is_none());
        assert!(prepared.value["oneOf"][0].get("minimum").is_none());
    }

    // =============================================================================
    // items (single schema and tuple form)
    // =============================================================================

    #[test]
    fn items_single_schema_is_recursed() {
        let schema = json!({
            "type": "array",
            "items": {"type": "integer", "minimum": 0}
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert!(prepared.value["items"].get("minimum").is_none());
    }

    #[test]
    fn items_tuple_array_is_recursed() {
        let schema = json!({
            "type": "array",
            "items": [
                {"type": "integer", "minimum": 0},
                {"type": "string", "minLength": 5}
            ]
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert!(prepared.value["items"][0].get("minimum").is_none());
        assert!(prepared.value["items"][1].get("minLength").is_none());
    }

    #[test]
    fn items_boolean_schema_is_preserved() {
        let schema = json!({
            "type": "array",
            "items": false
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value["items"], false);
    }

    // =============================================================================
    // Non-object input
    // =============================================================================

    #[test]
    fn boolean_schema_root_is_preserved() {
        let prepared = prepare_schema(json!(true), &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value, true);
    }

    #[test]
    fn null_schema_is_preserved() {
        let prepared = prepare_schema(json!(null), &SchemaPolicy::anthropic(), "test");
        assert_eq!(prepared.value, json!(null));
    }

    // =============================================================================
    // $defs/definitions traversal
    // =============================================================================

    #[test]
    fn defs_are_recursed_as_schemas() {
        let schema = json!({
            "$defs": {
                "Age": {"type": "integer", "minimum": 0}
            },
            "$ref": "#/$defs/Age"
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert!(prepared.value["$defs"]["Age"].get("minimum").is_none());
    }

    #[test]
    fn legacy_definitions_keyword_is_recursed_as_schemas() {
        let schema = json!({
            "definitions": {
                "Age": {"type": "integer", "minimum": 0}
            }
        });
        let prepared = prepare_schema(schema, &SchemaPolicy::anthropic(), "test");
        assert!(
            prepared.value["definitions"]["Age"]
                .get("minimum")
                .is_none()
        );
    }
}
