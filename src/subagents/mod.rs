//! Subagent system - Progressive Disclosure pattern implementation.
//!
//! This module provides a lazy-loading subagent system that minimizes token usage
//! by storing only metadata in the Task tool description and loading full prompts on-demand.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
//! │  SubagentIndex  │────▶│  IndexRegistry   │────▶│    TaskTool     │
//! │ (metadata only) │     │ <SubagentIndex>  │     │ (lazy loading)  │
//! └─────────────────┘     └──────────────────┘     └─────────────────┘
//!         │                                                 │
//!         ▼                                                 ▼
//! ┌─────────────────┐                              ┌─────────────────┐
//! │  ContentSource  │                              │   Agent spawn   │
//! │ (lazy prompt)   │                              │   (on-demand)   │
//! └─────────────────┘                              └─────────────────┘
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use branchforge::common::{ContentSource, IndexRegistry};
//! use branchforge::subagents::SubagentIndex;
//!
//! // Create subagent with metadata only (prompt loaded lazily)
//! let subagent = SubagentIndex::new("Explore", "Fast codebase exploration")
//!     .source(ContentSource::in_memory("You are an exploration agent..."))
//!     .tools(["Read", "Grep", "Glob"]);
//!
//! // Register in IndexRegistry
//! let mut registry = IndexRegistry::new();
//! registry.register(subagent);
//!
//! // Prompt is loaded only when spawning the agent
//! ```

mod builtin;
mod index;
#[cfg(feature = "cli-integration")]
mod index_loader;

pub use builtin::{
    bash_subagent, builtin_subagents, explore_subagent, find_builtin, general_purpose_subagent,
    plan_subagent,
};
pub use index::SubagentIndex;
#[cfg(feature = "cli-integration")]
pub use index_loader::{SubagentFrontmatter, SubagentIndexLoader};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ModelConfig;
    use crate::common::{ContentSource, SourceType, ToolRestricted};

    #[test]
    fn test_subagent_index() {
        let subagent = SubagentIndex::new("reviewer", "Code reviewer")
            .source(ContentSource::in_memory("Review the code"))
            .source_type(SourceType::Project)
            .tools(["Read", "Grep", "Glob"])
            .model("haiku");

        assert_eq!(subagent.name, "reviewer");
        assert!(subagent.has_tool_restrictions());
        assert!(subagent.is_tool_allowed("Read"));
        assert!(!subagent.is_tool_allowed("Bash"));

        let config = ModelConfig::default();
        assert!(subagent.resolve_model(&config).contains("haiku"));
    }

    #[test]
    fn test_tool_pattern_matching() {
        let subagent = SubagentIndex::new("git-agent", "Git helper")
            .source(ContentSource::in_memory("Help with git"))
            .tools(["Bash(git:*)", "Read"]);

        assert!(subagent.is_tool_allowed("Bash"));
        assert!(subagent.is_tool_allowed("Read"));
        assert!(!subagent.is_tool_allowed("Write"));
    }

    #[test]
    fn test_no_tool_restrictions() {
        let subagent = SubagentIndex::new("general-purpose", "General purpose agent")
            .source(ContentSource::in_memory("Do anything"));

        assert!(!subagent.has_tool_restrictions());
        assert!(subagent.is_tool_allowed("Anything"));
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

        let subagent = SubagentIndex::new("balanced", "Balanced agent")
            .source(ContentSource::in_memory("Be balanced"))
            .model("sonnet");
        assert!(subagent.resolve_model(&config).contains("sonnet"));

        // Test direct model ID passthrough
        let subagent = SubagentIndex::new("custom", "Custom agent")
            .source(ContentSource::in_memory("Custom"))
            .model("claude-custom-model-v1");
        assert_eq!(subagent.resolve_model(&config), "claude-custom-model-v1");
    }

    #[test]
    fn test_resolve_model_with_type() {
        use crate::client::ModelType;

        let config = ModelConfig::default();

        let subagent = SubagentIndex::new("typed", "Typed agent")
            .source(ContentSource::in_memory("Use type"))
            .model_type(ModelType::Small);
        assert!(subagent.resolve_model(&config).contains("haiku"));
    }
}
