//! Subagent Index for Progressive Disclosure.
//!
//! `SubagentIndex` contains minimal metadata (name, description, tools) that is
//! always loaded in the Task tool description. The full prompt content is loaded
//! on-demand only when the subagent is spawned.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::client::{ModelConfig, ModelType};
use crate::common::{ContentSource, Index, Named, SourceType, ToolRestricted};
use crate::hooks::HookRule;

/// Subagent index entry - minimal metadata always available in context.
///
/// This enables the progressive disclosure pattern where:
/// - Metadata (~30 tokens per subagent) is always in the Task tool description
/// - Full prompt (~200 tokens per subagent) is loaded only when spawned
///
/// # Token Efficiency
///
/// With 10 subagents:
/// - Index only: 10 × 30 = ~300 tokens
/// - Full load: 10 × 200 = ~2,000 tokens
/// - Savings: ~85%
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentIndex {
    /// Subagent name (unique identifier)
    pub name: String,

    /// Subagent description - used by Claude for semantic matching
    pub description: String,

    /// Allowed tools for this subagent (if restricted)
    #[serde(default, alias = "tools")]
    pub allowed_tools: Vec<String>,

    /// Source location for loading full prompt
    pub source: ContentSource,

    /// Source type (builtin, user, project, managed)
    #[serde(default)]
    pub source_type: SourceType,

    /// Optional model alias or ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Model type for resolution
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_type: Option<ModelType>,

    /// Skills available to this subagent
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,

    /// MCP servers explicitly available to this subagent.
    #[serde(default, alias = "mcpServers", skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<String>,

    /// Tools explicitly disallowed for this subagent
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,

    /// Authorization mode (e.g., "readOnly", "allowAll")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_mode: Option<String>,

    /// Maximum delegated turns before stopping this subagent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<usize>,

    /// Lifecycle hooks (event name → rules)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HashMap<String, Vec<HookRule>>>,
}

impl SubagentIndex {
    /// Create a new subagent index entry.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            allowed_tools: Vec::new(),
            source: ContentSource::default(),
            source_type: SourceType::default(),
            model: None,
            model_type: None,
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            disallowed_tools: Vec::new(),
            authorization_mode: None,
            max_turns: None,
            hooks: None,
        }
    }

    /// Set allowed tools.
    pub fn allowed_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    /// Alias for `allowed_tools()` for convenience.
    pub fn tools(self, t: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_tools(t)
    }

    /// Set the content source (prompt).
    pub fn source(mut self, source: ContentSource) -> Self {
        self.source = source;
        self
    }

    /// Set the source type.
    pub fn source_type(mut self, source_type: SourceType) -> Self {
        self.source_type = source_type;
        self
    }

    /// Set the model alias or ID.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the model type.
    pub fn model_type(mut self, model_type: ModelType) -> Self {
        self.model_type = Some(model_type);
        self
    }

    /// Set available skills.
    pub fn skills(mut self, skills: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.skills = skills.into_iter().map(Into::into).collect();
        self
    }

    /// Set MCP servers available to this subagent.
    pub fn mcp_servers(mut self, servers: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.mcp_servers = servers.into_iter().map(Into::into).collect();
        self
    }

    /// Limit the number of turns this subagent may take.
    pub fn max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = Some(max_turns);
        self
    }

    /// Resolve the model to use for this subagent.
    ///
    /// Supports both direct model IDs and aliases:
    /// - `"opus"` → resolves to reasoning model (e.g., claude-opus-4-6)
    /// - `"sonnet"` → resolves to primary model (e.g., claude-sonnet-4-5)
    /// - `"haiku"` → resolves to small model (e.g., claude-haiku-4-5)
    /// - Direct model ID → passed through unchanged
    ///
    /// Falls back to `model_type` if `model` is not set.
    pub fn resolve_model<'a>(&'a self, config: &'a ModelConfig) -> &'a str {
        if let Some(ref model) = self.model {
            return config.resolve_alias(model);
        }
        config.get(self.model_type.unwrap_or_default())
    }

    /// Load the full prompt content.
    pub async fn load_prompt(&self) -> crate::Result<String> {
        self.source.load().await
    }
}

impl Named for SubagentIndex {
    fn name(&self) -> &str {
        &self.name
    }
}

impl ToolRestricted for SubagentIndex {
    fn allowed_tools(&self) -> &[String] {
        &self.allowed_tools
    }
}

#[async_trait]
impl Index for SubagentIndex {
    fn source(&self) -> &ContentSource {
        &self.source
    }

    fn source_type(&self) -> SourceType {
        self.source_type
    }

    fn to_summary_line(&self) -> String {
        let tools_str = if self.allowed_tools.is_empty() {
            "*".to_string()
        } else {
            self.allowed_tools.join(", ")
        };
        let mcp_str = if self.mcp_servers.is_empty() {
            String::new()
        } else {
            format!("; MCP: {}", self.mcp_servers.join(", "))
        };
        format!(
            "- {}: {} (Tools: {}{})",
            self.name, self.description, tools_str, mcp_str
        )
    }

    fn description(&self) -> &str {
        &self.description
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subagent_index_creation() {
        let subagent = SubagentIndex::new("reviewer", "Code reviewer")
            .source(ContentSource::in_memory("Review the code"))
            .source_type(SourceType::Project)
            .tools(["Read", "Grep", "Glob"])
            .model("haiku");

        assert_eq!(subagent.name, "reviewer");
        assert!(subagent.has_tool_restrictions());
        assert!(subagent.is_tool_allowed("Read"));
        assert!(!subagent.is_tool_allowed("Bash"));
    }

    #[test]
    fn test_summary_line() {
        let subagent = SubagentIndex::new("explore", "Fast codebase exploration")
            .tools(["Read", "Grep", "Glob", "Bash"]);

        let summary = subagent.to_summary_line();
        assert!(summary.contains("explore"));
        assert!(summary.contains("Fast codebase exploration"));
        assert!(summary.contains("Read, Grep, Glob, Bash"));
    }

    #[test]
    fn test_summary_line_no_tools() {
        let subagent = SubagentIndex::new("general", "General purpose agent");
        let summary = subagent.to_summary_line();
        assert!(summary.contains("(Tools: *)"));
    }

    #[test]
    fn test_mcp_servers_builder() {
        let subagent =
            SubagentIndex::new("reviewer", "Code reviewer").mcp_servers(["context7", "filesystem"]);

        assert_eq!(subagent.mcp_servers, vec!["context7", "filesystem"]);
        assert!(
            subagent
                .to_summary_line()
                .contains("MCP: context7, filesystem")
        );
    }

    #[tokio::test]
    async fn test_load_prompt() {
        let subagent = SubagentIndex::new("test", "Test agent")
            .source(ContentSource::in_memory("You are a test agent."));

        let prompt = subagent.load_prompt().await.unwrap();
        assert_eq!(prompt, "You are a test agent.");
    }

    #[test]
    fn test_resolve_model_with_alias() {
        let config = ModelConfig::default();

        let subagent = SubagentIndex::new("fast", "Fast agent")
            .source(ContentSource::in_memory("Be quick"))
            .model("haiku");
        assert!(subagent.resolve_model(&config).contains("haiku"));

        let subagent = SubagentIndex::new("smart", "Smart agent")
            .source(ContentSource::in_memory("Think deep"))
            .model("opus");
        assert!(subagent.resolve_model(&config).contains("opus"));
    }

    #[test]
    fn test_resolve_model_with_type() {
        let config = ModelConfig::default();

        let subagent = SubagentIndex::new("typed", "Typed agent")
            .source(ContentSource::in_memory("Use type"))
            .model_type(ModelType::Small);
        assert!(subagent.resolve_model(&config).contains("haiku"));
    }

    #[test]
    fn test_max_turns_builder() {
        let subagent = SubagentIndex::new("limited", "Limited agent").max_turns(3);
        assert_eq!(subagent.max_turns, Some(3));
    }
}
