//! Skill system - Progressive Disclosure pattern implementation.
//!
//! This module provides a lazy-loading skill system that minimizes token usage
//! by storing only metadata in the system prompt and loading full content on-demand.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
//! │   SkillIndex    │────▶│  IndexRegistry   │────▶│   SkillRuntime  │
//! │ (metadata only) │     │ <I: Index>       │     │ (lazy loading)  │
//! └─────────────────┘     └──────────────────┘     └─────────────────┘
//!         │                        │                        │
//!         ▼                        ▼                        ▼
//! ┌─────────────────┐      ┌──────────────────┐    ┌─────────────────┐
//! │  ContentSource  │      │  Priority-based  │    │    SkillSpec    │
//! │ (lazy content)  │      │    Override      │    │ + invocation     │
//! └─────────────────┘      └──────────────────┘    └─────────────────┘
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use branchforge::common::{ContentSource, IndexRegistry};
//! use branchforge::skills::{SkillIndex, SkillRuntime};
//!
//! // Create skill with metadata only (content loaded lazily)
//! let skill = SkillIndex::new("commit", "Create git commits")
//!     .source(ContentSource::in_memory("Analyze and commit: $ARGUMENTS"))
//!     .triggers(["/commit"]);
//!
//! // Register in IndexRegistry
//! let mut registry = IndexRegistry::new();
//! registry.register(skill);
//!
//! // Execute loads content on-demand
//! let executor = SkillRuntime::new(registry);
//! let result = executor.execute("commit", Some("fix bug")).await;
//! ```

mod index;
mod index_loader;
mod processing;
mod runtime;
mod skill_tool;

pub use index::SkillIndex;
pub use index_loader::{SkillFrontmatter, SkillIndexLoader};
pub use runtime::{SkillExecutionKind, SkillRuntime, SkillSpec};
pub use skill_tool::{SkillInput, SkillTool};

use std::path::PathBuf;

use crate::common::{Index, IndexRegistry};
use serde::{Deserialize, Serialize};

pub fn list_model_invocable_skills(registry: &IndexRegistry<SkillIndex>) -> Vec<&SkillIndex> {
    registry
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect()
}

pub fn find_explicit_command<'a>(
    registry: &'a IndexRegistry<SkillIndex>,
    input: &str,
) -> Option<&'a SkillIndex> {
    let trimmed = input.trim_start();
    registry.iter().find(|skill| skill.matches_command(trimmed))
}

pub fn parse_explicit_command(
    registry: &IndexRegistry<SkillIndex>,
    input: &str,
) -> Option<(String, Option<String>)> {
    let trimmed = input.trim_start();
    let skill = find_explicit_command(registry, trimmed)?;
    let command = trimmed.strip_prefix('/')?;
    let mut parts = command.trim_start().splitn(2, char::is_whitespace);
    let _command_name = parts.next()?;
    let args = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Some((skill.name.clone(), args))
}

pub fn find_trigger_matches<'a>(
    registry: &'a IndexRegistry<SkillIndex>,
    input: &str,
) -> Vec<&'a SkillIndex> {
    list_model_invocable_skills(registry)
        .into_iter()
        .filter(|skill| skill.matches_triggers(input))
        .collect()
}

pub fn find_first_trigger_match<'a>(
    registry: &'a IndexRegistry<SkillIndex>,
    input: &str,
) -> Option<&'a SkillIndex> {
    find_trigger_matches(registry, input).into_iter().next()
}

pub fn extract_trigger_args(input: &str, skill: &SkillIndex) -> Option<String> {
    let input_lower = input.to_lowercase();
    for trigger in &skill.triggers {
        let trigger_lower = trigger.to_lowercase();
        if let Some(byte_pos) = input_lower.find(&trigger_lower) {
            let end_byte = byte_pos + trigger_lower.len();
            if end_byte <= input.len() && input.is_char_boundary(end_byte) {
                let after_trigger = input[end_byte..].trim();
                if !after_trigger.is_empty() {
                    return Some(after_trigger.to_string());
                }
            }
        }
    }
    None
}

pub fn build_model_invocable_summary(registry: &IndexRegistry<SkillIndex>) -> String {
    let mut lines: Vec<_> = list_model_invocable_skills(registry)
        .into_iter()
        .map(|skill| skill.to_summary_line())
        .collect();
    lines.sort();
    lines.join("\n")
}

/// Result of skill execution.
///
/// Contains the output, error status, and any context from the executed skill
/// such as tool restrictions or model override.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    #[serde(default)]
    pub execution_kind: SkillExecutionKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl SkillResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            error: None,
            execution_kind: SkillExecutionKind::Inline,
            allowed_tools: Vec::new(),
            model: None,
            base_dir: None,
            agent: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(message.into()),
            execution_kind: SkillExecutionKind::Inline,
            allowed_tools: Vec::new(),
            model: None,
            base_dir: None,
            agent: None,
        }
    }

    pub fn execution_kind(mut self, kind: SkillExecutionKind) -> Self {
        self.execution_kind = kind;
        self
    }

    pub fn allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    pub fn model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    pub fn base_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.base_dir = dir;
        self
    }

    pub fn agent(mut self, agent: Option<String>) -> Self {
        self.agent = agent;
        self
    }

    pub fn has_tool_restrictions(&self) -> bool {
        !self.allowed_tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{ContentSource, SourceType, ToolRestricted};

    #[test]
    fn test_skill_definition() {
        let skill = SkillIndex::new("commit", "Create a git commit")
            .source(ContentSource::in_memory(
                "Analyze changes and create commit message",
            ))
            .source_type(SourceType::Builtin)
            .triggers(["/commit"]);

        assert_eq!(skill.name, "commit");
        assert_eq!(skill.source_type, SourceType::Builtin);
        assert!(skill.matches_triggers("/commit please"));
    }

    #[test]
    fn test_skill_result() {
        let success = SkillResult::success("Done!");
        assert!(success.success);
        assert!(success.error.is_none());

        let error = SkillResult::error("Failed");
        assert!(!error.success);
        assert!(error.error.is_some());
    }

    #[test]
    fn test_skill_allowed_tools() {
        let skill = SkillIndex::new("reader", "Read files")
            .source(ContentSource::in_memory("Read: $ARGUMENTS"))
            .allowed_tools(["Read", "Grep", "Glob"]);

        assert!(skill.has_tool_restrictions());
        assert!(skill.is_tool_allowed("Read"));
        assert!(skill.is_tool_allowed("Grep"));
        assert!(!skill.is_tool_allowed("Bash"));
        assert!(!skill.is_tool_allowed("Write"));
    }

    #[test]
    fn test_skill_allowed_tools_pattern() {
        let skill = SkillIndex::new("git-helper", "Git commands")
            .source(ContentSource::in_memory("Git: $ARGUMENTS"))
            .allowed_tools(["Bash(git:*)", "Read"]);

        assert!(skill.is_tool_allowed("Bash")); // Base tool name
        assert!(skill.is_tool_allowed("Read"));
        assert!(!skill.is_tool_allowed("Write"));
    }

    #[test]
    fn test_skill_no_restrictions() {
        let skill =
            SkillIndex::new("any", "Any tools").source(ContentSource::in_memory("Do: $ARGUMENTS"));

        assert!(!skill.has_tool_restrictions());
        assert!(skill.is_tool_allowed("Bash"));
        assert!(skill.is_tool_allowed("Read"));
        assert!(skill.is_tool_allowed("Anything"));
    }

    #[test]
    fn test_skill_model_override() {
        let skill = SkillIndex::new("fast-task", "Quick task")
            .source(ContentSource::in_memory("Do: $ARGUMENTS"))
            .model("claude-haiku-4-5-20251001");

        assert_eq!(skill.model, Some("claude-haiku-4-5-20251001".to_string()));
    }

    #[test]
    fn test_skill_result_with_context() {
        let result = SkillResult::success("Output")
            .allowed_tools(vec!["Read".to_string(), "Grep".to_string()])
            .model(Some("claude-haiku-4-5-20251001".to_string()));

        assert!(result.has_tool_restrictions());
        assert_eq!(result.allowed_tools, vec!["Read", "Grep"]);
        assert_eq!(result.model, Some("claude-haiku-4-5-20251001".to_string()));
    }

    #[test]
    fn test_skill_base_dir() {
        let skill = SkillIndex::new("reviewer", "Review code")
            .source(ContentSource::file(
                "/home/user/.claude/skills/reviewer/skill.md",
            ))
            .base_dir("/home/user/.claude/skills/reviewer");

        assert_eq!(
            skill.resolve_path("style-guide.md"),
            Some(PathBuf::from(
                "/home/user/.claude/skills/reviewer/style-guide.md"
            ))
        );
    }

    #[tokio::test]
    async fn test_content_with_resolved_paths() {
        let content = r#"# Review Process
Check [style-guide.md](style-guide.md) for standards.
Also see [docs/api.md](docs/api.md).
External: [Rust Docs](https://doc.rust-lang.org)
Absolute: [config](/etc/config.md)"#;

        let skill = SkillIndex::new("test", "Test")
            .source(ContentSource::in_memory(content))
            .base_dir("/skills/test");

        let resolved = skill.load_content_with_resolved_paths().await.unwrap();

        assert!(resolved.contains("[style-guide.md](/skills/test/style-guide.md)"));
        assert!(resolved.contains("[docs/api.md](/skills/test/docs/api.md)"));
        assert!(resolved.contains("[Rust Docs](https://doc.rust-lang.org)"));
        assert!(resolved.contains("[config](/etc/config.md)"));
    }

    #[tokio::test]
    async fn test_content_without_base_dir() {
        let skill = SkillIndex::new("test", "Test")
            .source(ContentSource::in_memory("See [file.md](file.md)"));
        let resolved = skill.load_content_with_resolved_paths().await.unwrap();
        assert_eq!(resolved, "See [file.md](file.md)");
    }
}
