use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::instrument;

use super::{
    SkillIndex, SkillResult, build_model_invocable_summary, extract_trigger_args,
    find_explicit_command, find_first_trigger_match, list_model_invocable_skills,
    parse_explicit_command,
};
use crate::common::{IndexRegistry, Named};

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
    delegation_runtime: Option<crate::agent::DelegationRuntime>,
}

impl SkillRuntime {
    pub fn new(registry: IndexRegistry<SkillIndex>) -> Self {
        Self {
            registry,
            delegation_runtime: None,
        }
    }

    pub fn defaults() -> Self {
        Self::new(IndexRegistry::new())
    }

    pub(crate) fn delegation_runtime(mut self, runtime: crate::agent::DelegationRuntime) -> Self {
        self.delegation_runtime = Some(runtime);
        self
    }

    pub fn registry(&self) -> &IndexRegistry<SkillIndex> {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut IndexRegistry<SkillIndex> {
        &mut self.registry
    }

    #[instrument(skip(self, args), fields(skill = %name))]
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

    #[instrument(skip(self, args), fields(skill = %name))]
    pub async fn execute(&self, name: &str, args: Option<&str>) -> SkillResult {
        if let Some(skill) = self.registry.get(name)
            && skill.disable_model_invocation
        {
            return SkillResult::error(format!(
                "Skill '{}' is manual-only and must be invoked explicitly via /{}",
                skill.name, skill.name
            ));
        }

        self.execute_explicit(name, args).await
    }

    #[instrument(skip(self, args), fields(skill = %name))]
    pub async fn execute_explicit(&self, name: &str, args: Option<&str>) -> SkillResult {
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
        let args = extract_trigger_args(input, &skill);
        Some(self.execute(skill.name(), args.as_deref()).await)
    }

    pub fn list_model_invocable_skills(&self) -> Vec<&SkillIndex> {
        list_model_invocable_skills(&self.registry)
    }

    pub fn has_skill(&self, name: &str) -> bool {
        self.registry.contains(name)
    }

    pub fn find_by_command(&self, input: &str) -> Option<&SkillIndex> {
        find_explicit_command(&self.registry, input)
    }

    pub fn parse_explicit_command(&self, input: &str) -> Option<(String, Option<String>)> {
        parse_explicit_command(&self.registry, input)
    }

    pub fn find_by_trigger(&self, input: &str) -> Option<&SkillIndex> {
        find_first_trigger_match(&self.registry, input)
    }

    pub fn build_summary(&self) -> String {
        build_model_invocable_summary(&self.registry)
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
        if let Some(ref runtime) = self.delegation_runtime {
            return match runtime.execute_skill_fork(&spec).await {
                Ok(result) => result,
                Err(error) => SkillResult::error(error.to_string())
                    .execution_kind(SkillExecutionKind::Fork)
                    .agent(spec.agent.clone()),
            };
        }
        SkillResult::error("Forked skills require a bound delegation runtime")
            .execution_kind(SkillExecutionKind::Fork)
            .agent(spec.agent)
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
        skill.agent = Some("explore".to_string());
        registry.register(skill);

        let runtime = SkillRuntime::new(registry);
        let spec = runtime.load_spec("research", Some("topic")).await.unwrap();
        assert_eq!(spec.execution_kind, SkillExecutionKind::Fork);
        assert_eq!(spec.agent.as_deref(), Some("explore"));
    }

    #[tokio::test]
    async fn manual_only_skill_requires_explicit_execution() {
        let mut registry = IndexRegistry::new();
        let mut skill =
            SkillIndex::new("internal", "Internal skill").source(ContentSource::in_memory("X"));
        skill.disable_model_invocation = true;
        registry.register(skill);

        let runtime = SkillRuntime::new(registry);
        let result = runtime.execute("internal", None).await;
        assert!(!result.success);

        let result = runtime.execute_explicit("internal", None).await;
        assert!(result.success);
    }

    #[test]
    fn parse_explicit_command_extracts_name_and_args() {
        let mut registry = IndexRegistry::new();
        registry.register(
            SkillIndex::new("commit", "Commit helper")
                .source(ContentSource::in_memory("Commit: $ARGUMENTS")),
        );

        let runtime = SkillRuntime::new(registry);
        let parsed = runtime.parse_explicit_command("/commit -m test");
        assert_eq!(
            parsed,
            Some(("commit".to_string(), Some("-m test".to_string())))
        );
    }
}
