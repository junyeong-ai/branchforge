---
paths:
  - "src/client/schema/**"
---

# Schema Preparation Rules

Central infrastructure for JSON-schema-based structured outputs. Every codec runs user schemas through `prepare_schema(value, &policy, source_path)` before embedding them in its wire envelope, and calls `warn_dropped_metadata` for codecs whose wire format drops name/description. Tool `input_schema` values go through the same pipeline via `prepare_tool_schema`.

## Module layout

- `mod.rs` — public API: `SchemaPolicy`, `PreparedSchema`, `prepare_schema`, `schema_for<T>`, `prepare_tool_schema`, `warn_dropped_metadata`
- `policy.rs` — `SchemaPolicy` struct, enums (`ObjectClosure`, `RequiredHandling`, `MinItemsPolicy`), 5 `const fn` factories (`lenient`, `openai_strict`, `anthropic`, `gemini`, `bedrock_converse`)
- `walker.rs` — `SchemaWalker`, `JsonPointer`, `MAX_DEPTH = 500` guard, node-rule application
- `cycles.rs` — `compute_cyclic_defs` via DFS reachability over `$ref` graph
- `keywords.rs` — JSON-Schema keyword classification (`SCHEMA_VALUED`, `SCHEMA_VALUED_MAP`, `SCHEMA_VALUED_ARRAY`, `JSON_SCHEMA_META`, `is_internal_ref`)

## The configuration-object pattern (Open-Closed by data)

Adding a new provider is **additive**: define a new `const fn` on `SchemaPolicy` with appropriate field values, and the walker handles it with no core changes. Never add branches to the walker per provider — express the difference as data on `SchemaPolicy`.

The 5 existing factories:

| Factory | `object_closure` | `required_handling` | `min_items_policy` | `reject_cycles` | `wire_supports_name` | `wire_supports_description` |
| :--- | :--- | :--- | :--- | :---: | :---: | :---: |
| `lenient()` | Leave | Preserve | Keep | true | ❌ | ❌ |
| `openai_strict()` | Closed | AllProperties | Strip | false | ✅ | ✅ |
| `anthropic()` | Closed | Preserve | KeepUpTo(1) | true | ❌ | ❌ |
| `gemini()` | (alias of `lenient()`) | | | | | |
| `bedrock_converse()` | Closed | Preserve | Keep | true | ✅ | ✅ |

`lenient()` is the baseline: walker-level metadata strip + cycle /
external-`$ref` rejection, nothing else. `gemini()` is defined as
`Self::lenient()` because Gemini's OpenAPI 3.0 validator accepts
everything the lenient policy preserves; if Gemini ever gains a
stricter mode, the factory will inline its own body to diverge.

`lenient()` is also the policy used for **non-strict tool schemas**
across every codec — see the "Tool schema preparation" section below.

## JSON Schema metadata is walker-level, not policy-level

`$schema`, `$id`, `$comment`, `$anchor`, `$vocabulary` are **JSON Schema document metadata**, not schema constraints. They carry dialect markers / identifiers / annotations / vocabulary declarations that no structured-output API uses as a constraint. The walker strips them **unconditionally** regardless of which `SchemaPolicy` is active — see `JSON_SCHEMA_META` in `keywords.rs`.

This is a load-bearing separation of responsibility:
- **Walker**: JSON Schema semantics (what is a constraint vs metadata)
- **Policy**: Provider-specific rules (what constraints this provider rejects)

Never add `$schema` etc. to a policy's `strip_keywords` — the walker already handles them. Adding them at the policy level is a DRY violation and treats universal JSON Schema semantics as if they were provider-specific.

This stripping is what makes `JsonSchemaSpec::from_type::<T>()` work universally: `schemars::schema_for!(T)` emits `$schema` at the top level of every generated schema, and Gemini's strict OpenAPI 3.0 validator rejects it with `Unknown name "$schema"`. The walker-level strip catches this once for every provider, present and future.

## Wire metadata support is policy data

`JsonSchemaSpec` carries optional `name` and `description` fields. Whether these reach the wire depends on the provider's wire format:

- **OpenAI Chat** / **OpenAI Responses** / **Bedrock Converse**: native sibling fields alongside the schema. Codec inserts them into the envelope directly.
- **Anthropic Messages** / **Gemini GenerateContent**: no wire support. Metadata is dropped with a `LossyEncode` warning.

`SchemaPolicy.wire_supports_name` and `SchemaPolicy.wire_supports_description` carry this truth per policy. The shared `warn_dropped_metadata(spec, policy, codec_id, warnings)` function in `schema/mod.rs` reads these flags and emits warnings uniformly — **there is no per-codec `warn_dropped_<provider>_metadata` helper**. Codecs that drop metadata (Anthropic, Gemini) call the shared function after emitting the wire envelope; codecs that embed metadata on the wire (OpenAI, Bedrock) never need it because they handle the fields directly.

## Walker correctness — schema vs literal

The walker **only recurses into schema-valued children**. Literal values (`const`, `enum`, `default`, `examples`) are preserved verbatim even if they contain keywords that would be stripped in a schema context. This is load-bearing correctness: `const: {"minimum": 5}` must not have `minimum` stripped, because it's user data, not a schema constraint.

The classification lives in `keywords.rs`:

- **`SCHEMA_VALUED`**: value is a single schema — `items`, `additionalProperties`, `not`, `if`/`then`/`else`, `contains`, `propertyNames`, `contentSchema`, `unevaluatedItems`, `unevaluatedProperties`, `additionalItems`
- **`SCHEMA_VALUED_MAP`**: value is `{name: schema}` — `properties`, `patternProperties`, `$defs`, `definitions`, `dependentSchemas`
- **`SCHEMA_VALUED_ARRAY`**: value is `[schema, ...]` — `allOf`, `anyOf`, `oneOf`, `prefixItems`

Anything else (`type`, `const`, `enum`, `default`, `examples`, `required`, `minimum`, metadata) is treated as a value — the walker does not recurse into it.

## Node-rule application order

`SchemaWalker::apply_node_rules` runs in a fixed order. Changing the order affects warning surface:

1. `strip_keywords` iteration (generic "keyword not supported")
2. `allowed_formats` allowlist check
3. `apply_min_items_policy` (specific "minItems ..." message)
4. `override_open_objects` (`additionalProperties: true` → `false`)
5. `object_closure` (adds `additionalProperties: false` to `type: object` nodes)
6. `required_handling` (auto-fill via `auto_fill_required`)

**`minItems` is handled exclusively by `min_items_policy`** — it must not appear in any factory's `strip_keywords`. Putting it in both results in the generic message winning (DRY violation).

## `auto_fill_required` — preserve user order

When `RequiredHandling::AllProperties` strengthens optional fields to required, the existing user-supplied `required` array is preserved in its original order; newly-required fields are **appended** in property declaration order. Rebuilding the array from `properties.keys()` would silently lose load-bearing user order and is forbidden.

## Cycle detection

`cycles::compute_cyclic_defs` runs a per-def DFS reachability check over the `$defs` / `definitions` graph, returning the set of cyclic target paths (e.g. `"#/$defs/Node"`). The walker pre-computes this set in `SchemaWalker::new` and consults it on every `$ref` visit. A cyclic `$ref` node is **replaced with an empty object** and a lossy warning is emitted at the ref site, not at the target.

The `collect_refs_in` helper only recurses through schema-valued children — it does **not** follow references inside `const`/`enum`/`default` values, so fake "refs" in user data cannot create phantom cycles.

### Scope limitation (documented)

The detector only sees refs **reachable from `$defs` / `definitions` entries**. In-place self-referential schemas like `{"$ref": "#"}` or `{"$ref": "#/properties/x"}` are **not** detected locally — schemars always routes recursive types through `$defs`, so this doesn't affect realistic use, but manually-written in-place recursive schemas will reach the provider and be rejected there. Full JSON-pointer cycle detection is deferred until a real-world schema hits the gap.

## External `$ref`

`is_internal_ref(r)` returns `true` only when `r == "#"` or `r.starts_with("#/")`. Anything else (`http://`, `urn:`, relative file paths) is external. Policies with `reject_external_refs: true` replace external-ref nodes with an empty object.

## `MAX_DEPTH` guard

Hard limit: `const MAX_DEPTH: usize = 500`. Real schemas rarely exceed ~20 levels; the guard exists to prevent stack overflow on malicious or buggy inputs. At-limit nodes return an empty object with a `<source_path>/…/depth` lossy warning, where `<source_path>` is whatever the caller passed to `prepare_schema` (e.g. `response_format.schema` or `tool.calculator.input_schema`).

## `JsonPointer` for warning attribution

All warnings carry a field path built from `source_path + RFC 6901 JSON pointer`. For response-format schemas this looks like `response_format.schema/properties/foo/minimum`; for tool schemas prepared by `prepare_tool_schema` it looks like `tool.<name>.input_schema/properties/foo/minimum`. The source path is supplied by the caller (`prepare_schema(value, &policy, source_path)`), **not hardcoded in the walker** — this is what lets the same pipeline serve both response-format and tool schemas with correctly attributed warnings. `JsonPointer::push(segment)` escapes `~` → `~0` and `/` → `~1`. `JsonPointer::push_index(i)` is for schema-valued arrays like `allOf`.

## `PreparedSchema` ergonomics

- `PreparedSchema::passthrough(value)` — codec helper for the non-strict / skip-policy path (used by OpenAI codecs when `spec.strict == false`).
- `PreparedSchema::into_value()` — consume and discard warnings.
- `PreparedSchema::is_clean()` — `true` when no lossy transformations happened.

## Extension checklist (adding a new provider)

1. Add `pub const fn new_provider() -> Self` to `SchemaPolicy` in `policy.rs` with the appropriate field values, including `wire_supports_name` / `wire_supports_description` based on the wire format.
2. Add factory validation test to `policy.rs` tests.
3. In the new codec, declare `const SCHEMA_POLICY: SchemaPolicy = SchemaPolicy::new_provider();` next to the codec's other constants.
4. Implement `encode_response_format(format, body, warnings)` in the codec. In the `JsonSchema(spec)` branch:
   - `let prepared = prepare_schema(spec.schema.clone(), &SCHEMA_POLICY, "response_format.schema");`
   - `warnings.extend(prepared.warnings);`
   - emit the provider-specific wire envelope using `prepared.value`
   - if `SCHEMA_POLICY.wire_supports_name` / `description` is `false`, call `warn_dropped_metadata(spec, &SCHEMA_POLICY, CODEC_ID, warnings)` to surface lossy warnings uniformly. If the wire DOES support them, insert `spec.name` / `spec.description` into the envelope directly and skip the helper call.
5. Implement `encode_tool_definition(tool, warnings)` using `prepare_tool_schema(tool.parameters.clone(), &SCHEMA_POLICY, tool.strict, &tool.name)` — see the "Tool encode template" section below.
6. Add the codec to `tests/codec_contract.rs::capability_honesty_response_format` **and** `capability_honesty_tool_strict`.
7. **Do not modify `walker.rs`, `cycles.rs`, or `keywords.rs`.** If you need to, you're violating Open-Closed.

## Tool schema preparation

Tool `input_schema` goes through the **same `prepare_schema` pipeline**
as response_format schemas, via the shared
[`prepare_tool_schema(schema, codec_policy, is_strict, tool_name)`]
helper in `schema/mod.rs`. This is the root-cause fix for two bug
classes that would otherwise affect every codec:

1. **JSON Schema metadata leakage** — `schemars::schema_for!(T)` emits
   `$schema` at the top of every generated tool schema, and Gemini's
   OpenAPI 3.0 validator rejects it with a 400. The walker's
   unconditional `JSON_SCHEMA_META` strip catches this on every tool
   schema regardless of provider.
2. **Strict mode schema incompatibility** — OpenAI and Anthropic strict
   tool use apply the same constraint subset as response_format. Tools
   with schemars-generated numeric constraints (`minimum: 0` for
   unsigned types) would fail the strict validator without the shared
   preparation pipeline.

### When to use strict vs. lenient tool policy

`prepare_tool_schema` switches between two policy paths based on
`is_strict`:

- `is_strict == true` → codec's full strict `SCHEMA_POLICY`
  (provider-specific constraint subset)
- `is_strict == false` → [`SchemaPolicy::lenient`] (walker-level strip
  + cycle / external-`$ref` rejection; no provider-specific stripping,
  so user-declared `minimum: 0` etc. survive as documentation)

Per-codec wiring:

| Codec | `is_strict` source | Wire strict flag |
| :--- | :--- | :--- |
| `openai_chat` | `tool.strict` | emits `"strict": true` |
| `openai_responses` | `tool.strict` | emits `"strict": true` |
| `anthropic_messages` | `tool.strict` | emits `"strict": true` (Anthropic GA strict tool use) |
| `gemini_generate` | always `false` | no wire flag (Gemini has no tool strict mode) |
| `bedrock_converse` | always `false` | no wire flag (Bedrock tool strictness is model-dependent) |

Gemini and Bedrock always pass `is_strict: false` because their wire
format has no tool-level strict flag. The walker-level metadata strip
still runs (that's unconditional), which is what fixes the Gemini
`$schema` bug. When a caller sets `tool.strict = true` on a codec
whose wire format has no strict flag, the codec must emit a
`ModelWarning::LossyEncode { field: "tool.<name>.strict", … }` so
that capability honesty is preserved — the IR field was set and
silently dropped, which the caller deserves to see.

### Tool encode template

Every codec's `encode_tool_definition(tool, warnings)` follows the
same template. The `tool_name` is passed to `prepare_tool_schema` so
walker warnings are attributed as `tool.<name>.input_schema/...`
rather than being misattributed to `response_format.schema`.

```rust
fn encode_tool_definition(tool: &ToolDefinition, warnings: &mut Vec<ModelWarning>) -> Value {
    // 1. Prepare the schema through the shared pipeline. `&tool.name`
    //    flows through as the source_path prefix for warning attribution.
    let prepared = prepare_tool_schema(
        tool.parameters.clone(),
        &SCHEMA_POLICY,
        tool.strict,
        &tool.name,
    );
    // 2. Propagate walker warnings.
    warnings.extend(prepared.warnings);
    // 3. If the wire format has no strict flag but the caller set
    //    `tool.strict = true`, emit a drop warning for honesty.
    //    (Gemini and Bedrock only — OpenAI/Anthropic emit wire strict.)
    if tool.strict && !WIRE_SUPPORTS_TOOL_STRICT {
        warnings.push(ModelWarning::lossy(
            format!("tool.{}.strict", tool.name),
            format!("{CODEC_ID} wire format has no tool-level strict flag; dropped"),
        ));
    }
    // 4. Build the provider-specific wire envelope using `prepared.value`.
    let mut obj = json!({ "name": tool.name, /* ... */ });
    // 5. Emit the strict flag if the provider's wire format supports it.
    if tool.strict && WIRE_SUPPORTS_TOOL_STRICT { obj["strict"] = json!(true); }
    obj
}
```

The `encode_request` caller iterates `request.tools` and passes
`&mut warnings` to each call — never `.map(encode_tool_definition)`,
because the helper now takes a mutable reference.

[`prepare_tool_schema(schema, codec_policy, is_strict, tool_name)`]: crate::client::schema::prepare_tool_schema
[`SchemaPolicy::lenient`]: crate::client::schema::SchemaPolicy::lenient
