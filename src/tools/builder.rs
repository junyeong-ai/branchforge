//! Tool registry builder.

use std::path::PathBuf;
use std::sync::Arc;

use super::ProcessManager;
use super::context::ExecutionContext;
use super::env::ToolExecutionEnv;
use super::registry::ToolRegistry;
use super::surface::ToolSurface;
use super::traits::Tool;
use crate::agent::{TaskOutputTool, TaskRegistry, TaskTool};
use crate::authorization::AuthorizationPolicy;
use crate::common::IndexRegistry;
use crate::hooks::HookManager;
use crate::session::session_state::ToolState;
use crate::session::{MemoryPersistence, SessionAccessScope, SessionId, SessionManager};
use crate::subagents::SubagentIndex;

pub struct ToolRegistryBuilder {
    access: ToolSurface,
    working_dir: Option<PathBuf>,
    task_registry: Option<TaskRegistry>,
    skill_executor: Option<crate::skills::SkillRuntime>,
    subagent_registry: Option<IndexRegistry<SubagentIndex>>,
    policy: Option<AuthorizationPolicy>,
    sandbox_config: Option<crate::security::SandboxConfig>,
    tool_state: Option<ToolState>,
    session_id: Option<SessionId>,
    session_manager: Option<SessionManager>,
    hooks: Option<HookManager>,
    scope: Option<SessionAccessScope>,
    delegation_runtime: Option<crate::agent::DelegationRuntime>,
    custom_tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistryBuilder {
    fn effective_authorization_policy(&self) -> AuthorizationPolicy {
        self.policy
            .clone()
            .unwrap_or_else(|| self.access.default_policy())
    }

    pub fn new() -> Self {
        Self {
            access: ToolSurface::default(),
            working_dir: None,
            task_registry: None,
            skill_executor: None,
            subagent_registry: None,
            policy: None,
            sandbox_config: None,
            tool_state: None,
            session_id: None,
            session_manager: None,
            hooks: None,
            scope: None,
            delegation_runtime: None,
            custom_tools: Vec::new(),
        }
    }

    /// Register a custom tool. Custom tools participate in access filtering
    /// and the full execution pipeline (security, authorization, plan mode).
    pub fn custom_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.custom_tools.push(tool);
        self
    }

    /// Register multiple custom tools at once.
    pub fn custom_tools(mut self, tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        self.custom_tools.extend(tools);
        self
    }

    pub fn access(mut self, access: ToolSurface) -> Self {
        self.access = access;
        self
    }

    pub fn working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    pub fn task_registry(mut self, registry: TaskRegistry) -> Self {
        self.task_registry = Some(registry);
        self
    }

    pub fn skill_executor(mut self, executor: crate::skills::SkillRuntime) -> Self {
        self.skill_executor = Some(executor);
        self
    }

    pub fn subagent_registry(mut self, registry: IndexRegistry<SubagentIndex>) -> Self {
        self.subagent_registry = Some(registry);
        self
    }

    pub fn policy(mut self, policy: AuthorizationPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    pub fn sandbox_config(mut self, config: crate::security::SandboxConfig) -> Self {
        self.sandbox_config = Some(config);
        self
    }

    pub fn tool_state(mut self, state: ToolState) -> Self {
        self.tool_state = Some(state);
        self
    }

    pub fn session_id(mut self, id: SessionId) -> Self {
        self.session_id = Some(id);
        self
    }

    pub fn session_manager(mut self, manager: SessionManager) -> Self {
        self.session_manager = Some(manager);
        self
    }

    pub fn hooks(mut self, hooks: HookManager) -> Self {
        self.hooks = Some(hooks);
        self
    }

    pub fn scope(mut self, scope: SessionAccessScope) -> Self {
        self.scope = Some(scope);
        self
    }

    pub(crate) fn delegation_runtime(mut self, runtime: crate::agent::DelegationRuntime) -> Self {
        self.delegation_runtime = Some(runtime);
        self
    }

    pub fn build(self) -> ToolRegistry {
        let access = &self.access;
        let authorization_policy = self.effective_authorization_policy();
        let wd = self
            .working_dir
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        let sandbox_config = self
            .sandbox_config
            .unwrap_or_else(|| crate::security::SandboxConfig::disabled().working_dir(wd.clone()));

        let security = crate::security::SecurityContext::builder()
            .root(&wd)
            .sandbox(sandbox_config)
            .build()
            .map(|mut security| {
                security.policy = crate::security::SecurityPolicy::new(authorization_policy);
                security
            })
            .unwrap_or_else(|_| crate::security::SecurityContext::permissive());

        let session_id = self.session_id.unwrap_or_default();
        let mut context = ExecutionContext::new(security);
        if let Some(ref manager) = self.session_manager {
            context = context.session_manager(manager.clone());
        }
        if let Some(ref hooks) = self.hooks {
            context = context.hooks(hooks.clone(), session_id.to_string());
        }
        if let Some(ref scope) = self.scope {
            context = context.session_scope(scope.clone());
        }
        let task_registry = self.task_registry.unwrap_or_else(|| {
            if let Some(ref manager) = self.session_manager {
                let registry = TaskRegistry::new(manager.persistence());
                if let Some(ref parent_session_id) = self.session_id {
                    registry.parent_session(*parent_session_id)
                } else {
                    registry
                }
            } else {
                TaskRegistry::new(Arc::new(MemoryPersistence::new()))
            }
        });
        let process_manager = Arc::new(ProcessManager::new());
        let tool_state = self
            .tool_state
            .unwrap_or_else(|| ToolState::new(session_id));

        let mut task_tool_builder = TaskTool::new(task_registry.clone());
        if let Some(manager) = self.session_manager.clone() {
            task_tool_builder = task_tool_builder.session_manager(manager);
        }
        if let Some(runtime) = self.delegation_runtime.clone() {
            task_tool_builder = task_tool_builder.delegation_runtime(runtime);
        }
        let task_tool: Arc<dyn Tool> = match self.subagent_registry {
            Some(sr) => Arc::new(task_tool_builder.subagent_registry(sr)),
            None => Arc::new(task_tool_builder),
        };

        let skill_tool: Arc<dyn Tool> = match self.skill_executor {
            Some(executor) => Arc::new(crate::skills::SkillTool::new(executor)),
            None => Arc::new(crate::skills::SkillTool::defaults()),
        };

        let mut all_tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(super::ReadTool),
            Arc::new(super::WriteTool),
            Arc::new(super::EditTool),
            Arc::new(super::GlobTool),
            Arc::new(super::GrepTool),
            Arc::new(super::BashTool::process_manager(process_manager.clone())),
            Arc::new(super::KillShellTool::process_manager(
                process_manager.clone(),
            )),
            task_tool,
            Arc::new(TaskOutputTool::new(task_registry.clone())),
            Arc::new(super::TodoWriteTool::new(tool_state.clone(), session_id)),
            Arc::new(super::PlanTool::new(tool_state.clone())),
            skill_tool,
        ];
        if self.session_manager.is_some() {
            all_tools.push(Arc::new(super::GraphHistoryTool));
        }
        all_tools.extend(self.custom_tools);

        let env = ToolExecutionEnv {
            context,
            tool_state: Some(tool_state),
            process_manager: Some(process_manager),
        };

        let mut registry = ToolRegistry::from_env(task_registry, env);

        for tool in all_tools {
            if access.is_allowed(tool.name()) {
                registry.register(tool);
            }
        }

        registry
    }
}

impl Default for ToolRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::AuthorizationPolicy;

    #[tokio::test]
    async fn default_builder_policy_allows_visible_tools() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("value.txt");
        tokio::fs::write(&file, "visible").await.unwrap();

        let registry = ToolRegistryBuilder::new()
            .access(ToolSurface::only(["Read"]))
            .working_dir(dir.path())
            .build();

        let result = registry
            .execute(
                "Read",
                serde_json::json!({
                    "file_path": file,
                }),
            )
            .await;

        assert!(!result.is_error(), "visible tool should execute by default");
    }

    #[tokio::test]
    async fn explicit_default_authorization_policy_still_denies_without_rules() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("value.txt");
        tokio::fs::write(&file, "visible").await.unwrap();

        let registry = ToolRegistryBuilder::new()
            .access(ToolSurface::only(["Read"]))
            .policy(AuthorizationPolicy::default())
            .working_dir(dir.path())
            .build();

        let result = registry
            .execute(
                "Read",
                serde_json::json!({
                    "file_path": file,
                }),
            )
            .await;

        assert!(
            result.is_error(),
            "explicit default policy should remain fail-closed"
        );
        assert!(
            result
                .error_message()
                .contains("Rules mode: tool not explicitly allowed"),
            "expected permission-denied error, got {}",
            result.error_message()
        );
    }
}
