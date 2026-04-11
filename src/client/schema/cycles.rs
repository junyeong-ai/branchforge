//! Cycle detection on `$ref` graphs.
//!
//! Anthropic (and some other provider subsets) reject recursive schemas.
//! Detecting cycles requires walking the `$ref` dependency graph of the
//! schema's `$defs` / `definitions` and flagging every definition that
//! can reach itself via a back edge.
//!
//! We use a simple DFS reachability check rather than Tarjan's SCC: the
//! only information the walker needs is "is this def cyclic?" (a boolean
//! per def), not the structure of each strongly-connected component. For
//! typical schemas with < 50 definitions, the O(V·(V+E)) cost of the
//! per-def DFS is well under a millisecond.
//!
//! # Scope — known limitation
//!
//! This detector looks at refs **reachable from `$defs` / `definitions`
//! entries** only. In-place self-referential schemas like
//!
//! ```json
//! {
//!   "type": "object",
//!   "properties": {
//!     "self": {"$ref": "#"}
//!   }
//! }
//! ```
//!
//! are **not** detected as cycles — the `#` ref target is the root
//! schema, which is not a named def. In practice this is not a problem
//! because `schemars::schema_for!(T)` **always** routes recursive types
//! through a `$defs` entry, which this detector catches. Manually
//! written schemas with in-place cycles will reach the provider, and
//! the provider's validator will reject them with a clearer domain
//! error than we could produce locally.
//!
//! Generalising to full JSON pointer cycle detection would require
//! tracking the current visit stack at the walker level and resolving
//! every `$ref` on the fly — tracked as a future improvement if a
//! real-world schema hits the gap.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::keywords::{SCHEMA_VALUED, SCHEMA_VALUED_ARRAY, SCHEMA_VALUED_MAP, is_internal_ref};

/// Return the set of `$ref` target paths (e.g. `"#/$defs/Node"`) that
/// participate in a cycle in the schema's definition graph.
///
/// A def participates in a cycle if it is reachable from itself via a
/// chain of internal `$ref` edges. Both direct self-loops (`Node →
/// Node`) and indirect cycles (`A → B → A`) are detected.
pub(super) fn compute_cyclic_defs(schema: &Value) -> HashSet<String> {
    let defs = collect_defs(schema);
    let mut cyclic = HashSet::new();
    for start in defs.keys() {
        if is_reachable_from_self(start, &defs) {
            cyclic.insert(start.clone());
        }
    }
    cyclic
}

/// Collect all definitions under `$defs` and `definitions` keyed by their
/// JSON pointer path (e.g. `"#/$defs/Node"`).
fn collect_defs(schema: &Value) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    if let Some(defs) = schema.get("$defs").and_then(Value::as_object) {
        for (name, value) in defs {
            out.insert(format!("#/$defs/{name}"), value.clone());
        }
    }
    if let Some(defs) = schema.get("definitions").and_then(Value::as_object) {
        for (name, value) in defs {
            out.insert(format!("#/definitions/{name}"), value.clone());
        }
    }
    out
}

/// `true` if `start` is reachable from itself via the `$ref` graph.
fn is_reachable_from_self(start: &str, defs: &HashMap<String, Value>) -> bool {
    let mut stack = Vec::new();
    let mut visited = HashSet::new();

    // Seed the stack with the direct references of `start` so the first
    // pop represents a non-trivial step. Without this the function would
    // trivially return `true` for every def.
    if let Some(def) = defs.get(start) {
        for reference in collect_refs_in(def) {
            if is_internal_ref(&reference) {
                stack.push(reference);
            }
        }
    }

    while let Some(current) = stack.pop() {
        if current == start {
            return true;
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        if let Some(def) = defs.get(&current) {
            for reference in collect_refs_in(def) {
                if is_internal_ref(&reference) {
                    stack.push(reference);
                }
            }
        }
    }
    false
}

/// Collect every `$ref` string value reachable from `schema` via the
/// schema-valued children (not via `const` / `enum` / `default` literals).
pub(super) fn collect_refs_in(schema: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_refs_walk(schema, &mut out);
    out
}

fn collect_refs_walk(value: &Value, out: &mut Vec<String>) {
    let Value::Object(obj) = value else { return };

    if let Some(r) = obj.get("$ref").and_then(Value::as_str) {
        out.push(r.to_string());
    }

    for (key, child) in obj {
        if SCHEMA_VALUED_MAP.contains(&key.as_str()) {
            if let Some(map) = child.as_object() {
                for (_, nested) in map {
                    collect_refs_walk(nested, out);
                }
            }
        } else if SCHEMA_VALUED_ARRAY.contains(&key.as_str()) {
            if let Some(arr) = child.as_array() {
                for nested in arr {
                    collect_refs_walk(nested, out);
                }
            }
        } else if SCHEMA_VALUED.contains(&key.as_str()) {
            if key == "items" && child.is_array() {
                if let Some(arr) = child.as_array() {
                    for nested in arr {
                        collect_refs_walk(nested, out);
                    }
                }
            } else if !child.is_boolean() {
                collect_refs_walk(child, out);
            }
        }
        // `const`, `enum`, `default`, etc. — skipped intentionally.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn direct_self_loop_is_cyclic() {
        let schema = json!({
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": { "next": { "$ref": "#/$defs/Node" } }
                }
            }
        });
        let cyclic = compute_cyclic_defs(&schema);
        assert!(cyclic.contains("#/$defs/Node"));
        assert_eq!(cyclic.len(), 1);
    }

    #[test]
    fn indirect_two_def_cycle_detected() {
        let schema = json!({
            "$defs": {
                "A": { "properties": { "b": { "$ref": "#/$defs/B" } } },
                "B": { "properties": { "a": { "$ref": "#/$defs/A" } } }
            }
        });
        let cyclic = compute_cyclic_defs(&schema);
        assert!(cyclic.contains("#/$defs/A"));
        assert!(cyclic.contains("#/$defs/B"));
    }

    #[test]
    fn three_def_cycle_detected() {
        let schema = json!({
            "$defs": {
                "A": { "properties": { "b": { "$ref": "#/$defs/B" } } },
                "B": { "properties": { "c": { "$ref": "#/$defs/C" } } },
                "C": { "properties": { "a": { "$ref": "#/$defs/A" } } }
            }
        });
        let cyclic = compute_cyclic_defs(&schema);
        assert_eq!(cyclic.len(), 3);
    }

    #[test]
    fn non_cyclic_chain_not_flagged() {
        let schema = json!({
            "$defs": {
                "Name": { "type": "string" },
                "Person": {
                    "properties": { "name": { "$ref": "#/$defs/Name" } }
                }
            }
        });
        let cyclic = compute_cyclic_defs(&schema);
        assert!(cyclic.is_empty());
    }

    #[test]
    fn external_refs_are_ignored_by_cycle_detection() {
        let schema = json!({
            "$defs": {
                "Node": {
                    "properties": {
                        "other": { "$ref": "http://example.com/other.json" }
                    }
                }
            }
        });
        let cyclic = compute_cyclic_defs(&schema);
        assert!(cyclic.is_empty());
    }

    #[test]
    fn empty_defs_produces_empty_set() {
        let schema = json!({ "type": "object" });
        let cyclic = compute_cyclic_defs(&schema);
        assert!(cyclic.is_empty());
    }

    #[test]
    fn schema_without_any_ref_is_non_cyclic() {
        let schema = json!({
            "$defs": {
                "Name": { "type": "string" },
                "Age": { "type": "integer" }
            }
        });
        let cyclic = compute_cyclic_defs(&schema);
        assert!(cyclic.is_empty());
    }

    #[test]
    fn refs_inside_const_values_are_ignored() {
        // Even if `const` contains something that looks like a ref, it's
        // user data — not a schema edge.
        let schema = json!({
            "$defs": {
                "A": {
                    "const": { "$ref": "#/$defs/A" }
                }
            }
        });
        let cyclic = compute_cyclic_defs(&schema);
        assert!(cyclic.is_empty());
    }

    #[test]
    fn legacy_definitions_keyword_also_supported() {
        let schema = json!({
            "definitions": {
                "Node": {
                    "properties": { "next": { "$ref": "#/definitions/Node" } }
                }
            }
        });
        let cyclic = compute_cyclic_defs(&schema);
        assert!(cyclic.contains("#/definitions/Node"));
    }
}
