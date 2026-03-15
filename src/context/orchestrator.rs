//! Context Orchestrator - Progressive Disclosure Engine
//!
//! Manages three-layer context loading:
//! 1. Static context (always loaded, cached)
//! 2. Context-aware loading (rules based on file path)
//! 3. On-demand loading (explicit skill/rule requests)

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::common::{IndexRegistry, LoadedEntry};
use crate::session::compact::DEFAULT_COMPACT_THRESHOLD;
use crate::skills::SkillIndex;
use crate::skills::{build_model_invocable_summary, find_explicit_command, find_trigger_matches};
use crate::types::{TokenUsage, context_window};

use super::rule_index::RuleIndex;
use super::static_context::StaticContext;

pub struct PromptOrchestrator {
    static_context: StaticContext,
    skill_registry: IndexRegistry<SkillIndex>,
    rule_registry: Arc<RwLock<IndexRegistry<RuleIndex>>>,
    model: String,
    current_input_tokens: u64,
    compact_threshold: f32,
    current_file: Option<PathBuf>,
    active_rule_names: Arc<RwLock<HashSet<String>>>,
}

impl PromptOrchestrator {
    pub fn new(static_context: StaticContext, model: &str) -> Self {
        Self {
            static_context,
            skill_registry: IndexRegistry::new(),
            rule_registry: Arc::new(RwLock::new(IndexRegistry::new())),
            model: model.to_string(),
            current_input_tokens: 0,
            compact_threshold: DEFAULT_COMPACT_THRESHOLD,
            current_file: None,
            active_rule_names: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Set the rule registry with pre-populated rules.
    pub fn rule_registry(mut self, registry: IndexRegistry<RuleIndex>) -> Self {
        self.rule_registry = Arc::new(RwLock::new(registry));
        self
    }

    pub fn skill_registry(mut self, registry: IndexRegistry<SkillIndex>) -> Self {
        self.skill_registry = registry;
        self
    }

    pub fn static_context(&self) -> &StaticContext {
        &self.static_context
    }

    pub fn static_context_mut(&mut self) -> &mut StaticContext {
        &mut self.static_context
    }

    /// Get read access to the rule registry.
    pub async fn get_rule_registry(
        &self,
    ) -> tokio::sync::RwLockReadGuard<'_, IndexRegistry<RuleIndex>> {
        self.rule_registry.read().await
    }

    pub fn get_skill_registry(&self) -> &IndexRegistry<SkillIndex> {
        &self.skill_registry
    }

    pub fn max_tokens(&self) -> u64 {
        context_window::for_model(&self.model)
    }

    pub fn current_input_tokens(&self) -> u64 {
        self.current_input_tokens
    }

    pub fn update_usage(&mut self, usage: &TokenUsage) {
        self.current_input_tokens = usage.input_tokens;
    }

    pub fn needs_compact(&self) -> bool {
        let ratio = self.current_input_tokens as f32 / self.max_tokens() as f32;
        ratio > self.compact_threshold
    }

    pub fn available_tokens(&self) -> u64 {
        let threshold = (self.max_tokens() as f32 * self.compact_threshold) as u64;
        threshold.saturating_sub(self.current_input_tokens)
    }

    pub fn usage_percent(&self) -> f32 {
        (self.current_input_tokens as f32 / self.max_tokens() as f32) * 100.0
    }

    pub fn set_current_file(&mut self, path: impl AsRef<Path>) {
        self.current_file = Some(path.as_ref().to_path_buf());
    }

    pub fn current_file(&self) -> Option<&Path> {
        self.current_file.as_deref()
    }

    /// Get loaded rules for the current file.
    pub async fn get_rules_for_current_file(&self) -> Vec<LoadedEntry<RuleIndex>> {
        match &self.current_file {
            Some(path) => {
                let registry = self.rule_registry.read().await;
                registry.load_matching(path).await
            }
            None => Vec::new(),
        }
    }

    /// Get loaded rules for a specific path.
    pub async fn get_rules_for_path(&self, path: &Path) -> Vec<LoadedEntry<RuleIndex>> {
        let registry = self.rule_registry.read().await;
        registry.load_matching(path).await
    }

    /// Activate rules for a file and track their names.
    pub async fn activate_rules_for_file(&self, path: &Path) -> Vec<LoadedEntry<RuleIndex>> {
        let registry = self.rule_registry.read().await;
        let rules = registry.load_matching(path).await;
        let mut active = self.active_rule_names.write().await;
        for rule in &rules {
            active.insert(rule.index.name.clone());
        }
        rules
    }

    pub async fn active_rule_names(&self) -> Vec<String> {
        let active = self.active_rule_names.read().await;
        active.iter().cloned().collect()
    }

    /// Build dynamic context for a file path using matching rules.
    pub async fn build_dynamic_context(&self, file_path: Option<&Path>) -> String {
        let Some(path) = file_path else {
            return String::new();
        };

        let registry = self.rule_registry.read().await;
        let rules = registry.load_matching(path).await;
        if rules.is_empty() {
            return String::new();
        }

        let mut parts = Vec::with_capacity(rules.len() + 1);
        parts.push(format!("# Active Rules for {}\n", path.display()));
        for rule in rules {
            parts.push(format!("## {}\n{}", rule.index.name, rule.content));
        }

        parts.join("\n\n")
    }

    /// Find rules matching a path (indices only, no content loading).
    pub async fn find_matching_rules(&self, path: &Path) -> Vec<RuleIndex> {
        let registry = self.rule_registry.read().await;
        registry.find_matching(path).into_iter().cloned().collect()
    }

    /// Check if any rules match a path.
    pub async fn has_matching_rules(&self, path: &Path) -> bool {
        let registry = self.rule_registry.read().await;
        registry.has_matching(path)
    }

    pub fn find_skills_by_triggers(&self, input: &str) -> Vec<&SkillIndex> {
        find_trigger_matches(&self.skill_registry, input)
    }

    pub fn find_skill_by_command(&self, input: &str) -> Option<&SkillIndex> {
        find_explicit_command(&self.skill_registry, input)
    }

    pub fn build_skill_summary(&self) -> String {
        let summary = build_model_invocable_summary(&self.skill_registry);
        if summary.is_empty() {
            return String::new();
        }
        format!("# Available Skills\n{summary}")
    }

    /// Build a summary of all registered rules.
    pub async fn build_rules_summary(&self) -> String {
        let registry = self.rule_registry.read().await;
        let summary = registry.build_priority_summary();
        if summary.is_empty() {
            return String::new();
        }
        format!("# Available Rules\n{summary}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ContentSource;

    #[test]
    fn test_orchestrator_creation() {
        let static_context = StaticContext::new().system_prompt("Hello");
        let orchestrator = PromptOrchestrator::new(static_context, "claude-sonnet-4-5");

        assert_eq!(orchestrator.max_tokens(), 200_000);
    }

    #[test]
    fn test_token_tracking() {
        let static_context = StaticContext::new();
        let mut orchestrator = PromptOrchestrator::new(static_context, "claude-sonnet-4-5");

        orchestrator.update_usage(&TokenUsage {
            input_tokens: 100_000,
            output_tokens: 500,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        });

        assert!(!orchestrator.needs_compact());
        assert_eq!(orchestrator.usage_percent(), 50.0);

        orchestrator.update_usage(&TokenUsage {
            input_tokens: 170_000,
            output_tokens: 500,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        });

        assert!(orchestrator.needs_compact());
    }

    #[tokio::test]
    async fn test_rules_for_path() {
        let mut rule_registry = IndexRegistry::new();
        rule_registry.register(
            RuleIndex::new("rust")
                .paths(vec!["**/*.rs".into()])
                .source(ContentSource::in_memory("Use snake_case")),
        );
        rule_registry
            .register(RuleIndex::new("global").source(ContentSource::in_memory("Be helpful")));

        let static_context = StaticContext::new();
        let orchestrator = PromptOrchestrator::new(static_context, "claude-sonnet-4-5")
            .rule_registry(rule_registry);

        let rules = orchestrator
            .get_rules_for_path(Path::new("src/lib.rs"))
            .await;
        assert_eq!(rules.len(), 2);

        let rules = orchestrator
            .get_rules_for_path(Path::new("src/lib.ts"))
            .await;
        assert_eq!(rules.len(), 1); // Only global rule matches
    }

    #[tokio::test]
    async fn test_find_matching_rules() {
        let mut rule_registry = IndexRegistry::new();
        rule_registry.register(
            RuleIndex::new("rust")
                .paths(vec!["**/*.rs".into()])
                .source(ContentSource::in_memory("Rust rules")),
        );

        let static_context = StaticContext::new();
        let orchestrator = PromptOrchestrator::new(static_context, "claude-sonnet-4-5")
            .rule_registry(rule_registry);

        let rules = orchestrator
            .find_matching_rules(Path::new("src/lib.rs"))
            .await;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "rust");

        assert!(
            orchestrator
                .has_matching_rules(Path::new("src/lib.rs"))
                .await
        );
        assert!(
            !orchestrator
                .has_matching_rules(Path::new("src/lib.ts"))
                .await
        );
    }

    #[test]
    fn test_skill_registry_integration() {
        let static_context = StaticContext::new();
        let mut skill_registry = IndexRegistry::new();
        skill_registry.register(
            SkillIndex::new("test", "A test skill")
                .source(ContentSource::in_memory("Test content")),
        );

        let orchestrator = PromptOrchestrator::new(static_context, "claude-sonnet-4-5")
            .skill_registry(skill_registry);

        assert!(orchestrator.get_skill_registry().contains("test"));
    }

    #[test]
    fn test_build_skill_summary() {
        let static_context = StaticContext::new();
        let mut skill_registry = IndexRegistry::new();
        skill_registry.register(SkillIndex::new("commit", "Create git commits"));
        skill_registry.register(SkillIndex::new("review", "Review code"));

        let orchestrator = PromptOrchestrator::new(static_context, "claude-sonnet-4-5")
            .skill_registry(skill_registry);

        let summary = orchestrator.build_skill_summary();
        assert!(summary.contains("commit"));
        assert!(summary.contains("review"));
    }

    #[test]
    fn test_find_skill_by_command_includes_manual_only_skills() {
        let static_context = StaticContext::new();
        let mut skill_registry = IndexRegistry::new();
        let mut skill = SkillIndex::new("internal", "Internal manual-only skill");
        skill.disable_model_invocation = true;
        skill_registry.register(skill);

        let orchestrator = PromptOrchestrator::new(static_context, "claude-sonnet-4-5")
            .skill_registry(skill_registry);

        let skill = orchestrator.find_skill_by_command("/internal");
        assert!(skill.is_some());
        assert_eq!(skill.unwrap().name, "internal");
        assert!(orchestrator.build_skill_summary().is_empty());
    }

    #[tokio::test]
    async fn test_build_rules_summary() {
        let mut rule_registry = IndexRegistry::new();
        rule_registry.register(
            RuleIndex::new("security")
                .description("Security best practices")
                .source(ContentSource::in_memory("content")),
        );

        let static_context = StaticContext::new();
        let orchestrator = PromptOrchestrator::new(static_context, "claude-sonnet-4-5")
            .rule_registry(rule_registry);

        let summary = orchestrator.build_rules_summary().await;
        assert!(summary.contains("security"));
        assert!(summary.contains("Security best practices"));
    }
}
