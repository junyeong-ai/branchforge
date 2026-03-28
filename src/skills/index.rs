//! Skill Index for Progressive Disclosure.
//!
//! `SkillIndex` contains minimal metadata (name, description, triggers) that is
//! always loaded in the system prompt. The full skill content is loaded on-demand
//! only when the skill is executed.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::common::{ContentSource, Index, Named, SourceType, ToolRestricted};
use crate::hooks::HookRule;

use super::processing;

/// Skill index entry - minimal metadata always available in context.
///
/// This enables the progressive disclosure pattern where:
/// - Metadata (~20 tokens per skill) is always in the system prompt
/// - Full content (~500 tokens per skill) is loaded only when executed
///
/// # Token Efficiency
///
/// With 50 skills:
/// - Index only: 50 × 20 = ~1,000 tokens
/// - Full load: 50 × 500 = ~25,000 tokens
/// - Savings: ~96%
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillIndex {
    /// Skill name (unique identifier)
    pub name: String,

    /// Skill description - used by Claude for semantic matching
    pub description: String,

    /// Trigger keywords for fast matching
    #[serde(default)]
    pub triggers: Vec<String>,

    /// Allowed tools for this skill (if restricted)
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Source location for loading full content
    pub source: ContentSource,

    /// Source type (builtin, user, project, managed)
    #[serde(default)]
    pub source_type: SourceType,

    /// Optional model override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Argument hint for display
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,

    /// When true, the skill is excluded from model-driven discovery and summaries.
    /// Users may still invoke it explicitly via `/skill-name`.
    #[serde(default)]
    pub disable_model_invocation: bool,

    /// Context mode (e.g., "fork" for forked context)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    /// Agent to delegate execution to (e.g., "explore", "plan")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,

    /// Lifecycle hooks (event name → rules)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HashMap<String, Vec<HookRule>>>,

    /// Base directory for relative path resolution (override for InMemory sources)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_dir_override: Option<PathBuf>,
}

impl SkillIndex {
    /// Create a new skill index entry.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            triggers: Vec::new(),
            allowed_tools: Vec::new(),
            source: ContentSource::default(),
            source_type: SourceType::default(),
            model: None,
            argument_hint: None,
            disable_model_invocation: false,
            context: None,
            agent: None,
            hooks: None,
            base_dir_override: None,
        }
    }

    /// Set the base directory for relative path resolution.
    /// This is useful for InMemory sources where the ContentSource doesn't have a file path.
    pub fn with_base_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.base_dir_override = Some(dir.into());
        self
    }

    /// Set triggers for keyword matching.
    pub fn triggers(mut self, triggers: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.triggers = triggers.into_iter().map(Into::into).collect();
        self
    }

    /// Set allowed tools.
    pub fn allowed_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.allowed_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    /// Set the content source.
    pub fn source(mut self, source: ContentSource) -> Self {
        self.source = source;
        self
    }

    /// Set the source type.
    pub fn source_type(mut self, source_type: SourceType) -> Self {
        self.source_type = source_type;
        self
    }

    /// Set the model override.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the argument hint.
    pub fn argument_hint(mut self, hint: impl Into<String>) -> Self {
        self.argument_hint = Some(hint.into());
        self
    }

    /// Check if input matches any trigger keyword.
    pub fn matches_triggers(&self, input: &str) -> bool {
        let input_lower = input.to_lowercase();
        self.triggers
            .iter()
            .any(|trigger| input_lower.contains(&trigger.to_lowercase()))
    }

    /// Check if this is a slash command match (e.g., /skill-name).
    pub fn matches_command(&self, input: &str) -> bool {
        if let Some(cmd) = input.strip_prefix('/') {
            let cmd_lower = cmd.split_whitespace().next().unwrap_or("").to_lowercase();
            self.name.to_lowercase() == cmd_lower
        } else {
            false
        }
    }

    /// Get the base directory for this skill (for relative path resolution).
    /// Checks base_dir_override first, then falls back to ContentSource's base_dir.
    pub fn base_dir(&self) -> Option<PathBuf> {
        self.base_dir_override
            .clone()
            .or_else(|| self.source.base_dir())
    }

    /// Resolve a relative path against this skill's base directory.
    pub fn resolve_path(&self, relative: &str) -> Option<PathBuf> {
        self.base_dir().map(|base| base.join(relative))
    }

    /// Load content with resolved relative paths.
    pub async fn load_content_with_resolved_paths(&self) -> crate::Result<String> {
        let content = self.load_content().await?;

        if let Some(base_dir) = self.base_dir() {
            Ok(processing::resolve_markdown_paths(&content, &base_dir))
        } else {
            Ok(content)
        }
    }

    /// Load skill content for startup preload without executing dynamic content.
    pub async fn load_preloaded_content(&self) -> crate::Result<String> {
        let content = self.load_content_with_resolved_paths().await?;
        Ok(processing::strip_frontmatter(&content).to_string())
    }

    /// Substitute arguments in content.
    ///
    /// Supports: $ARGUMENTS, ${ARGUMENTS}, $1-$9 positional args.
    pub fn substitute_args(content: &str, args: Option<&str>) -> String {
        processing::substitute_args(content, args.unwrap_or(""))
    }

    /// Execute skill with full content processing.
    ///
    /// Processing steps:
    /// 1. Strip frontmatter
    /// 2. Process bash backticks (!`command`)
    /// 3. Process file references (@file.txt)
    /// 4. Resolve markdown paths
    /// 5. Substitute arguments ($ARGUMENTS, $1-$9)
    pub async fn execute(&self, arguments: &str, content: &str) -> String {
        let base_dir = self
            .base_dir()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        // 1. Strip frontmatter
        let content = processing::strip_frontmatter(content);
        // 2. Process bash backticks
        let content = processing::process_bash_backticks(content, &base_dir).await;
        // 3. Process file references
        let content = processing::process_file_references(&content, &base_dir).await;
        // 4. Resolve markdown paths
        let content = processing::resolve_markdown_paths(&content, &base_dir);
        // 5. Substitute arguments
        processing::substitute_args(&content, arguments)
    }
}

impl Named for SkillIndex {
    fn name(&self) -> &str {
        &self.name
    }
}

impl ToolRestricted for SkillIndex {
    fn allowed_tools(&self) -> &[String] {
        &self.allowed_tools
    }
}

#[async_trait]
impl Index for SkillIndex {
    fn source(&self) -> &ContentSource {
        &self.source
    }

    fn source_type(&self) -> SourceType {
        self.source_type
    }

    fn to_summary_line(&self) -> String {
        let tools_str = if self.allowed_tools.is_empty() {
            String::new()
        } else {
            format!(" [tools: {}]", self.allowed_tools.join(", "))
        };

        format!("- {}: {}{}", self.name, self.description, tools_str)
    }

    fn description(&self) -> &str {
        &self.description
    }
}

#[cfg(test)]
mod tests {
    use super::processing;
    use super::*;

    #[test]
    fn test_skill_index_creation() {
        let skill = SkillIndex::new("commit", "Create a git commit with conventional format")
            .triggers(["git commit", "commit changes"])
            .source_type(SourceType::User);

        assert_eq!(skill.name, "commit");
        assert!(skill.matches_triggers("I want to git commit these changes"));
        assert!(!skill.matches_triggers("deploy the application"));
    }

    #[test]
    fn test_command_matching() {
        let skill = SkillIndex::new("commit", "Create a git commit");

        assert!(skill.matches_command("/commit"));
        assert!(skill.matches_command("/commit -m 'message'"));
        assert!(!skill.matches_command("/other"));
        assert!(!skill.matches_command("commit"));
    }

    #[test]
    fn test_summary_line() {
        let skill = SkillIndex::new("test", "A test skill").source_type(SourceType::Project);

        let summary = skill.to_summary_line();
        assert!(summary.contains("test"));
        assert!(summary.contains("A test skill"));
    }

    #[test]
    fn test_summary_line_with_tools() {
        let skill = SkillIndex::new("reader", "Read files only").allowed_tools(["Read", "Grep"]);

        let summary = skill.to_summary_line();
        assert!(summary.contains("[tools: Read, Grep]"));
    }

    #[test]
    fn test_substitute_args() {
        let content = "Do something with $ARGUMENTS and ${ARGUMENTS}";
        let result = SkillIndex::substitute_args(content, Some("test args"));
        assert_eq!(result, "Do something with test args and test args");
    }

    #[tokio::test]
    async fn test_load_content() {
        let skill = SkillIndex::new("test", "Test skill")
            .source(ContentSource::in_memory("Full skill content here"));

        let content = skill.load_content().await.unwrap();
        assert_eq!(content, "Full skill content here");
    }

    #[tokio::test]
    async fn test_load_preloaded_content() {
        let skill = SkillIndex::new("review", "Review code")
            .source(ContentSource::in_memory(
                r#"---
name: review
description: Review code
---
Check [guide](guide.md)
Run !`echo should-not-run`"#,
            ))
            .with_base_dir("/tmp/skills");

        let content = skill.load_preloaded_content().await.unwrap();

        assert!(!content.contains("name: review"));
        assert!(content.contains("[guide](/tmp/skills/guide.md)"));
        assert!(content.contains("!`echo should-not-run`"));
    }

    #[test]
    fn test_priority() {
        let builtin = SkillIndex::new("a", "").source_type(SourceType::Builtin);
        let user = SkillIndex::new("b", "").source_type(SourceType::User);
        let project = SkillIndex::new("c", "").source_type(SourceType::Project);

        assert!(project.priority() > user.priority());
        assert!(user.priority() > builtin.priority());
    }

    #[test]
    fn test_resolve_markdown_paths() {
        let content = r#"# Review Process
Check [style-guide.md](style-guide.md) for standards.
Also see [docs/api.md](docs/api.md).
External: [Rust Docs](https://doc.rust-lang.org)
Absolute: [config](/etc/config.md)"#;

        let resolved =
            processing::resolve_markdown_paths(content, std::path::Path::new("/skills/test"));

        assert!(resolved.contains("[style-guide.md](/skills/test/style-guide.md)"));
        assert!(resolved.contains("[docs/api.md](/skills/test/docs/api.md)"));
        assert!(resolved.contains("[Rust Docs](https://doc.rust-lang.org)"));
        assert!(resolved.contains("[config](/etc/config.md)"));
    }

    #[test]
    fn test_substitute_args_positional() {
        let content = "File: $1, Action: $2, All: $ARGUMENTS";
        let result = SkillIndex::substitute_args(content, Some("main.rs build"));
        assert_eq!(result, "File: main.rs, Action: build, All: main.rs build");
    }

    #[tokio::test]
    async fn test_execute() {
        let skill = SkillIndex::new("test", "Test skill")
            .source(ContentSource::in_memory("Process: $ARGUMENTS"));

        let content = skill.load_content().await.unwrap();
        let result = skill.execute("my argument", &content).await;
        assert_eq!(result.trim(), "Process: my argument");
    }
}
