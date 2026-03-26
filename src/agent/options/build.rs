//! Agent build methods.

use std::sync::Arc;

use crate::client::{CloudProvider, ProviderConfig};
use crate::common::Index;
use crate::common::IndexRegistry;
use crate::context::{MemoryProvider, PromptOrchestrator, RuleIndex, StaticContext};
use crate::skills::{SkillRuntime, build_model_invocable_summary};
use crate::tools::{ToolRegistry, ToolSearchConfig, ToolSearchManager};

use super::builder::AgentBuilder;

impl AgentBuilder {
    pub async fn build(mut self) -> crate::Result<crate::agent::Agent> {
        // Load resources in fixed order (regardless of chaining order)
        // Order: Enterprise → User → Project → Local (later overrides earlier)
        self.load_resources_by_level().await;

        #[cfg(feature = "plugins")]
        self.load_plugins().await;

        self.resolve_output_style().await?;
        self.resolve_model_aliases();
        self.connect_mcp_servers().await?;
        self.initialize_tool_search().await;

        let delegation_runtime = self.build_delegation_runtime().await;
        let client = self.build_client().await?;
        let tools = self.build_tools(delegation_runtime).await;
        let orchestrator = self.build_orchestrator().await;
        let identity = self.config.identity.clone();
        let session_scope = crate::session::SessionAccessScope {
            tenant_id: identity.tenant_id.clone(),
            principal_id: identity.principal_id.clone(),
        };

        let tenant_budget = self.tenant_budget_manager.as_ref().and_then(|m| {
            self.config
                .identity
                .tenant_id
                .as_ref()
                .and_then(|id| m.get(id))
        });

        let mut agent = crate::agent::Agent::from_orchestrator(
            client,
            self.config,
            tools,
            self.hooks,
            orchestrator,
        );

        agent.execution_mode = self.execution_mode;

        if let Some(messages) = self.initial_messages {
            agent = agent.initial_messages(messages);
        }
        if let Some(id) = self.resume_session_id {
            agent = agent.session_id(id);
        }
        if let Some(mcp) = self.mcp_manager {
            agent = agent.mcp_manager(mcp);
        }
        if let Some(budget) = tenant_budget {
            agent = agent.tenant_budget(budget);
        }
        if let Some(manager) = self.session_manager.clone() {
            let scope = if session_scope.tenant_id.is_some() || session_scope.principal_id.is_some()
            {
                Some(session_scope.clone())
            } else {
                None
            };
            agent = agent.session_persistence(manager, scope);
        }

        agent
            .state()
            .with_session_mut(|session| {
                session.tenant_id = identity.tenant_id.clone();
                session.principal_id = identity.principal_id.clone();
            })
            .await;

        if let Some(orchestrator) = agent.orchestrator() {
            let static_hash = {
                let context = orchestrator.read().await.static_context().clone();
                context.tools(agent.tools().definitions()).content_hash()
            };
            agent
                .state()
                .with_session_mut(|session| {
                    session.static_context_hash = Some(static_hash);
                })
                .await;
        }

        if let Some(scope) = self.context_scope {
            agent.context_scope = Some(scope);
        }

        agent.persist_session_state().await?;
        if let Some(tsm) = self.tool_search_manager {
            agent = agent.tool_search_manager(tsm);
        }

        Ok(agent)
    }

    #[cfg(feature = "file-resources")]
    async fn resolve_output_style(&mut self) -> crate::Result<()> {
        use crate::common::{Provider, SourceType};
        use crate::output_style::{
            ChainOutputStyleProvider, InMemoryOutputStyleProvider, builtin_styles,
            file_output_style_provider,
        };

        if self.config.prompt.output_style.is_some() {
            return Ok(());
        }

        let Some(ref name) = self.output_style_name else {
            return Ok(());
        };

        let builtins = InMemoryOutputStyleProvider::new()
            .items(builtin_styles())
            .priority(0)
            .source_type(SourceType::Builtin);

        let mut chain = ChainOutputStyleProvider::new().provider(builtins);

        if let Some(ref working_dir) = self.config.working_dir {
            let project = file_output_style_provider()
                .project_path(working_dir)
                .priority(20)
                .source_type(SourceType::Project);
            chain = chain.provider(project);
        }

        let user = file_output_style_provider()
            .user_path()
            .priority(10)
            .source_type(SourceType::User);
        chain = chain.provider(user);

        if let Ok(Some(style)) = chain.get(name).await {
            self.config.prompt.output_style = Some(style);
        }

        Ok(())
    }

    #[cfg(not(feature = "file-resources"))]
    async fn resolve_output_style(&mut self) -> crate::Result<()> {
        Ok(())
    }

    fn resolve_model_aliases(&mut self) {
        let provider = self.cloud_provider.unwrap_or_else(CloudProvider::from_env);
        let model_config = self
            .model_config
            .clone()
            .unwrap_or_else(|| provider.default_models());

        // Resolve primary model alias
        let primary = &self.config.model.primary;
        let resolved_primary = model_config.resolve_alias(primary);
        if resolved_primary != primary {
            tracing::debug!(
                alias = %primary,
                resolved = %resolved_primary,
                "Resolved primary model alias"
            );
            self.config.model.primary = resolved_primary.to_string();
        }

        // Resolve small model alias
        let small = &self.config.model.small;
        let resolved_small = model_config.resolve_alias(small);
        if resolved_small != small {
            tracing::debug!(
                alias = %small,
                resolved = %resolved_small,
                "Resolved small model alias"
            );
            self.config.model.small = resolved_small.to_string();
        }
    }

    async fn connect_mcp_servers(&mut self) -> crate::Result<()> {
        if self.mcp_configs.is_empty() && self.mcp_manager.is_none() {
            return Ok(());
        }

        let manager = self
            .mcp_manager
            .take()
            .unwrap_or_else(|| std::sync::Arc::new(crate::mcp::McpManager::new()));

        for (name, config) in std::mem::take(&mut self.mcp_configs) {
            manager.add_server(&name, config).await.map_err(|e| {
                crate::Error::Mcp(crate::mcp::McpError::ConnectionFailed {
                    message: format!("{}: {}", name, e),
                })
            })?;
        }

        self.mcp_manager = Some(manager);
        Ok(())
    }

    async fn initialize_tool_search(&mut self) {
        let Some(ref mcp_manager) = self.mcp_manager else {
            return;
        };

        // Use shared manager if provided, otherwise create new one
        let manager = if let Some(shared) = self.tool_search_manager.take() {
            shared
        } else {
            let config = self.tool_search_config.take().unwrap_or_else(|| {
                let context_window =
                    crate::types::context_window::for_model(&self.config.model.primary) as usize;
                ToolSearchConfig::default().context_window(context_window)
            });
            Arc::new(ToolSearchManager::new(config))
        };

        // Set toolset registry if available
        if let Some(registry) = self.mcp_toolset_registry.take() {
            manager.set_toolset_registry(registry);
        }

        manager.build_index(mcp_manager).await;

        let prepared = manager
            .prepare_tools_for_access(&self.config.security.tool_surface)
            .await;
        tracing::debug!(
            use_search = prepared.use_search,
            immediate_count = prepared.immediate.len(),
            deferred_count = prepared.deferred.len(),
            total_tokens = prepared.total_tokens,
            threshold_tokens = prepared.threshold_tokens,
            "Tool search initialized"
        );

        self.tool_search_manager = Some(manager);
    }

    /// Loads resources from enabled levels in fixed order.
    ///
    /// The order is always: Enterprise → User → Project → Local
    /// regardless of the order `with_*_resources()` methods were called.
    /// This ensures consistent override behavior where later levels
    /// override settings from earlier levels.
    #[cfg(feature = "file-resources")]
    async fn load_resources_by_level(&mut self) {
        // 1. Enterprise (lowest priority)
        if self.load_enterprise {
            self.load_enterprise_resources().await;
        }

        // 2. User
        if self.load_user {
            self.load_user_resources().await;
        }

        // 3. Project
        if self.load_project {
            self.load_project_resources().await;
        }

        // 4. Local (highest priority, overrides all)
        if self.load_local {
            self.load_local_resources().await;
        }
    }

    #[cfg(not(feature = "file-resources"))]
    async fn load_resources_by_level(&mut self) {
        // No-op when file-resources is disabled
    }

    #[cfg(feature = "plugins")]
    async fn load_plugins(&mut self) {
        use crate::plugins::{PluginDiscovery, PluginManager};
        use crate::subagents::builtin_subagents;

        let mut dirs = std::mem::take(&mut self.plugin_dirs);
        if let Some(default_dir) = PluginDiscovery::default_plugins_dir()
            && default_dir.exists()
            && !dirs.contains(&default_dir)
        {
            dirs.push(default_dir);
        }
        if dirs.is_empty() {
            return;
        }

        let manager = match PluginManager::load_from_dirs(&dirs).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load plugins");
                return;
            }
        };

        if manager.plugin_count() == 0 {
            return;
        }

        let skill_registry = self.skill_registry.get_or_insert_with(IndexRegistry::new);
        manager.register_skills(skill_registry);

        let subagent_registry = self.subagent_registry.get_or_insert_with(|| {
            let mut registry = IndexRegistry::new();
            registry.register_all(builtin_subagents());
            registry
        });
        manager.register_subagents(subagent_registry);

        for (name, config) in manager.mcp_servers() {
            self.mcp_configs.insert(name.clone(), config.clone());
        }

        let mut hook_counters = std::collections::HashMap::<String, usize>::new();
        for entry in manager.hooks() {
            let key = format!("{}:{}", entry.plugin, entry.event);
            let count = hook_counters.entry(key).or_insert(0);
            let hook_name = crate::plugins::namespace::namespaced(
                &entry.plugin,
                &format!("{}-{}", entry.event, count),
            );
            *count += 1;
            let mut hook =
                crate::hooks::CommandHook::from_event_config(hook_name, entry.event, &entry.config);
            hook = hook.env(
                "CLAUDE_PLUGIN_ROOT",
                entry.plugin_root.display().to_string(),
            );
            self.hooks.register(hook);
        }
    }

    async fn build_orchestrator(&mut self) -> PromptOrchestrator {
        let mut static_context = StaticContext::new();

        let mut claude_md = String::new();
        let mut rule_indices = std::mem::take(&mut self.rule_indices);

        if let Some(ref provider) = self.memory_provider
            && let Ok(content) = provider.load().await
        {
            claude_md = content.combined_claude_md();
            rule_indices.extend(content.rule_indices);
        }

        if !claude_md.is_empty() {
            static_context = static_context.claude_md(claude_md);
        }

        // Get skill registry for building summary
        let skill_registry = self.skill_registry.clone().unwrap_or_default();

        if self.config.security.tool_surface.is_allowed("Skill") {
            let summary = build_model_invocable_summary(&skill_registry);
            if !summary.is_empty() {
                static_context =
                    static_context.skill_summary(format!("# Available Skills\n{summary}"));
            }
        }

        let mut rule_registry: IndexRegistry<RuleIndex> = IndexRegistry::new();
        if !rule_indices.is_empty() {
            let summary = {
                let mut lines = vec!["# Available Rules".to_string()];
                for rule in &rule_indices {
                    lines.push(rule.to_summary_line());
                }
                lines.join("\n")
            };
            static_context = static_context.rules_summary(summary);
            rule_registry.register_all(rule_indices);
        }

        PromptOrchestrator::new(static_context, &self.config.model.primary)
            .rule_registry(rule_registry)
            .skill_registry(skill_registry)
    }

    async fn build_tools(
        &mut self,
        delegation_runtime: crate::agent::DelegationRuntime,
    ) -> Arc<ToolRegistry> {
        let skill_registry = self.skill_registry.take().unwrap_or_default();
        let skill_count = skill_registry.iter().count();
        tracing::debug!(skill_count, "build_tools: skill_registry taken");
        let subagent_registry = self.subagent_registry.take();
        let skill_executor =
            SkillRuntime::new(skill_registry).delegation_runtime(delegation_runtime.clone());

        let working_dir = self
            .config
            .working_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        let sandbox_config = self
            .sandbox_settings
            .take()
            .map(|s| s.to_sandbox_config(working_dir.clone()));

        let (tool_state, session_id) = match self.resumed_session.take() {
            Some(session) => {
                let id = session.id;
                (crate::session::ToolState::from_session(session), id)
            }
            None => {
                let id = self
                    .resume_session_id
                    .as_deref()
                    .and_then(crate::session::SessionId::parse)
                    .unwrap_or_default();
                (crate::session::ToolState::new(id), id)
            }
        };

        let mut builder = crate::tools::ToolRegistryBuilder::new()
            .access(self.config.security.tool_surface.clone())
            .working_dir(working_dir)
            .skill_executor(skill_executor)
            .tool_state(tool_state)
            .session_id(session_id)
            .hooks(self.hooks.clone())
            .scope(crate::session::SessionAccessScope {
                tenant_id: self.config.identity.tenant_id.clone(),
                principal_id: self.config.identity.principal_id.clone(),
            });

        if self.authorization_policy_explicit
            || AgentBuilder::tool_policy_is_custom(&self.config.security.authorization_policy)
        {
            builder = builder.policy(self.config.security.authorization_policy.clone());
        }

        if let Some(ref manager) = self.session_manager {
            builder = builder.session_manager(manager.clone());
        }

        if let Some(sr) = subagent_registry {
            builder = builder
                .subagent_registry(sr)
                .delegation_runtime(delegation_runtime.clone());
        } else {
            builder = builder.delegation_runtime(delegation_runtime);
        }
        if let Some(sc) = sandbox_config {
            builder = builder.sandbox_config(sc);
        }

        let tools = builder.build();

        for tool in std::mem::take(&mut self.custom_tools) {
            tools.register(tool);
        }

        if let Some(ref mcp_manager) = self.mcp_manager {
            let mcp_tools = crate::tools::create_mcp_tools_with_access(
                Arc::clone(mcp_manager),
                &self.config.security.tool_surface,
            )
            .await;
            for tool in mcp_tools {
                tools.register(tool);
            }
        }

        Arc::new(tools)
    }

    async fn build_delegation_runtime(&self) -> crate::agent::DelegationRuntime {
        let memory_content = match self.memory_provider.as_ref() {
            Some(provider) => provider.load().await.unwrap_or_default(),
            None => crate::context::MemoryContent::default(),
        };

        crate::agent::DelegationRuntime::new(crate::agent::DelegationRuntimeConfig {
            config: self.config.clone(),
            auth: self.auth_type.clone(),
            provider_config: self.provider_config.clone(),
            model_config: self.model_config.clone(),
            skill_registry: self.skill_registry.clone().unwrap_or_default(),
            subagent_registry: self.subagent_registry.clone().unwrap_or_default(),
            hooks: self.hooks.clone(),
            memory_content,
            sandbox_settings: self.sandbox_settings.clone(),
            session_manager: self.session_manager.clone(),
            mcp_manager: self.mcp_manager.clone(),
            tool_search_manager: self.tool_search_manager.clone(),
        })
    }

    async fn build_client(&mut self) -> crate::Result<crate::Client> {
        let provider = self.cloud_provider.unwrap_or_else(CloudProvider::from_env);
        let models = self
            .model_config
            .take()
            .unwrap_or_else(|| provider.default_models());
        let mut config = self
            .provider_config
            .take()
            .unwrap_or_else(|| ProviderConfig::new(models));

        config = config.max_tokens(self.config.model.max_tokens);

        if self.supports_server_tools() {
            config.beta.add(crate::client::BetaFeature::WebSearch);
            config.beta.add(crate::client::BetaFeature::WebFetch);
            tracing::debug!("Enabled server-side web tools");
        }

        // Enable tool search beta if manager is configured
        if self.tool_search_manager.is_some() {
            config.beta.add(crate::client::BetaFeature::AdvancedToolUse);
            tracing::debug!("Enabled advanced tool use for tool search");
        }

        // Enable 1M context window beta if extended context is enabled
        if self.config.model.extended_context {
            config.beta.add(crate::client::BetaFeature::Context1M);
            tracing::debug!("Enabled extended context window (1M tokens)");
        }

        // Enable structured outputs beta if output_schema is configured
        if self.config.prompt.output_schema.is_some() {
            config
                .beta
                .add(crate::client::BetaFeature::StructuredOutputs);
            tracing::debug!("Enabled structured outputs beta for JSON schema");
        }

        let mut builder = crate::Client::builder().config(config);

        match provider {
            CloudProvider::Anthropic => {
                builder = builder.anthropic();
                if let Some(cred) = self.credential.take() {
                    builder = builder.auth(cred).await?;
                }
                if let Some(oauth_config) = self.oauth_config.take() {
                    builder = builder.oauth_config(oauth_config);
                }
            }
            #[cfg(feature = "aws")]
            CloudProvider::Bedrock => {
                let region = self.aws_region.take().unwrap_or_else(|| "us-east-1".into());
                builder = builder.aws_region(region);
            }
            #[cfg(feature = "gcp")]
            CloudProvider::Vertex => {
                let project = self
                    .gcp_project
                    .take()
                    .ok_or_else(|| crate::Error::Config("Vertex requires gcp_project".into()))?;
                let region = self
                    .gcp_region
                    .take()
                    .unwrap_or_else(|| "us-central1".into());
                builder = builder.gcp(project, region);
            }
            #[cfg(feature = "azure")]
            CloudProvider::Foundry => {
                let resource = self.azure_resource.take().ok_or_else(|| {
                    crate::Error::Config("Foundry requires azure_resource".into())
                })?;
                builder = builder.azure_resource(resource);
            }
            #[cfg(feature = "openai")]
            CloudProvider::OpenAi => {
                builder = builder.openai();
                if let Some(cred) = self.credential.take() {
                    builder = builder.auth(cred).await?;
                }
            }
            #[cfg(feature = "gemini")]
            CloudProvider::Gemini => {
                builder = builder.gemini();
                if let Some(cred) = self.credential.take() {
                    builder = builder.auth(cred).await?;
                }
            }
        }

        if let Some(fallback) = self.fallback_config.take() {
            builder = builder.fallback(fallback);
        } else if let Some(ref model) = self.config.budget.fallback_model {
            builder = builder.fallback_model(model);
        }

        builder.build().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_model_aliases() {
        let mut builder = AgentBuilder::new();

        // Set alias as model
        builder.config.model.primary = "sonnet".to_string();
        builder.config.model.small = "haiku".to_string();

        // Resolve aliases
        builder.resolve_model_aliases();

        // Should be resolved to full model IDs
        assert!(
            builder.config.model.primary.contains("sonnet"),
            "Primary model should contain 'sonnet': {}",
            builder.config.model.primary
        );
        assert!(
            builder.config.model.primary.starts_with("claude-"),
            "Primary model should start with 'claude-': {}",
            builder.config.model.primary
        );
        assert!(
            builder.config.model.small.contains("haiku"),
            "Small model should contain 'haiku': {}",
            builder.config.model.small
        );
        assert!(
            builder.config.model.small.starts_with("claude-"),
            "Small model should start with 'claude-': {}",
            builder.config.model.small
        );
    }

    #[test]
    fn test_resolve_model_aliases_full_id_unchanged() {
        let mut builder = AgentBuilder::new();

        // Set full model ID (not an alias)
        let full_id = "claude-sonnet-4-5-20250929";
        builder.config.model.primary = full_id.to_string();

        // Resolve aliases
        builder.resolve_model_aliases();

        // Should remain unchanged
        assert_eq!(builder.config.model.primary, full_id);
    }

    #[test]
    fn test_resolve_model_aliases_opus() {
        let mut builder = AgentBuilder::new();

        builder.config.model.primary = "opus".to_string();
        builder.resolve_model_aliases();

        assert!(
            builder.config.model.primary.contains("opus"),
            "Primary model should contain 'opus': {}",
            builder.config.model.primary
        );
    }
}
