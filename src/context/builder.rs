//! Context Builder for Progressive Disclosure

use std::path::{Path, PathBuf};

use crate::client::DEFAULT_MODEL;
use crate::common::IndexRegistry;
use crate::skills::{SkillIndex, build_model_invocable_summary};

use super::ContextResult;
use super::memory_loader::MemoryLoader;
use super::orchestrator::PromptOrchestrator;
use super::rule_index::RuleIndex;
use super::static_context::StaticContext;

pub struct ContextBuilder {
    system_prompt: Option<String>,
    claude_md: Option<String>,
    skill_registry: IndexRegistry<SkillIndex>,
    rule_registry: IndexRegistry<RuleIndex>,
    working_dir: Option<PathBuf>,
    model: String,
}

impl Default for ContextBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextBuilder {
    pub fn new() -> Self {
        Self {
            system_prompt: None,
            claude_md: None,
            skill_registry: IndexRegistry::new(),
            rule_registry: IndexRegistry::new(),
            working_dir: None,
            model: DEFAULT_MODEL.to_string(),
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn claude_md(mut self, content: impl Into<String>) -> Self {
        self.claude_md = Some(content.into());
        self
    }

    pub fn working_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(path.into());
        self
    }

    pub fn skill(mut self, skill: SkillIndex) -> Self {
        self.skill_registry.register(skill);
        self
    }

    pub fn skills(mut self, skills: impl IntoIterator<Item = SkillIndex>) -> Self {
        self.skill_registry.register_all(skills);
        self
    }

    pub fn skill_registry(mut self, registry: IndexRegistry<SkillIndex>) -> Self {
        self.skill_registry = registry;
        self
    }

    pub fn rule(mut self, rule: RuleIndex) -> Self {
        self.rule_registry.register(rule);
        self
    }

    pub fn rules(mut self, rules: impl IntoIterator<Item = RuleIndex>) -> Self {
        self.rule_registry.register_all(rules);
        self
    }

    pub fn rule_registry(mut self, registry: IndexRegistry<RuleIndex>) -> Self {
        self.rule_registry = registry;
        self
    }

    pub async fn load_from_directory(mut self, dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref();
        let loader = MemoryLoader::new();

        if let Ok(content) = loader.load(dir).await {
            let combined = content.combined_claude_md();
            if !combined.is_empty() {
                self.claude_md = Some(match self.claude_md {
                    Some(existing) => format!("{}\n\n{}", existing, combined),
                    None => combined,
                });
            }

            // Register rules from directory
            for rule in content.rule_indices {
                self.rule_registry.register(rule);
            }
        }

        self
    }

    pub fn build(self) -> ContextResult<PromptOrchestrator> {
        let mut static_context = StaticContext::new();

        if let Some(ref prompt) = self.system_prompt {
            static_context = static_context.system_prompt(prompt.clone());
        }

        if let Some(ref md) = self.claude_md {
            static_context = static_context.claude_md(md.clone());
        }

        let skill_summary = self.build_skill_summary();
        if !skill_summary.is_empty() {
            static_context = static_context.skill_summary(skill_summary);
        }

        let rules_summary = self.build_rules_summary();
        if !rules_summary.is_empty() {
            static_context = static_context.rules_summary(rules_summary);
        }

        let orchestrator = PromptOrchestrator::new(static_context, &self.model)
            .rule_registry(self.rule_registry)
            .skill_registry(self.skill_registry);

        Ok(orchestrator)
    }

    fn build_skill_summary(&self) -> String {
        let summary = build_model_invocable_summary(&self.skill_registry);
        if summary.is_empty() {
            return String::new();
        }
        format!("# Available Skills\n{summary}")
    }

    fn build_rules_summary(&self) -> String {
        let summary = self.rule_registry.build_priority_summary();
        if summary.is_empty() {
            return String::new();
        }
        format!("# Available Rules\n{summary}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_builder_basic() {
        let orchestrator = ContextBuilder::new()
            .system_prompt("You are helpful")
            .claude_md("# Project\nA test project")
            .model("claude-sonnet-4-5")
            .build()
            .unwrap();

        let static_context = orchestrator.static_context();
        assert!(static_context.system_prompt.contains("helpful"));
        assert!(static_context.claude_md.contains("test project"));
    }

    #[test]
    fn test_context_builder_with_skills() {
        let skill = SkillIndex::new("test", "A test skill");

        let orchestrator = ContextBuilder::new().skill(skill).build().unwrap();

        assert!(!orchestrator.static_context().skill_summary.is_empty());
    }

    #[test]
    fn test_context_builder_filters_manual_only_skills_from_summary() {
        let mut skill = SkillIndex::new("internal", "Manual-only skill");
        skill.disable_model_invocation = true;

        let orchestrator = ContextBuilder::new().skill(skill).build().unwrap();
        assert!(orchestrator.static_context().skill_summary.is_empty());
    }

    #[tokio::test]
    async fn test_load_from_directory() {
        use tempfile::tempdir;
        use tokio::fs;

        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "# Test Project")
            .await
            .unwrap();

        let rules_dir = dir.path().join(".claude").join("rules");
        fs::create_dir_all(&rules_dir).await.unwrap();
        fs::write(
            rules_dir.join("test.md"),
            r#"---
paths: **/*.rs
---

# Test Rule"#,
        )
        .await
        .unwrap();

        let orchestrator = ContextBuilder::new()
            .load_from_directory(dir.path())
            .await
            .build()
            .unwrap();

        assert!(
            orchestrator
                .static_context()
                .claude_md
                .contains("Test Project")
        );
        // Rules are now in the rule_registry, not a separate engine
        // We can verify through the rules summary in static context
        assert!(!orchestrator.static_context().rules_summary.is_empty());
    }

    #[tokio::test]
    async fn test_rule_registry_integration() {
        use crate::common::ContentSource;

        let rule = RuleIndex::new("test-rule")
            .description("Test description")
            .paths(vec!["**/*.rs".into()])
            .source(ContentSource::in_memory("Rule content"));

        let orchestrator = ContextBuilder::new().rule(rule).build().unwrap();

        // Check that rule is in the registry
        let registry = orchestrator.get_rule_registry().await;
        assert!(registry.contains("test-rule"));
    }
}
