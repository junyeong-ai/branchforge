//! CLI integration methods for AgentBuilder.
//!
//! Resource loading follows a fixed order regardless of method call sequence:
//! Enterprise → User → Project → Local (later levels override earlier).

use std::path::Path;

#[cfg(feature = "cli-auth")]
use crate::auth::Auth;
use crate::authorization::ToolPolicy;
use crate::common::{IndexRegistry, Named, Provider};
use crate::config::{Settings, SettingsLoader};
use crate::context::{LeveledMemoryProvider, MemoryLoader, enterprise_base_path, user_base_path};
use crate::hooks::CommandHook;
use crate::output_style::file_output_style_provider;
use crate::skills::SkillIndexLoader;
use crate::subagents::{SubagentIndexLoader, builtin_subagents};

use super::builder::AgentBuilder;

impl AgentBuilder {
    /// Initializes Claude Code CLI authentication and working directory.
    ///
    /// This is the minimal setup. Use `with_*_resources()` methods to enable
    /// loading from specific levels. Resources are loaded during `build()` in
    /// a fixed order: Enterprise → User → Project → Local.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use branchforge::Agent;
    /// # async fn example() -> branchforge::Result<()> {
    /// let agent = Agent::builder()
    ///     .from_claude_cli_workspace("./project").await?
    ///     .user_resources()
    ///     .project_resources()
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "cli-auth")]
    pub async fn from_claude_cli_workspace(
        mut self,
        path: impl AsRef<Path>,
    ) -> crate::Result<Self> {
        let path = path.as_ref();
        self = self.auth(Auth::ClaudeCli).await?;
        self.config.working_dir = Some(path.to_path_buf());
        Ok(self)
    }

    /// Enables loading of enterprise-level resources during build.
    ///
    /// Resources are loaded from system configuration paths:
    /// - macOS: `/Library/Application Support/ClaudeCode/`
    /// - Linux: `/etc/claude-code/`
    ///
    /// This method only sets a flag; actual loading happens during `build()`
    /// in the fixed order: Enterprise → User → Project → Local.
    pub fn enterprise_resources(mut self) -> Self {
        self.load_enterprise = true;
        self
    }

    /// Enables loading of user-level resources during build.
    ///
    /// Resources are loaded from `~/.claude/`.
    ///
    /// This method only sets a flag; actual loading happens during `build()`
    /// in the fixed order: Enterprise → User → Project → Local.
    pub fn user_resources(mut self) -> Self {
        self.load_user = true;
        self
    }

    /// Enables loading of project-level resources during build.
    ///
    /// Resources are loaded from `{working_dir}/.claude/`.
    /// Requires `from_claude_cli_workspace()` to be called first to set the working directory.
    ///
    /// This method only sets a flag; actual loading happens during `build()`
    /// in the fixed order: Enterprise → User → Project → Local.
    pub fn project_resources(mut self) -> Self {
        self.load_project = true;
        self
    }

    /// Enables loading of local-level resources during build.
    ///
    /// Loads CLAUDE.local.md and settings.local.json from the project directory.
    /// These are typically gitignored and contain personal/machine-specific settings.
    /// Requires `from_claude_cli_workspace()` to be called first to set the working directory.
    ///
    /// This method only sets a flag; actual loading happens during `build()`
    /// in the fixed order: Enterprise → User → Project → Local.
    pub fn local_resources(mut self) -> Self {
        self.load_local = true;
        self
    }

    // =========================================================================
    // Internal resource loading methods (called by build.rs in fixed order)
    // =========================================================================

    pub(super) async fn load_enterprise_resources(&mut self) {
        let Some(base) = enterprise_base_path() else {
            return;
        };

        self.load_settings_from(&base).await;
        self.load_skills_from(&base).await;
        self.load_subagents_from(&base).await;
        self.load_output_styles_from(&base).await;
        self.load_memory_from(&base).await;
    }

    pub(super) async fn load_user_resources(&mut self) {
        let Some(base) = user_base_path() else {
            return;
        };

        self.load_settings_from(&base).await;
        self.load_skills_from(&base).await;
        self.load_subagents_from(&base).await;
        self.load_output_styles_from(&base).await;
        self.load_memory_from(&base).await;
    }

    pub(super) async fn load_project_resources(&mut self) {
        let Some(working_dir) = self.config.working_dir.clone() else {
            tracing::warn!("working_dir not set, call from_claude_cli_workspace() first");
            return;
        };

        self.load_settings_from(&working_dir).await;
        self.load_skills_from(&working_dir).await;
        self.load_subagents_from(&working_dir).await;
        self.load_output_styles_from(&working_dir).await;
        self.load_memory_from(&working_dir).await;
    }

    pub(super) async fn load_local_resources(&mut self) {
        let Some(working_dir) = self.config.working_dir.clone() else {
            tracing::warn!("working_dir not set, call from_claude_cli_workspace() first");
            return;
        };

        let mut settings_loader = SettingsLoader::new();
        if settings_loader.load_local(&working_dir).await.is_ok() {
            let settings = settings_loader.into_settings();
            self.apply_settings_mut(&settings);
        }

        let loader = MemoryLoader::new();
        if let Ok(content) = loader.load_local(&working_dir).await
            && !content.local_md.is_empty()
        {
            let provider = self
                .memory_provider
                .get_or_insert_with(LeveledMemoryProvider::new);
            provider.add_memory_content(content);
        }
    }

    // =========================================================================
    // Helper methods for resource loading
    // =========================================================================

    async fn load_settings_from(&mut self, base: &Path) {
        let mut loader = SettingsLoader::new();
        if loader.load_from(base).await.is_ok() {
            let settings = loader.into_settings();
            self.apply_settings_mut(&settings);
        }
    }

    async fn load_skills_from(&mut self, base: &Path) {
        let skills_dir = base.join(".claude").join("skills");
        let loader = SkillIndexLoader::new();
        if let Ok(skills) = loader.scan_directory(&skills_dir).await {
            let count = skills.len();
            for skill in skills {
                tracing::debug!(skill_name = %skill.name, "Registering skill to builder");
                self.skill_registry
                    .get_or_insert_with(IndexRegistry::new)
                    .register(skill);
            }
            tracing::info!(skill_count = count, "Skills loaded from project");
        }
    }

    async fn load_subagents_from(&mut self, base: &Path) {
        let loader = SubagentIndexLoader::new();
        let subagents_dir = base.join(".claude").join("subagents");
        if let Ok(subagents) = loader.scan_directory(&subagents_dir).await {
            for subagent in subagents {
                self.subagent_registry
                    .get_or_insert_with(|| {
                        let mut registry = IndexRegistry::new();
                        registry.register_all(builtin_subagents());
                        registry
                    })
                    .register(subagent);
            }
        }
    }

    async fn load_output_styles_from(&mut self, base: &Path) {
        let provider = file_output_style_provider().project_path(base);
        if let Ok(styles) = provider.load_all().await
            && let Some(first) = styles.first()
            && self.output_style_name.is_none()
        {
            self.output_style_name = Some(first.name().to_string());
        }
    }

    async fn load_memory_from(&mut self, base: &Path) {
        let loader = MemoryLoader::new();
        if let Ok(content) = loader.load_shared(base).await
            && !content.is_empty()
        {
            let provider = self
                .memory_provider
                .get_or_insert_with(LeveledMemoryProvider::new);
            provider.add_memory_content(content);
        }
    }

    /// Apply settings mutably (for internal use by load methods).
    fn apply_settings_mut(&mut self, settings: &Settings) {
        if let Some(model) = &settings.model {
            self.config.model.primary = model.clone();
        }
        if let Some(small) = &settings.small_model {
            self.config.model.small = small.clone();
        }
        if let Some(max_tokens) = settings.max_tokens {
            self.config.model.max_tokens = max_tokens;
        }

        if let Some(ref hooks_settings) = settings.hooks {
            for hook in CommandHook::from_settings(hooks_settings) {
                self.hooks.register(hook);
            }
        }

        self.config.security.env.extend(settings.env.clone());

        if !settings.authorization.is_empty() {
            self.authorization_policy_explicit = true;
            let loaded_policy = settings.authorization.to_policy();
            let existing_policy = std::mem::take(&mut self.config.security.authorization_policy);
            self.config.security.authorization_policy =
                Self::merge_permission_policies(existing_policy, loaded_policy);
        }

        if settings.sandbox.is_enabled() || settings.sandbox.has_network_settings() {
            self.sandbox_settings = Some(settings.sandbox.clone());
        }

        if let Some(ref style_name) = settings.output_style {
            self.output_style_name = Some(style_name.clone());
        }

        for (name, config_value) in &settings.mcp_servers {
            if let Ok(config) = serde_json::from_value(config_value.clone()) {
                self.mcp_configs.insert(name.clone(), config);
            }
        }

        // Apply tool search settings
        if !settings.tool_search.is_empty() && settings.tool_search.is_enabled() {
            let context_window =
                crate::types::context_window::for_model(&self.config.model.primary) as usize;
            let config = settings.tool_search.to_config(context_window);
            self.tool_search_config = Some(config);
        }
    }

    /// Apply settings (for test use, returns Self for chaining).
    #[cfg(test)]
    pub(super) fn apply_settings(mut self, settings: Settings) -> Self {
        self.apply_settings_mut(&settings);
        self
    }

    fn merge_permission_policies(
        from_settings: ToolPolicy,
        programmatic: ToolPolicy,
    ) -> ToolPolicy {
        let mut rules = from_settings.rules;
        rules.extend(programmatic.rules);

        let mut tool_limits = from_settings.tool_limits;
        tool_limits.extend(programmatic.tool_limits);

        let mut policy = ToolPolicy::new();
        policy.rules = rules;
        policy.tool_limits = tool_limits;
        policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_apply_values() {
        let settings = Settings {
            model: Some("settings-model".to_string()),
            small_model: Some("settings-small".to_string()),
            max_tokens: Some(1000),
            ..Default::default()
        };

        let builder = AgentBuilder::new().apply_settings(settings);

        assert_eq!(builder.config.model.primary, "settings-model");
        assert_eq!(builder.config.model.small, "settings-small");
        assert_eq!(builder.config.model.max_tokens, 1000);
    }

    #[test]
    fn test_explicit_config_after_settings_overrides() {
        let settings = Settings {
            model: Some("settings-model".to_string()),
            small_model: Some("settings-small".to_string()),
            max_tokens: Some(1000),
            ..Default::default()
        };

        let builder = AgentBuilder::new()
            .apply_settings(settings)
            .model("explicit-model")
            .small_model("explicit-small")
            .max_tokens(2000);

        assert_eq!(builder.config.model.primary, "explicit-model");
        assert_eq!(builder.config.model.small, "explicit-small");
        assert_eq!(builder.config.model.max_tokens, 2000);
    }

    #[test]
    fn test_settings_cascade_order() {
        // Test that settings are applied in order: enterprise → user → project
        // Later settings override earlier ones
        let enterprise = Settings {
            model: Some("enterprise-model".to_string()),
            small_model: Some("enterprise-small".to_string()),
            ..Default::default()
        };
        let user = Settings {
            model: Some("user-model".to_string()),
            ..Default::default()
        };
        let project = Settings {
            small_model: Some("project-small".to_string()),
            ..Default::default()
        };

        let builder = AgentBuilder::new()
            .apply_settings(enterprise)
            .apply_settings(user)
            .apply_settings(project);

        // user-model overrides enterprise-model
        assert_eq!(builder.config.model.primary, "user-model");
        // project-small overrides enterprise-small
        assert_eq!(builder.config.model.small, "project-small");
    }

    #[test]
    fn test_resource_flags_are_independent() {
        // Verify that flag methods don't actually load anything, just set flags
        let builder = AgentBuilder::new()
            .enterprise_resources()
            .user_resources()
            .project_resources()
            .local_resources();

        assert!(builder.load_enterprise);
        assert!(builder.load_user);
        assert!(builder.load_project);
        assert!(builder.load_local);

        // No memory content should be loaded yet (loading happens in build())
        assert!(builder.memory_provider.is_none());
    }

    #[test]
    fn test_chaining_order_does_not_affect_flags() {
        // Different chaining orders should produce same flag state
        let builder1 = AgentBuilder::new()
            .enterprise_resources()
            .user_resources()
            .project_resources();

        let builder2 = AgentBuilder::new()
            .project_resources()
            .user_resources()
            .enterprise_resources();

        assert_eq!(builder1.load_enterprise, builder2.load_enterprise);
        assert_eq!(builder1.load_user, builder2.load_user);
        assert_eq!(builder1.load_project, builder2.load_project);
    }

    #[test]
    fn test_settings_permissions_mark_policy_explicit() {
        let settings = Settings {
            authorization: crate::config::AuthorizationConfig {
                default_mode: Some("default".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };

        let builder = AgentBuilder::new().apply_settings(settings);

        assert!(builder.authorization_policy_explicit);
    }
}
