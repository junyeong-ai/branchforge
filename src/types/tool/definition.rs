//! Local tool spec — runtime metadata wrapper around a tool's name + schema.
//!
//! This is the *local registry* representation of a tool. It carries
//! runtime-only metadata (`defer_loading`, token estimation) that the
//! provider-neutral wire format [`crate::ir::ToolDefinition`] does not.
//!
//! At request build time the agent layer converts `ToolSpec` into one or
//! more `ir::ToolDefinition` values for the codec to encode.
//!
//! Naming: `ToolDefinition` is reserved for the wire-format IR type. Any
//! "tool definition with extras" is a [`ToolSpec`].

use serde::{Deserialize, Serialize};

/// Local runtime spec for a tool: schema + lazy-load metadata.
///
/// Distinct from [`crate::ir::ToolDefinition`] (wire format). The agent
/// runtime stores `ToolSpec` so it can apply registry-side optimizations
/// (deferred loading, token estimation) before lowering to IR.
///
/// `strict` and `defer_loading` are plain `bool` (not `Option<bool>`)
/// because both have a sensible default of `false` and the previous
/// tri-state was an artifact of legacy serialization, not a real signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub strict: bool,
    /// If `true`, the agent runtime may delay loading the schema into
    /// the model context until the tool is searched for or invoked.
    #[serde(default)]
    pub defer_loading: bool,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            strict: false,
            defer_loading: false,
        }
    }

    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    pub fn defer_loading(mut self, defer: bool) -> Self {
        self.defer_loading = defer;
        self
    }

    pub fn deferred(mut self) -> Self {
        self.defer_loading = true;
        self
    }

    pub fn is_deferred(&self) -> bool {
        self.defer_loading
    }

    pub fn estimated_tokens(&self) -> usize {
        estimate_tool_tokens(&self.name, &self.description, &self.input_schema)
    }
}

/// Lower a borrowed [`ToolSpec`] to the wire-format
/// [`crate::ir::ToolDefinition`]. Used in the read-only path where the
/// agent layer iterates over a collection of specs.
///
/// `defer_loading` is intentionally dropped — it is a runtime registry
/// hint, not part of the wire payload. The agent layer queries
/// [`ToolSpec::is_deferred`] before deciding whether to include the spec
/// in the request at all.
impl From<&ToolSpec> for crate::ir::ToolDefinition {
    fn from(spec: &ToolSpec) -> Self {
        crate::ir::ToolDefinition {
            name: spec.name.clone(),
            description: Some(spec.description.clone()),
            parameters: spec.input_schema.clone(),
            strict: spec.strict,
        }
    }
}

/// Lower an owned [`ToolSpec`] to a wire-format [`crate::ir::ToolDefinition`]
/// by **moving** the fields — avoids the clones that `From<&ToolSpec>`
/// must perform. Use this when the caller no longer needs the original
/// `ToolSpec` after lowering.
impl From<ToolSpec> for crate::ir::ToolDefinition {
    fn from(spec: ToolSpec) -> Self {
        crate::ir::ToolDefinition {
            name: spec.name,
            description: Some(spec.description),
            parameters: spec.input_schema,
            strict: spec.strict,
        }
    }
}

/// Estimate token count for a tool based on name, description, and schema sizes.
///
/// Uses a chars/4 heuristic (roughly 4 characters per token) plus a fixed
/// overhead of 20 tokens for JSON structure.
pub fn estimate_tool_tokens(name: &str, description: &str, schema: &serde_json::Value) -> usize {
    name.len() / 4 + description.len() / 4 + schema.to_string().len() / 4 + 20
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_tool_spec_drops_defer_loading_and_translates_strict() {
        let spec = ToolSpec::new("calc", "calculator", json!({"type": "object"}))
            .strict(true)
            .deferred();
        let ir: crate::ir::ToolDefinition = (&spec).into();
        assert_eq!(ir.name, "calc");
        assert_eq!(ir.description.as_deref(), Some("calculator"));
        assert_eq!(ir.parameters, json!({"type": "object"}));
        assert!(ir.strict);
    }

    #[test]
    fn from_tool_spec_defaults_strict_to_false_when_unset() {
        let spec = ToolSpec::new("calc", "calculator", json!({"type": "object"}));
        let ir: crate::ir::ToolDefinition = (&spec).into();
        assert!(!ir.strict);
    }

    #[test]
    fn owned_from_tool_spec_moves_fields_without_cloning() {
        let spec = ToolSpec::new("calc", "calculator", json!({"type": "object"})).strict(true);
        let ir: crate::ir::ToolDefinition = spec.into();
        assert_eq!(ir.name, "calc");
        assert_eq!(ir.description.as_deref(), Some("calculator"));
        assert!(ir.strict);
    }
}
