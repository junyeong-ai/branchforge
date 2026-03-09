use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{SkillIndex, SkillResult};
use crate::auth::Auth;
use crate::client::CloudProvider;
use crate::common::{Index, IndexRegistry, Named};
use crate::subagents::{SubagentIndex, builtin_subagents};
use crate::tools::ToolAccess;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillExecutionKind {
    #[default]
    Inline,
    Fork,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillSpec {
    pub index: SkillIndex,
    pub execution_kind: SkillExecutionKind,
    pub prompt: String,
    pub allowed_tools: Vec<String>,
    pub model: Option<String>,
    pub base_dir: Option<PathBuf>,
    pub agent: Option<String>,
}

pub struct SkillRuntime {
    registry: IndexRegistry<SkillIndex>,
    subagent_registry: IndexRegistry<SubagentIndex>,
}

impl SkillRuntime {
    pub fn new(registry: IndexRegistry<SkillIndex>) -> Self {
        let mut subagent_registry = IndexRegistry::new();
        subagent_registry.register_all(builtin_subagents());
        Self {
            registry,
            subagent_registry,
        }
    }

    pub fn defaults() -> Self {
        Self::new(IndexRegistry::new())
    }

    pub fn subagent_registry(mut self, subagent_registry: IndexRegistry<SubagentIndex>) -> Self {
        self.subagent_registry = subagent_registry;
        self
    }

    pub fn registry(&self) -> &IndexRegistry<SkillIndex> {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut IndexRegistry<SkillIndex> {
        &mut self.registry
    }

    pub async fn load_spec(&self, name: &str, args: Option<&str>) -> Result<SkillSpec, String> {
        let skill = self
            .registry
            .get(name)
            .cloned()
            .ok_or_else(|| format!("Skill '{}' not found", name))?;

        let content = self
            .registry
            .load_content(skill.name())
            .await
            .map_err(|e| format!("Failed to load skill '{}': {}", skill.name, e))?;
        let prompt = skill.execute(args.unwrap_or(""), &content).await;
        let execution_kind = if skill.context.as_deref() == Some("fork") {
            SkillExecutionKind::Fork
        } else {
            SkillExecutionKind::Inline
        };

        Ok(SkillSpec {
            allowed_tools: skill.allowed_tools.clone(),
            model: skill.model.clone(),
            base_dir: skill.get_base_dir(),
            agent: skill.agent.clone(),
            index: skill,
            execution_kind,
            prompt,
        })
    }

    pub async fn execute(&self, name: &str, args: Option<&str>) -> SkillResult {
        let spec = match self.load_spec(name, args).await {
            Ok(spec) => spec,
            Err(error) => return SkillResult::error(error),
        };

        match spec.execution_kind {
            SkillExecutionKind::Inline => self.execute_inline(spec),
            SkillExecutionKind::Fork => self.execute_fork(spec).await,
        }
    }

    pub async fn execute_by_trigger(&self, input: &str) -> Option<SkillResult> {
        let skill = self.find_by_trigger(input)?.clone();
        let args = self.extract_args(input, &skill);
        Some(self.execute(skill.name(), args.as_deref()).await)
    }

    pub fn list_model_invocable_skills(&self) -> Vec<&SkillIndex> {
        self.registry
            .iter()
            .filter(|skill| !skill.disable_model_invocation)
            .collect()
    }

    pub fn list_user_invocable_skills(&self) -> Vec<&SkillIndex> {
        self.registry
            .iter()
            .filter(|skill| skill.user_invocable)
            .collect()
    }

    pub fn has_skill(&self, name: &str) -> bool {
        self.registry.contains(name)
    }

    pub fn find_by_trigger(&self, input: &str) -> Option<&SkillIndex> {
        self.registry
            .iter()
            .find(|skill| !skill.disable_model_invocation && skill.matches_triggers(input))
    }

    fn extract_args(&self, input: &str, skill: &SkillIndex) -> Option<String> {
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

    pub fn build_summary(&self) -> String {
        self.list_model_invocable_skills()
            .into_iter()
            .map(|skill| skill.to_summary_line())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn execute_inline(&self, spec: SkillSpec) -> SkillResult {
        SkillResult::success(spec.prompt)
            .execution_kind(SkillExecutionKind::Inline)
            .allowed_tools(spec.allowed_tools)
            .model(spec.model)
            .base_dir(spec.base_dir)
            .agent(spec.agent)
    }

    async fn execute_fork(&self, spec: SkillSpec) -> SkillResult {
        let provider = CloudProvider::from_env();
        let model_config = provider.default_models();
        let subagent_name = spec.agent.as_deref().unwrap_or("general-purpose");
        let subagent = self.subagent_registry.get(subagent_name);
        let model = spec
            .model
            .clone()
            .or_else(|| subagent.map(|agent| agent.resolve_model(&model_config).to_string()))
            .unwrap_or_else(|| model_config.primary.clone());

        let mut builder = crate::agent::AgentBuilder::new();
        let auth_builder = match builder.auth(Auth::FromEnv).await {
            Ok(b) => b,
            Err(error) => return SkillResult::error(error.to_string()),
        };

        builder = auth_builder.model(&model).max_iterations(50);
        if !spec.allowed_tools.is_empty() {
            builder = builder.tools(ToolAccess::only(
                spec.allowed_tools.iter().map(String::as_str),
            ));
        } else if let Some(agent) = subagent
            && !agent.allowed_tools.is_empty()
        {
            builder = builder.tools(ToolAccess::only(
                agent.allowed_tools.iter().map(String::as_str),
            ));
        }
        if let Some(base_dir) = spec.base_dir.clone() {
            builder = builder.working_dir(base_dir);
        }

        let agent = match builder.build().await {
            Ok(agent) => agent,
            Err(error) => return SkillResult::error(error.to_string()),
        };
        match agent.execute(&spec.prompt).await {
            Ok(result) => SkillResult::success(result.text().to_string())
                .execution_kind(SkillExecutionKind::Fork)
                .allowed_tools(spec.allowed_tools)
                .model(Some(model))
                .base_dir(spec.base_dir)
                .agent(Some(subagent_name.to_string())),
            Err(error) => SkillResult::error(error.to_string())
                .execution_kind(SkillExecutionKind::Fork)
                .agent(Some(subagent_name.to_string())),
        }
    }
}

impl Default for SkillRuntime {
    fn default() -> Self {
        Self::defaults()
    }
}

#[cfg(test)]
mod tests {
    use crate::common::ContentSource;

    use super::*;

    fn test_skill(name: &str, content: &str) -> SkillIndex {
        SkillIndex::new(name, format!("Test skill: {name}"))
            .source(ContentSource::in_memory(content))
    }

    #[tokio::test]
    async fn loads_inline_skill_spec() {
        let mut registry = IndexRegistry::new();
        registry.register(test_skill("test-skill", "Execute: $ARGUMENTS"));
        let runtime = SkillRuntime::new(registry);

        let spec = runtime.load_spec("test-skill", Some("abc")).await.unwrap();
        assert_eq!(spec.execution_kind, SkillExecutionKind::Inline);
        assert!(spec.prompt.contains("abc"));
    }

    #[tokio::test]
    async fn marks_fork_skills_as_forked() {
        let mut registry = IndexRegistry::new();
        let mut skill = SkillIndex::new("research", "Research task")
            .source(ContentSource::in_memory("Research: $ARGUMENTS"))
            .source_type(crate::common::SourceType::Project)
            .allowed_tools(["Read"])
            .model("haiku")
            .base_dir(".")
            .triggers(["research"]);
        skill.context = Some("fork".to_string());
        skill.agent = Some("Explore".to_string());
        registry.register(skill);

        let runtime = SkillRuntime::new(registry);
        let spec = runtime.load_spec("research", Some("topic")).await.unwrap();
        assert_eq!(spec.execution_kind, SkillExecutionKind::Fork);
        assert_eq!(spec.agent.as_deref(), Some("Explore"));
    }
}
