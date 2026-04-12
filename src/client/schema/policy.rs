//! Provider-specific JSON Schema preparation policies.
//!
//! Each LLM provider's structured-output validator accepts a different
//! subset of JSON Schema. `SchemaPolicy` captures that subset as a
//! configuration object — a flat set of transformation and rejection
//! rules that a single walker algorithm consumes. Adding a new provider
//! is purely additive: define a new `const fn` factory with the right
//! field values and the walker handles it with no core changes. This is
//! the Open-Closed Principle implemented as data rather than branches.

/// Whether to close object schemas by adding `additionalProperties: false`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectClosure {
    /// Leave `additionalProperties` alone if not present (Gemini).
    Leave,
    /// Add `additionalProperties: false` to any `type: "object"` schema
    /// that doesn't already specify it (OpenAI strict, Anthropic,
    /// Bedrock Converse).
    Closed,
}

/// How to handle `required` arrays on object schemas.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequiredHandling {
    /// Preserve the user-supplied `required` array verbatim (Anthropic,
    /// Gemini, Bedrock).
    Preserve,
    /// Auto-fill `required` with every property name. OpenAI strict mode
    /// requires this. Strengthens optional fields to required; the walker
    /// emits a `LossyEncode` warning so the caller knows the semantic
    /// change happened.
    AllProperties,
}

/// How to handle `minItems` on array schemas.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinItemsPolicy {
    /// Strip `minItems` unconditionally (OpenAI strict).
    Strip,
    /// Keep `minItems` if its value is ≤ `max`; strip (with warning)
    /// otherwise. Anthropic uses `KeepUpTo(1)` — values 0 and 1 allowed.
    KeepUpTo(u64),
    /// Keep `minItems` regardless of value (Gemini, Bedrock).
    Keep,
}

/// A provider-specific JSON Schema preparation policy.
///
/// Construct one of these via a `const fn` factory and pass it to
/// [`super::prepare_schema`] to transform a raw schema into the provider's
/// accepted subset. Callers that define their own codec can build a
/// custom `SchemaPolicy` inline.
///
/// # What this policy does *not* handle
///
/// JSON Schema **document metadata** (`$schema`, `$id`, `$comment`,
/// `$anchor`, `$vocabulary`) is stripped **unconditionally by the
/// walker**, regardless of which policy is active. These keywords are
/// document annotations, not schema constraints, so the decision to
/// drop them belongs to the walker (JSON Schema semantics), not to
/// any individual provider policy.
#[derive(Clone, Copy, Debug)]
pub struct SchemaPolicy {
    // ----- Transformation rules -----
    /// `additionalProperties: false` auto-insertion policy.
    pub object_closure: ObjectClosure,
    /// `required` array handling.
    pub required_handling: RequiredHandling,
    /// `minItems` value handling.
    pub min_items_policy: MinItemsPolicy,
    /// If `true`, an explicit `additionalProperties: true` is rewritten
    /// to `false` with a warning (Anthropic).
    pub override_open_objects: bool,
    /// Provider-specific keywords that must be stripped from every
    /// schema node, with a `LossyEncode` warning emitted per occurrence.
    /// JSON Schema document metadata is stripped separately by the
    /// walker — see the struct-level docs.
    pub strip_keywords: &'static [&'static str],
    /// Allowlist for the `format` keyword. `None` means any format is
    /// accepted (no stripping). `Some(&[...])` strips any `format` value
    /// not in the list, with a warning. Critical for `schemars`
    /// compatibility: `schemars` generates non-standard OpenAPI formats
    /// (`"uint64"`, `"int32"`, ...) that providers like Anthropic reject.
    pub allowed_formats: Option<&'static [&'static str]>,

    // ----- Rejection rules -----
    /// If `true`, direct and indirect `$ref` cycles are detected and the
    /// offending `$ref` sub-tree is replaced with an empty object.
    pub reject_cycles: bool,
    /// If `true`, `$ref` values that do not begin with `#/` (external
    /// references, e.g. `http://...`) are replaced with an empty object.
    pub reject_external_refs: bool,

    // ----- Wire format metadata support -----
    /// `true` if the provider's wire format has a sibling `name` field
    /// alongside the schema (OpenAI, Bedrock Converse). `false` if
    /// `JsonSchemaSpec::name` is silently irrelevant and should be
    /// dropped with a `LossyEncode` warning (Anthropic, Gemini).
    pub wire_supports_name: bool,
    /// `true` if the provider's wire format has a sibling `description`
    /// field alongside the schema. See [`Self::wire_supports_name`].
    pub wire_supports_description: bool,
}

impl SchemaPolicy {
    /// OpenAI strict mode, used by both
    /// [`OpenAiChatCodec`](crate::client::codec::OpenAiChatCodec) via
    /// `response_format.json_schema` and
    /// [`OpenAiResponsesCodec`](crate::client::codec::OpenAiResponsesCodec)
    /// via `text.format`.
    ///
    /// Rules:
    /// - Every object is closed (`additionalProperties: false`).
    /// - Every property is marked as required (strict mode invariant).
    /// - Numerical, length, and array constraints are stripped.
    /// - Formats are not filtered — OpenAI accepts most standard formats.
    /// - Wire format surfaces `name`, `description`, and `strict`.
    ///
    /// `minItems` is **not** listed in `strip_keywords`; it is handled
    /// exclusively by [`MinItemsPolicy::Strip`] to keep the responsibility
    /// for array-item constraints in one place. JSON Schema document
    /// metadata (`$schema`, `$id`, …) is stripped at the walker level,
    /// not here.
    pub const fn openai_strict() -> Self {
        Self {
            object_closure: ObjectClosure::Closed,
            required_handling: RequiredHandling::AllProperties,
            min_items_policy: MinItemsPolicy::Strip,
            override_open_objects: false,
            strip_keywords: &[
                // Numeric constraints (OpenAI strict mode rejects them).
                "minimum",
                "maximum",
                "exclusiveMinimum",
                "exclusiveMaximum",
                "multipleOf",
                // String length.
                "minLength",
                "maxLength",
                // Array / object size (minItems handled by MinItemsPolicy).
                "maxItems",
                "minProperties",
                "maxProperties",
            ],
            allowed_formats: None,
            reject_cycles: false,
            reject_external_refs: false,
            wire_supports_name: true,
            wire_supports_description: true,
        }
    }

    /// Anthropic structured outputs (`output_config.format.schema`).
    ///
    /// # Anthropic JSON Schema subset (2026-04-10 GA)
    ///
    /// **Supported**: `object`, `array`, `string`, `integer`, `number`,
    /// `boolean`, `null`; `enum` (primitives only); `const`; `anyOf`,
    /// `allOf` (but not `allOf` with `$ref`); internal `$ref`, `$defs`,
    /// `definitions`; `default`; `required`; `additionalProperties: false`;
    /// string formats (`date-time`, `time`, `date`, `duration`, `email`,
    /// `hostname`, `uri`, `ipv4`, `ipv6`, `uuid`); `minItems` values 0/1.
    ///
    /// **Rejected by this walker**: recursive schemas, external `$ref`,
    /// numeric constraints, length constraints, `additionalProperties: true`,
    /// `minItems > 1`, `maxItems`, non-allowlisted formats.
    ///
    /// **Not detected by this walker — provider will reject**:
    /// - `allOf` with `$ref` sibling (detection requires sibling-aware
    ///   traversal; provider returns 400).
    /// - Complex values inside `enum` (enum items must be primitives).
    /// - Regex backreferences, lookaround, word boundaries, complex
    ///   `{n,m}` quantifiers in `pattern`.
    ///
    /// # Property ordering
    ///
    /// Anthropic's grammar emits required properties first (in schema
    /// order), then optional properties. Callers that care about order
    /// should mark all properties required or reorder after parsing.
    ///
    /// # Grammar caching
    ///
    /// Anthropic caches compiled grammars for 24 hours. Reusing the same
    /// schema keeps subsequent calls fast; changing the schema (or its
    /// set of tools) invalidates the cache.
    ///
    /// # Token cost
    ///
    /// Enabling structured outputs injects an extra system prompt,
    /// slightly increasing input token count.
    pub const fn anthropic() -> Self {
        Self {
            object_closure: ObjectClosure::Closed,
            required_handling: RequiredHandling::Preserve,
            min_items_policy: MinItemsPolicy::KeepUpTo(1),
            override_open_objects: true,
            strip_keywords: &[
                // Numeric constraints (Anthropic GA subset rejects them).
                "minimum",
                "maximum",
                "exclusiveMinimum",
                "exclusiveMaximum",
                "multipleOf",
                // String length.
                "minLength",
                "maxLength",
                // Array / object size (minItems handled by MinItemsPolicy).
                "maxItems",
                "minProperties",
                "maxProperties",
            ],
            allowed_formats: Some(&[
                "date-time",
                "time",
                "date",
                "duration",
                "email",
                "hostname",
                "uri",
                "ipv4",
                "ipv6",
                "uuid",
            ]),
            reject_cycles: true,
            reject_external_refs: true,
            // `output_config.format.schema` has no sibling `name` or
            // `description` fields on the wire — any metadata set on
            // `JsonSchemaSpec` is dropped with a lossy warning.
            wire_supports_name: false,
            wire_supports_description: false,
        }
    }

    /// Gemini `generationConfig.responseSchema` (OpenAPI 3.0 subset).
    ///
    /// Gemini's `responseSchema` validator is **strict about unknown
    /// fields**: any property that is not in the OpenAPI 3.0 schema
    /// object definition returns a wire-level 400 with
    /// `Unknown name "<field>" at 'generation_config.response_schema'`.
    ///
    /// Gemini has **no provider-specific strict rules** beyond the
    /// universal walker-level metadata strip and cycle / external-`$ref`
    /// rejection — so this factory is defined as an alias of
    /// [`Self::lenient`]. The walker-level JSON Schema metadata strip
    /// handles schemars-generated `$schema` / `$id` keywords; numeric
    /// constraints pass through because Gemini's OpenAPI 3.0 validator
    /// accepts them (unlike Anthropic's grammar compiler).
    ///
    /// If Gemini ever gains a stricter mode (e.g. a grammar compiler),
    /// this factory will inline its own body to diverge from `lenient`.
    pub const fn gemini() -> Self {
        Self::lenient()
    }

    /// Baseline "minimum safe prep" policy for schemas whose provider
    /// constraints are either **unknown** or **not enforced** — the
    /// canonical use case is **non-strict tool schemas**.
    ///
    /// Behavior:
    /// - Walker still strips JSON Schema document metadata
    ///   (`$schema`, `$id`, `$comment`, `$anchor`, `$vocabulary`) —
    ///   this is unconditional and happens regardless of policy.
    /// - Cycles and external `$ref` are rejected (universally bad).
    /// - Nothing else is transformed or stripped. Objects stay as
    ///   declared, `required` is user-controlled, numeric constraints
    ///   survive.
    ///
    /// Use via
    /// `prepare_tool_schema(value, &SCHEMA_POLICY, is_strict, tool_name)` —
    /// when `is_strict` is `false`, the helper substitutes this policy
    /// instead of the codec's full strict policy. This preserves user
    /// intent on non-strict tools (e.g. `"minimum": 0` documentation)
    /// while still fixing schemars-generated schemas for Gemini.
    pub const fn lenient() -> Self {
        Self {
            object_closure: ObjectClosure::Leave,
            required_handling: RequiredHandling::Preserve,
            min_items_policy: MinItemsPolicy::Keep,
            override_open_objects: false,
            strip_keywords: &[],
            allowed_formats: None,
            reject_cycles: true,
            reject_external_refs: true,
            // Tool schemas never carry wire metadata (`name` and
            // `description` live on the `ToolDefinition`, not inside
            // the schema), so these flags are irrelevant for tool use.
            // Setting `false` is a safe default.
            wire_supports_name: false,
            wire_supports_description: false,
        }
    }

    /// AWS Bedrock Converse structured outputs
    /// (`outputConfig.textFormat.structure.jsonSchema`).
    ///
    /// Bedrock Converse hosts many model families (Anthropic Claude,
    /// Qwen, DeepSeek, Mistral, Gemma, Kimi, gpt-oss, MiniMax, Nemotron,
    /// …), each with its own schema subset. This policy is the
    /// intersection-compatible minimum: close objects, reject cycles
    /// and external references, strip nothing else, and let Bedrock's
    /// server-side validator surface model-specific errors.
    pub const fn bedrock_converse() -> Self {
        Self {
            object_closure: ObjectClosure::Closed,
            required_handling: RequiredHandling::Preserve,
            min_items_policy: MinItemsPolicy::Keep,
            override_open_objects: false,
            strip_keywords: &[],
            allowed_formats: None,
            reject_cycles: true,
            reject_external_refs: true,
            // `outputConfig.textFormat.structure.jsonSchema` has sibling
            // `name` and `description` fields natively.
            wire_supports_name: true,
            wire_supports_description: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_strict_factory_has_expected_rules() {
        let p = SchemaPolicy::openai_strict();
        assert_eq!(p.object_closure, ObjectClosure::Closed);
        assert_eq!(p.required_handling, RequiredHandling::AllProperties);
        assert_eq!(p.min_items_policy, MinItemsPolicy::Strip);
        assert!(!p.override_open_objects);
        assert!(p.strip_keywords.contains(&"minimum"));
        assert!(p.strip_keywords.contains(&"maxLength"));
        // JSON Schema metadata is stripped at walker level — never in
        // any policy's strip_keywords.
        assert!(!p.strip_keywords.contains(&"$schema"));
        assert!(p.allowed_formats.is_none());
        assert!(!p.reject_cycles);
        assert!(!p.reject_external_refs);
        assert!(p.wire_supports_name);
        assert!(p.wire_supports_description);
    }

    #[test]
    fn anthropic_factory_has_expected_rules() {
        let p = SchemaPolicy::anthropic();
        assert_eq!(p.object_closure, ObjectClosure::Closed);
        assert_eq!(p.required_handling, RequiredHandling::Preserve);
        assert_eq!(p.min_items_policy, MinItemsPolicy::KeepUpTo(1));
        assert!(p.override_open_objects);
        assert!(p.strip_keywords.contains(&"minimum"));
        assert!(!p.strip_keywords.contains(&"$schema"));
        assert!(!p.strip_keywords.contains(&"minItems"));
        let allowed = p.allowed_formats.unwrap();
        assert!(allowed.contains(&"date-time"));
        assert!(allowed.contains(&"email"));
        assert!(!allowed.contains(&"uint64"));
        assert!(p.reject_cycles);
        assert!(p.reject_external_refs);
        // Anthropic `output_config.format` has no sibling metadata.
        assert!(!p.wire_supports_name);
        assert!(!p.wire_supports_description);
    }

    #[test]
    fn gemini_factory_is_lenient_on_constraints() {
        let p = SchemaPolicy::gemini();
        assert_eq!(p.object_closure, ObjectClosure::Leave);
        assert_eq!(p.required_handling, RequiredHandling::Preserve);
        assert_eq!(p.min_items_policy, MinItemsPolicy::Keep);
        // Gemini strips nothing at the policy level. JSON Schema
        // metadata is handled by the walker.
        assert!(p.strip_keywords.is_empty());
        assert!(p.allowed_formats.is_none());
        assert!(p.reject_cycles);
        assert!(p.reject_external_refs);
        // `responseSchema` has no sibling metadata.
        assert!(!p.wire_supports_name);
        assert!(!p.wire_supports_description);
    }

    #[test]
    fn bedrock_converse_factory_closes_objects_only() {
        let p = SchemaPolicy::bedrock_converse();
        assert_eq!(p.object_closure, ObjectClosure::Closed);
        assert_eq!(p.required_handling, RequiredHandling::Preserve);
        assert_eq!(p.min_items_policy, MinItemsPolicy::Keep);
        assert!(p.strip_keywords.is_empty());
        assert!(p.allowed_formats.is_none());
        assert!(p.reject_cycles);
        assert!(p.reject_external_refs);
        // Bedrock Converse surfaces name + description natively.
        assert!(p.wire_supports_name);
        assert!(p.wire_supports_description);
    }

    #[test]
    fn policies_are_const_constructible() {
        // Compile-time assertion that every factory is usable in a `const`
        // context — callers keep the policy as a `const` next to their
        // codec and pay zero allocation cost per encode.
        const _OPENAI: SchemaPolicy = SchemaPolicy::openai_strict();
        const _ANTHROPIC: SchemaPolicy = SchemaPolicy::anthropic();
        const _GEMINI: SchemaPolicy = SchemaPolicy::gemini();
        const _BEDROCK: SchemaPolicy = SchemaPolicy::bedrock_converse();
    }
}
