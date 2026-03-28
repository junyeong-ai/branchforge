use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::agent::{AgentBuilder, AgentConfig, AgentResult};
use crate::auth::Auth;
use crate::authorization::{ExecutionMode, ToolPolicy, ToolRule};
use crate::client::{ModelConfig, ModelType, ProviderConfig};
use crate::common::{IndexRegistry, matches_tool_pattern};
use crate::config::SandboxConfig;
use crate::context::MemoryContent;
use crate::hooks::{CommandHook, HookEvent, HookManager, HookRule};
use crate::mcp::{is_mcp_name, parse_mcp_name};
use crate::session::SessionManager;
use crate::skills::{SkillIndex, SkillSpec};
use crate::subagents::SubagentIndex;
use crate::tools::ToolSurface;
use crate::types::Message;

#[derive(Clone)]
pub(crate) struct DelegationRuntime {
    config: AgentConfig,
    auth: Option<Auth>,
    provider_config: Option<ProviderConfig>,
    model_config: Option<ModelConfig>,
    skill_registry: IndexRegistry<SkillIndex>,
    subagent_registry: IndexRegistry<SubagentIndex>,
    hooks: HookManager,
    memory_content: MemoryContent,
    sandbox_settings: Option<SandboxConfig>,
    session_manager: Option<SessionManager>,
    mcp_manager: Option<Arc<crate::mcp::McpManager>>,
    tool_search_manager: Option<Arc<crate::tools::ToolSearchManager>>,
}

/// Configuration bundle for delegation runtime construction.
pub(crate) struct DelegationRuntimeConfig {
    pub config: AgentConfig,
    pub auth: Option<Auth>,
    pub provider_config: Option<ProviderConfig>,
    pub model_config: Option<ModelConfig>,
    pub skill_registry: IndexRegistry<SkillIndex>,
    pub subagent_registry: IndexRegistry<SubagentIndex>,
    pub hooks: HookManager,
    pub memory_content: MemoryContent,
    pub sandbox_settings: Option<SandboxConfig>,
    pub session_manager: Option<SessionManager>,
    pub mcp_manager: Option<Arc<crate::mcp::McpManager>>,
    pub tool_search_manager: Option<Arc<crate::tools::ToolSearchManager>>,
}

impl DelegationRuntime {
    pub(crate) fn new(cfg: DelegationRuntimeConfig) -> Self {
        Self {
            config: cfg.config,
            auth: cfg.auth,
            provider_config: cfg.provider_config,
            model_config: cfg.model_config,
            skill_registry: cfg.skill_registry,
            subagent_registry: cfg.subagent_registry,
            hooks: cfg.hooks,
            memory_content: cfg.memory_content,
            sandbox_settings: cfg.sandbox_settings,
            session_manager: cfg.session_manager,
            mcp_manager: cfg.mcp_manager,
            tool_search_manager: cfg.tool_search_manager,
        }
    }

    pub(crate) async fn execute_task(
        &self,
        subagent: &SubagentIndex,
        input: &crate::agent::TaskInput,
        task_session_manager: SessionManager,
        task_session_id: crate::session::SessionId,
        boot_messages: Option<Vec<Message>>,
    ) -> crate::Result<AgentResult> {
        let mut builder = self.base_builder().await?;
        let prompt = self.build_subagent_system_prompt(subagent).await?;
        let model = input
            .model
            .as_deref()
            .map(|model| self.resolve_model_alias(model))
            .or(subagent
                .model
                .as_deref()
                .map(|model| self.resolve_model_alias(model)))
            .unwrap_or_else(|| self.resolve_subagent_model(subagent))
            .to_string();

        builder = builder
            .session_manager(task_session_manager)
            .resume_session(task_session_id.to_string())
            .await?;
        builder = self.apply_subagent(builder, subagent).await;
        builder = builder.model(model).append_system_prompt(prompt);

        let agent = builder.build().await?;
        match boot_messages {
            Some(messages) if !messages.is_empty() => {
                agent.execute_with_messages(messages, &input.prompt).await
            }
            _ => agent.execute(&input.prompt).await,
        }
    }

    pub(crate) async fn execute_skill_fork(
        &self,
        spec: &SkillSpec,
    ) -> crate::Result<crate::skills::SkillResult> {
        let subagent = spec
            .agent
            .as_deref()
            .and_then(|name| self.subagent_registry.get(name));
        let model = spec
            .model
            .as_deref()
            .map(|model| self.resolve_model_alias(model))
            .or_else(|| subagent.map(|agent| self.resolve_subagent_model(agent)))
            .unwrap_or(self.config.model.primary.as_str())
            .to_string();

        let mut builder = self.base_builder().await?;
        if let Some(subagent) = subagent {
            let prompt = self.build_subagent_system_prompt(subagent).await?;
            builder = self.apply_subagent(builder, subagent).await;
            builder = builder.append_system_prompt(prompt);
        } else {
            builder = self.apply_skill_constraints(builder, spec, None).await;
        }

        if let Some(base_dir) = spec.base_dir.clone() {
            builder = builder.working_dir(base_dir);
        }

        builder = builder.model(model.clone());
        builder = self.apply_skill_constraints(builder, spec, subagent).await;

        let agent = builder.build().await?;
        match agent.execute(&spec.prompt).await {
            Ok(result) => Ok(
                crate::skills::SkillResult::success(result.text().to_string())
                    .execution_kind(crate::skills::SkillExecutionKind::Fork)
                    .allowed_tools(spec.allowed_tools.clone())
                    .model(Some(model))
                    .base_dir(spec.base_dir.clone())
                    .agent(spec.agent.clone()),
            ),
            Err(error) => Ok(crate::skills::SkillResult::error(error.to_string())
                .execution_kind(crate::skills::SkillExecutionKind::Fork)
                .agent(spec.agent.clone())),
        }
    }

    async fn base_builder(&self) -> crate::Result<AgentBuilder> {
        let mut builder = AgentBuilder::new()
            .agent_config(self.config.clone())
            .skip_resource_loading()
            .hooks_manager(self.hooks.clone())
            .subagent_registry(self.subagent_registry.clone())
            .skill_registry(self.skill_registry.clone());

        if let Some(auth) = self.auth.clone() {
            builder = builder.auth(auth).await?;
        }
        if let Some(provider_config) = self.provider_config.clone() {
            builder = builder.provider_config(provider_config);
        }
        if let Some(model_config) = self.model_config.clone() {
            builder = builder.models(model_config);
        }
        if let Some(session_manager) = self.session_manager.clone() {
            builder = builder.session_manager(session_manager);
        }
        if let Some(ref sandbox_settings) = self.sandbox_settings {
            builder = builder.sandbox_settings(sandbox_settings.clone());
        }
        if let Some(ref mcp_manager) = self.mcp_manager {
            builder = builder.shared_mcp_manager(mcp_manager.clone());
        }
        if let Some(ref tool_search_manager) = self.tool_search_manager {
            builder = builder.shared_tool_search_manager(tool_search_manager.clone());
        }

        for content in &self.memory_content.claude_md {
            builder = builder.memory_content(content.clone());
        }
        for content in &self.memory_content.local_md {
            builder = builder.local_memory_content(content.clone());
        }
        for rule in &self.memory_content.rule_indices {
            builder = builder.rule_index(rule.clone());
        }

        Ok(builder)
    }

    async fn build_subagent_system_prompt(
        &self,
        subagent: &SubagentIndex,
    ) -> crate::Result<String> {
        let prompt = subagent.load_prompt().await?;
        let preloaded_skills = self.load_preloaded_skill_block(subagent).await?;
        if preloaded_skills.is_empty() {
            return Ok(prompt);
        }

        Ok(format!(
            "{prompt}\n\n<preloaded_skills>\nThese skills are preloaded for this subagent at startup. Treat them as active reference material.\n\n{preloaded_skills}\n</preloaded_skills>"
        ))
    }

    async fn load_preloaded_skill_block(&self, subagent: &SubagentIndex) -> crate::Result<String> {
        let mut sections = Vec::new();
        for skill_name in &subagent.skills {
            let skill = self.skill_registry.get(skill_name).ok_or_else(|| {
                crate::Error::Config(format!(
                    "Subagent '{}' references unknown skill '{}'",
                    subagent.name, skill_name
                ))
            })?;
            let content = skill.load_preloaded_content().await.map_err(|error| {
                crate::Error::Config(format!(
                    "Failed to preload skill '{}' for subagent '{}': {}",
                    skill_name, subagent.name, error
                ))
            })?;
            let content = content.trim();
            if content.is_empty() {
                continue;
            }
            sections.push(format!(
                "<skill>\n<name>{}</name>\n<description>{}</description>\n{}\n</skill>",
                skill.name, skill.description, content
            ));
        }
        Ok(sections.join("\n\n"))
    }

    async fn apply_subagent(
        &self,
        builder: AgentBuilder,
        subagent: &SubagentIndex,
    ) -> AgentBuilder {
        let skills_enabled = !subagent.skills.is_empty();
        let skill_registry = self.filtered_skills(&subagent.skills);
        let hooks = merged_hooks(
            &self.hooks,
            subagent.hooks.as_ref(),
            &format!("subagent:{}", subagent.name),
        );
        let access = self
            .restricted_tool_surface(
                &self.config.security.tool_surface,
                Some(&subagent.allowed_tools),
                &subagent.disallowed_tools,
                skills_enabled,
                &subagent.mcp_servers,
            )
            .await;
        let policy = restricted_tool_policy(
            &self.config.security.authorization_policy,
            &subagent.disallowed_tools,
            skills_enabled,
        );
        let mut builder = builder
            .skill_registry(skill_registry)
            .subagent_registry(self.subagent_registry.clone())
            .tools(access)
            .authorization_policy(policy)
            .hooks_manager(hooks);

        if let Some(mode) = subagent
            .authorization_mode
            .as_deref()
            .and_then(parse_execution_mode)
        {
            builder = builder.execution_mode(mode);
        }

        if let Some(max_turns) = subagent.max_turns {
            builder = builder.max_iterations(max_turns);
        }

        builder
    }

    async fn apply_skill_constraints(
        &self,
        builder: AgentBuilder,
        spec: &SkillSpec,
        subagent: Option<&SubagentIndex>,
    ) -> AgentBuilder {
        let inherited_disallowed = subagent
            .map(|agent| agent.disallowed_tools.as_slice())
            .unwrap_or_default();
        let skills_enabled = subagent.is_some_and(|agent| !agent.skills.is_empty());
        let allowed = if !spec.allowed_tools.is_empty() {
            Some(spec.allowed_tools.as_slice())
        } else {
            subagent
                .filter(|agent| !agent.allowed_tools.is_empty())
                .map(|agent| agent.allowed_tools.as_slice())
        };
        let hooks = match subagent {
            Some(agent) => {
                let inherited = merged_hooks(
                    &self.hooks,
                    agent.hooks.as_ref(),
                    &format!("subagent:{}", agent.name),
                );
                merged_hooks(
                    &inherited,
                    spec.index.hooks.as_ref(),
                    &format!("skill:{}", spec.index.name),
                )
            }
            None => merged_hooks(
                &self.hooks,
                spec.index.hooks.as_ref(),
                &format!("skill:{}", spec.index.name),
            ),
        };
        let mcp_servers = subagent
            .map(|agent| agent.mcp_servers.as_slice())
            .unwrap_or_default();
        let policy = restricted_tool_policy(
            &self.config.security.authorization_policy,
            inherited_disallowed,
            skills_enabled,
        );
        let mut builder = builder;
        if let Some(mode) = subagent
            .and_then(|agent| agent.authorization_mode.as_deref())
            .and_then(parse_execution_mode)
        {
            builder = builder.execution_mode(mode);
        }

        builder
            .tools(
                self.restricted_tool_surface(
                    &self.config.security.tool_surface,
                    allowed,
                    inherited_disallowed,
                    skills_enabled,
                    mcp_servers,
                )
                .await,
            )
            .authorization_policy(policy)
            .hooks_manager(hooks)
    }

    async fn restricted_tool_surface(
        &self,
        base: &ToolSurface,
        allowed: Option<&[String]>,
        disallowed: &[String],
        skills_enabled: bool,
        mcp_servers: &[String],
    ) -> ToolSurface {
        let access = restricted_tool_surface(base, allowed, disallowed, skills_enabled);
        self.restrict_mcp_servers(access, base, mcp_servers).await
    }

    async fn restrict_mcp_servers(
        &self,
        access: ToolSurface,
        base: &ToolSurface,
        mcp_servers: &[String],
    ) -> ToolSurface {
        if mcp_servers.is_empty() {
            return access;
        }

        let Some(manager) = self.mcp_manager.as_ref() else {
            return access;
        };

        let allowed_servers: HashSet<&str> = mcp_servers.iter().map(String::as_str).collect();
        let all_mcp_tools: HashSet<String> = manager
            .list_tools()
            .await
            .into_iter()
            .map(|(qualified_name, _)| qualified_name)
            .collect();
        apply_mcp_server_tool_filter(access, base, &allowed_servers, &all_mcp_tools)
    }

    fn filtered_skills(&self, allowed: &[String]) -> IndexRegistry<SkillIndex> {
        if allowed.is_empty() {
            return IndexRegistry::new();
        }

        let mut registry = IndexRegistry::new();
        for skill_name in allowed {
            if let Some(skill) = self.skill_registry.get(skill_name) {
                registry.register(skill.clone());
            }
        }
        registry
    }

    fn resolve_subagent_model<'a>(&'a self, subagent: &'a SubagentIndex) -> &'a str {
        self.model_config
            .as_ref()
            .map(|models| subagent.resolve_model(models))
            .unwrap_or_else(|| match subagent.model.as_deref() {
                Some(model) => model,
                None => match subagent.model_type.unwrap_or_default() {
                    ModelType::Primary => self.config.model.primary.as_str(),
                    ModelType::Small => self.config.model.small.as_str(),
                    ModelType::Reasoning => self.config.model.primary.as_str(),
                },
            })
    }

    fn resolve_model_alias<'a>(&'a self, model: &'a str) -> &'a str {
        self.model_config
            .as_ref()
            .map(|models| models.resolve_alias(model))
            .unwrap_or(model)
    }
}

fn merged_hooks(
    base: &HookManager,
    extra: Option<&HashMap<String, Vec<HookRule>>>,
    prefix: &str,
) -> HookManager {
    let mut hooks = base.clone();
    let Some(extra) = extra else {
        return hooks;
    };

    let mut counter = 0usize;
    for (event_name, rules) in extra {
        let Some(event) = HookEvent::from_pascal_case(event_name) else {
            continue;
        };

        for rule in rules {
            for action in &rule.hooks {
                let Some(config) = action.to_hook_config(rule.matcher.as_deref()) else {
                    continue;
                };
                let hook = CommandHook::from_event_config(
                    format!("{prefix}:{event_name}:{counter}"),
                    event,
                    &config,
                );
                hooks.register(hook);
                counter += 1;
            }
        }
    }

    hooks
}

fn restricted_tool_surface(
    base: &ToolSurface,
    allowed: Option<&[String]>,
    disallowed: &[String],
    skills_enabled: bool,
) -> ToolSurface {
    let mut denied: HashSet<String> = disallowed.iter().cloned().collect();
    denied.insert("Task".to_string());
    denied.insert("TaskOutput".to_string());
    if !skills_enabled {
        denied.insert("Skill".to_string());
    }

    match allowed {
        Some(allowed) if !allowed.is_empty() => {
            let filtered: HashSet<String> = allowed
                .iter()
                .map(|tool| tool_registration_name(tool))
                .filter(|tool| base.is_allowed(tool))
                .filter(|tool| !matches_denied_pattern(&denied, tool))
                .map(str::to_string)
                .collect();
            if filtered.is_empty() {
                ToolSurface::None
            } else {
                ToolSurface::Only(filtered)
            }
        }
        _ => match base {
            ToolSurface::None => ToolSurface::None,
            ToolSurface::Core => {
                let filtered: HashSet<String> = ToolSurface::CORE_TOOLS
                    .iter()
                    .copied()
                    .filter(|tool| !matches_denied_pattern(&denied, tool))
                    .map(str::to_string)
                    .collect();
                if filtered.is_empty() {
                    ToolSurface::None
                } else {
                    ToolSurface::Only(filtered)
                }
            }
            ToolSurface::All => {
                if denied.is_empty() {
                    ToolSurface::All
                } else {
                    ToolSurface::Except(denied)
                }
            }
            ToolSurface::Only(allowed) => {
                let filtered: HashSet<String> = allowed
                    .iter()
                    .filter(|tool| !matches_denied_pattern(&denied, tool))
                    .cloned()
                    .collect();
                if filtered.is_empty() {
                    ToolSurface::None
                } else {
                    ToolSurface::Only(filtered)
                }
            }
            ToolSurface::Except(existing) => {
                let mut merged = existing.clone();
                merged.extend(denied);
                ToolSurface::Except(merged)
            }
        },
    }
}

fn tool_registration_name(pattern: &str) -> &str {
    pattern.split('(').next().unwrap_or(pattern)
}

fn matches_denied_pattern(denied: &HashSet<String>, tool_name: &str) -> bool {
    denied
        .iter()
        .any(|pattern| matches_tool_pattern(pattern, tool_name))
}

fn apply_mcp_server_tool_filter(
    access: ToolSurface,
    base: &ToolSurface,
    allowed_servers: &HashSet<&str>,
    all_mcp_tools: &HashSet<String>,
) -> ToolSurface {
    if allowed_servers.is_empty() || all_mcp_tools.is_empty() {
        return access;
    }

    let allowed_mcp_tools: HashSet<String> = all_mcp_tools
        .iter()
        .filter(|name| {
            parse_mcp_name(name).is_some_and(|(server, _)| allowed_servers.contains(server))
        })
        .filter(|name| base.is_allowed(name))
        .cloned()
        .collect();
    let denied_mcp_tools: HashSet<String> = all_mcp_tools
        .difference(&allowed_mcp_tools)
        .cloned()
        .collect();

    match access {
        ToolSurface::None => ToolSurface::None,
        ToolSurface::Core => ToolSurface::Core,
        ToolSurface::All => {
            if denied_mcp_tools.is_empty() {
                ToolSurface::All
            } else {
                ToolSurface::Except(denied_mcp_tools)
            }
        }
        ToolSurface::Except(mut denied) => {
            denied.extend(denied_mcp_tools);
            ToolSurface::Except(denied)
        }
        ToolSurface::Only(mut allowed) => {
            let has_explicit_mcp_allowlist = allowed.iter().any(|tool| is_mcp_name(tool));
            allowed.retain(|tool| !is_mcp_name(tool) || allowed_mcp_tools.contains(tool));
            if !has_explicit_mcp_allowlist {
                allowed.extend(allowed_mcp_tools);
            }
            if allowed.is_empty() {
                ToolSurface::None
            } else {
                ToolSurface::Only(allowed)
            }
        }
    }
}

fn restricted_tool_policy(
    base: &ToolPolicy,
    disallowed: &[String],
    skills_enabled: bool,
) -> ToolPolicy {
    let mut policy = base.clone();
    for tool in disallowed {
        policy.rules.push(ToolRule::deny_pattern(tool));
    }
    policy.rules.push(ToolRule::deny_pattern("Task"));
    policy.rules.push(ToolRule::deny_pattern("TaskOutput"));
    if !skills_enabled {
        policy.rules.push(ToolRule::deny_pattern("Skill"));
    }
    policy
}

fn parse_execution_mode(value: &str) -> Option<ExecutionMode> {
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::common::ContentSource;

    fn runtime_with_skills(skill_registry: IndexRegistry<SkillIndex>) -> DelegationRuntime {
        DelegationRuntime::new(DelegationRuntimeConfig {
            config: AgentConfig::default(),
            auth: None,
            provider_config: None,
            model_config: None,
            skill_registry,
            subagent_registry: IndexRegistry::new(),
            hooks: HookManager::default(),
            memory_content: MemoryContent::default(),
            sandbox_settings: None,
            session_manager: None,
            mcp_manager: None,
            tool_search_manager: None,
        })
    }

    #[tokio::test]
    async fn build_subagent_system_prompt_preloads_declared_skills() {
        let mut skills = IndexRegistry::new();
        skills.register(
            SkillIndex::new("review", "Review guidance")
                .source(ContentSource::in_memory(
                    r#"---
name: review
description: Review guidance
---
Check [guide](guide.md)
Run !`echo should-not-run`"#,
                ))
                .with_base_dir("/tmp/skills"),
        );

        let runtime = runtime_with_skills(skills);
        let subagent = SubagentIndex::new("reviewer", "Code reviewer")
            .source(ContentSource::in_memory("You are a reviewer."))
            .skills(["review"]);

        let prompt = runtime
            .build_subagent_system_prompt(&subagent)
            .await
            .unwrap();

        assert!(prompt.contains("You are a reviewer."));
        assert!(prompt.contains("<preloaded_skills>"));
        assert!(prompt.contains("<name>review</name>"));
        assert!(prompt.contains("[guide](/tmp/skills/guide.md)"));
        assert!(prompt.contains("!`echo should-not-run`"));
        assert!(!prompt.contains("name: review"));
    }

    #[test]
    fn resolve_subagent_model_honors_model_type_without_model_registry() {
        let runtime = runtime_with_skills(IndexRegistry::new());

        let primary = SubagentIndex::new("planner", "Planner")
            .source(ContentSource::in_memory("Plan"))
            .model_type(ModelType::Primary);
        let small = SubagentIndex::new("explore", "Explore")
            .source(ContentSource::in_memory("Explore"))
            .model_type(ModelType::Small);

        assert_eq!(
            runtime.resolve_subagent_model(&primary),
            runtime.config.model.primary.as_str()
        );
        assert_eq!(
            runtime.resolve_subagent_model(&small),
            runtime.config.model.small.as_str()
        );
    }

    #[test]
    fn restricted_tool_surface_normalizes_scoped_allowed_tool_names() {
        let access = restricted_tool_surface(
            &ToolSurface::all(),
            Some(&["Bash(git:*)".to_string(), "Read".to_string()]),
            &[],
            false,
        );

        assert!(access.is_allowed("Bash"));
        assert!(access.is_allowed("Read"));
        assert!(!access.is_allowed("Write"));
    }

    #[test]
    fn apply_mcp_server_tool_filter_limits_visible_servers() {
        let allowed_servers = HashSet::from(["context7"]);
        let all_mcp_tools = HashSet::from([
            "mcp__context7__search".to_string(),
            "mcp__filesystem__read_file".to_string(),
        ]);

        let filtered = apply_mcp_server_tool_filter(
            ToolSurface::All,
            &ToolSurface::All,
            &allowed_servers,
            &all_mcp_tools,
        );

        assert!(filtered.is_allowed("mcp__context7__search"));
        assert!(!filtered.is_allowed("mcp__filesystem__read_file"));
    }

    #[test]
    fn apply_mcp_server_tool_filter_respects_parent_only_access() {
        let allowed_servers = HashSet::from(["context7"]);
        let all_mcp_tools = HashSet::from(["mcp__context7__search".to_string()]);

        let filtered = apply_mcp_server_tool_filter(
            ToolSurface::Only(HashSet::from(["Read".to_string()])),
            &ToolSurface::Only(HashSet::from(["Read".to_string()])),
            &allowed_servers,
            &all_mcp_tools,
        );

        assert!(filtered.is_allowed("Read"));
        assert!(!filtered.is_allowed("mcp__context7__search"));
    }

    #[test]
    fn apply_mcp_server_tool_filter_does_not_widen_explicit_mcp_allowlist() {
        let allowed_servers = HashSet::from(["context7"]);
        let all_mcp_tools = HashSet::from([
            "mcp__context7__search".to_string(),
            "mcp__context7__fetch".to_string(),
        ]);

        let filtered = apply_mcp_server_tool_filter(
            ToolSurface::Only(HashSet::from([
                "Read".to_string(),
                "mcp__context7__search".to_string(),
            ])),
            &ToolSurface::Only(HashSet::from([
                "Read".to_string(),
                "mcp__context7__search".to_string(),
            ])),
            &allowed_servers,
            &all_mcp_tools,
        );

        assert!(filtered.is_allowed("Read"));
        assert!(filtered.is_allowed("mcp__context7__search"));
        assert!(!filtered.is_allowed("mcp__context7__fetch"));
    }

    #[test]
    fn restricted_tool_surface_respects_denied_patterns() {
        let access = restricted_tool_surface(
            &ToolSurface::all(),
            Some(&["Bash(git:*)".to_string(), "Read".to_string()]),
            &["Bash(git:*)".to_string()],
            false,
        );

        assert!(access.is_allowed("Read"));
        assert!(!access.is_allowed("Bash"));
    }
}
