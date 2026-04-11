//! JSON Schema keyword classification for the walker.
//!
//! The walker must distinguish between **schema-valued children** (values
//! that are themselves JSON schemas and should be recursively transformed)
//! and **value-valued children** (user-data literals like `const`, `enum`,
//! `default` that must be preserved verbatim). Naively recursing into every
//! child would incorrectly treat the value of `const: {"minimum": 5}` as a
//! schema and strip the `minimum` keyword as if it were a constraint.
//!
//! This module is the single source of truth for that classification. The
//! lists are defined once here and consumed by
//! [`super::walker::SchemaWalker`].
//!
//! It also owns [`JSON_SCHEMA_META`] — the set of JSON Schema **document
//! metadata** keywords that are never schema constraints and therefore
//! must never reach a structured-output API. The walker strips these
//! unconditionally before applying any provider-specific rules.

/// Keywords whose value is a single nested schema.
///
/// `additionalProperties`, `additionalItems`, `items`, `contains`, and
/// `propertyNames` may also take a boolean value — the walker handles that
/// case inline.
pub(super) const SCHEMA_VALUED: &[&str] = &[
    "items",
    "additionalProperties",
    "additionalItems",
    "not",
    "contains",
    "propertyNames",
    "contentSchema",
    "unevaluatedItems",
    "unevaluatedProperties",
    "if",
    "then",
    "else",
];

/// Keywords whose value is a `{name: schema}` map. Walker recurses into
/// each map value as a nested schema.
pub(super) const SCHEMA_VALUED_MAP: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];

/// Keywords whose value is a `[schema, schema, ...]` array. Walker
/// recurses into each array element as a nested schema.
pub(super) const SCHEMA_VALUED_ARRAY: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];

/// JSON Schema document metadata keywords.
///
/// These identify the schema dialect, document ID, annotations,
/// fragment anchors, and vocabulary declarations. They are **never**
/// schema constraints — they carry document-level information that
/// structured-output APIs have no use for. Every walker run strips
/// them unconditionally, regardless of which provider policy is active.
///
/// Stripping these is what makes `schemars::schema_for!(T)` output work
/// universally: `schemars` emits `$schema` (draft marker) and sometimes
/// `$id` (document URI) at the top level of every generated schema, and
/// Gemini's OpenAPI 3.0 validator rejects them with
/// `"Unknown name '$schema' at 'generation_config.response_schema'"`.
///
/// # Keyword inventory
///
/// - `$schema` — JSON Schema dialect marker (draft-07, 2020-12, …)
/// - `$id` — document URI identifier
/// - `$comment` — human-readable annotation
/// - `$anchor` — 2020-12 named fragment anchor (referenced by `$ref`
///   *from outside the document* — in-document targets use `$defs`)
/// - `$vocabulary` — 2020-12 vocabulary map (declares which keyword
///   vocabularies the schema depends on)
///
/// **Intentionally NOT in this list**: `$ref`, `$defs`, `definitions`,
/// `$dynamicRef`, `$dynamicAnchor`. These are schema structure, not
/// metadata — stripping them would break the schema's semantics. The
/// walker follows `$ref` for cycle detection; `$defs` / `definitions`
/// are recursed into as schema-valued maps.
pub(super) const JSON_SCHEMA_META: &[&str] =
    &["$schema", "$id", "$comment", "$anchor", "$vocabulary"];

/// `true` if `r` is an internal JSON pointer reference (`"#"` or starts
/// with `"#/"`). External references (`http://...`, relative file paths,
/// `urn:...`) return `false`.
pub(super) fn is_internal_ref(r: &str) -> bool {
    r == "#" || r.starts_with("#/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_valued_keywords_are_distinct_from_value_valued() {
        // The lists must not overlap — a keyword cannot be both
        // schema-valued and literal at the same time.
        let all_schema: Vec<&&str> = SCHEMA_VALUED
            .iter()
            .chain(SCHEMA_VALUED_MAP.iter())
            .chain(SCHEMA_VALUED_ARRAY.iter())
            .collect();
        let unique: std::collections::HashSet<&&str> = all_schema.iter().copied().collect();
        assert_eq!(
            unique.len(),
            all_schema.len(),
            "duplicate keyword across SCHEMA_VALUED lists"
        );
    }

    #[test]
    fn value_valued_keywords_are_not_in_schema_lists() {
        // Spot-check: these must never be treated as schema-valued, or
        // the walker will corrupt user data inside them.
        let value_keywords = [
            "const",
            "enum",
            "default",
            "examples",
            "description",
            "title",
            "type",
            "required",
            "format",
            "pattern",
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
            "minItems",
            "maxItems",
        ];
        for kw in value_keywords {
            assert!(
                !SCHEMA_VALUED.contains(&kw),
                "{kw} must not be in SCHEMA_VALUED"
            );
            assert!(
                !SCHEMA_VALUED_MAP.contains(&kw),
                "{kw} must not be in SCHEMA_VALUED_MAP"
            );
            assert!(
                !SCHEMA_VALUED_ARRAY.contains(&kw),
                "{kw} must not be in SCHEMA_VALUED_ARRAY"
            );
        }
    }

    #[test]
    fn is_internal_ref_accepts_hash_root_and_hash_slash_paths() {
        assert!(is_internal_ref("#"));
        assert!(is_internal_ref("#/"));
        assert!(is_internal_ref("#/$defs/Foo"));
        assert!(is_internal_ref("#/properties/name"));
        assert!(is_internal_ref("#/definitions/Bar"));
    }

    #[test]
    fn is_internal_ref_rejects_external_forms() {
        assert!(!is_internal_ref("http://example.com/schema.json"));
        assert!(!is_internal_ref("https://example.com/schema.json"));
        assert!(!is_internal_ref("schemas/foo.json"));
        assert!(!is_internal_ref("schemas/foo.json#/definitions/Foo"));
        assert!(!is_internal_ref("urn:iso:std:iso:20022:schema"));
        assert!(!is_internal_ref(""));
        assert!(!is_internal_ref("foo"));
    }
}
