//! Tool registry builder.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "coding-tools")]
use super::ProcessScheduler;
use super::context::ExecutionContext;
use super::env::ToolExecutionEnv;
use super::registry::ToolRegistry;
use super::surface::ToolSurface;
use super::traits::Tool;
use crate::agent::{TaskOutputTool, TaskTool, TaskTracker};
use crate::authorization::ToolPolicy;
use crate::common::IndexRegistry;
use crate::hooks::HookRegistry;
use crate::session::session_handle::SessionHandle;
use crate::session::{MemoryPersistence, SessionAccessScope, SessionId, SessionManager};
use crate::subagents::SubagentIndex;

pub struct ToolRegistryBuilder {
    access: ToolSurface,
    working_dir: Option<PathBuf>,
    task_tracker: Option<TaskTracker>,
    skill_executor: Option<crate::skills::SkillRuntime>,
    subagent_registry: Option<IndexRegistry<SubagentIndex>>,
    policy: Option<ToolPolicy>,
    #[cfg(feature = "local-fs")]
    sandbox_config: Option<crate::security::SandboxConfig>,
    session_handle: Option<SessionHandle>,
    session_id: Option<SessionId>,
    session_manager: Option<SessionManager>,
    hooks: Option<HookRegistry>,
    scope: Option<SessionAccessScope>,
    delegation_runtime: Option<crate::agent::DelegationRuntime>,
    custom_tools: Vec<Arc<dyn Tool>>,
    overflow_store: Option<Arc<dyn super::OverflowStore>>,
    human_handler: Option<Arc<dyn crate::authorization::HumanInteractionHandler>>,
    explicit_context: Option<ExecutionContext>,
}

impl ToolRegistryBuilder {
    fn effective_tool_policy(&self) -> ToolPolicy {
        self.policy
            .clone()
            .unwrap_or_else(|| self.access.default_policy())
    }

    pub fn new() -> Self {
        Self {
            access: ToolSurface::default(),
            working_dir: None,
            task_tracker: None,
            skill_executor: None,
            subagent_registry: None,
            policy: None,
            #[cfg(feature = "local-fs")]
            sandbox_config: None,
            session_handle: None,
            session_id: None,
            session_manager: None,
            hooks: None,
            scope: None,
            delegation_runtime: None,
            custom_tools: Vec::new(),
            overflow_store: None,
            human_handler: None,
            explicit_context: None,
        }
    }

    /// Supply a pre-built [`ExecutionContext`] instead of letting the
    /// builder construct one from working_dir / sandbox / security.
    /// Useful in tests or when the caller owns the context lifecycle.
    pub fn context(mut self, context: ExecutionContext) -> Self {
        self.explicit_context = Some(context);
        self
    }

    /// Phase D C-2: attach a unified
    /// [`crate::authorization::HumanInteractionHandler`] so
    /// built-in HITL tools (AskUserQuestion today, more in future)
    /// can route through it via the [`super::ExecutionContext`]
    /// extensions TypeMap. Normally populated by
    /// `AgentBuilder::human_handler` — custom registry builders
    /// can call this directly.
    pub fn human_handler(
        mut self,
        handler: Arc<dyn crate::authorization::HumanInteractionHandler>,
    ) -> Self {
        self.human_handler = Some(handler);
        self
    }

    /// Phase C-7: attach an [`super::OverflowStore`] for result-size
    /// spill. When set, any tool result that exceeds the tool's
    /// `max_result_size_bytes()` is spilled to the store and the
    /// inline payload is replaced with a short preview + an
    /// [`super::OverflowRef`]. Defaults to no spill.
    pub fn overflow_store(mut self, store: Arc<dyn super::OverflowStore>) -> Self {
        self.overflow_store = Some(store);
        self
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

    pub fn task_tracker(mut self, registry: TaskTracker) -> Self {
        self.task_tracker = Some(registry);
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

    pub fn policy(mut self, policy: ToolPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    #[cfg(feature = "local-fs")]
    pub fn sandbox_config(mut self, config: crate::security::SandboxConfig) -> Self {
        self.sandbox_config = Some(config);
        self
    }

    pub fn session_handle(mut self, state: SessionHandle) -> Self {
        self.session_handle = Some(state);
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

    pub fn hooks(mut self, hooks: HookRegistry) -> Self {
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
        let tool_policy = self.effective_tool_policy();

        let wd = self
            .working_dir
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        let mut context = if let Some(ctx) = self.explicit_context {
            #[cfg(not(feature = "local-fs"))]
            let _ = tool_policy;
            ctx
        } else {

            #[cfg(feature = "local-fs")]
            let ctx = {
                let sandbox_config = self.sandbox_config.unwrap_or_else(|| {
                    crate::security::SandboxConfig::disabled().working_dir(wd.clone())
                });

                let security = crate::security::SecurityContext::builder()
                    .root(&wd)
                    .sandbox(sandbox_config)
                    .build()
                    .map(|mut security| {
                        security.policy =
                            crate::security::SecurityPolicy::new(tool_policy.clone());
                        security
                    })
                    .or_else(|_| crate::security::SecurityContext::try_permissive())
                    .expect("failed to create security context");

                ExecutionContext::new(security)
            };
            #[cfg(not(feature = "local-fs"))]
            let ctx = {
                let _ = tool_policy;
                ExecutionContext::empty()
            };
            ctx
        };

        // Always attach the workspace as an extension regardless of feature —
        // Layer 1 tools that consult it (e.g. for generating hook cwd) work
        // uniformly across builds.
        context.insert_extension(crate::Workspace::new(wd.clone()));

        // Phase D C-2: wire the unified HITL handler into the
        // execution context so tools that need human interaction
        // (currently `AskUserQuestion`, more to come) can reach it
        // through `ctx.extensions().get::<HumanInteractionExtension>()`.
        if let Some(handler) = self.human_handler.clone() {
            context.insert_extension(crate::authorization::HumanInteractionExtension::new(
                handler,
            ));
        }

        let session_id = self.session_id.unwrap_or_default();
        if let Some(ref manager) = self.session_manager {
            context = context.with_session_manager(manager.clone());
        }
        if let Some(ref hooks) = self.hooks {
            context = context.with_hooks(hooks.clone(), session_id.to_string());
        }
        if let Some(ref scope) = self.scope {
            context = context.with_session_scope(scope.clone());
        }
        let task_tracker = self.task_tracker.unwrap_or_else(|| {
            if let Some(ref manager) = self.session_manager {
                let registry = TaskTracker::new(manager.persistence());
                if let Some(ref parent_session_id) = self.session_id {
                    registry.parent_session(*parent_session_id)
                } else {
                    registry
                }
            } else {
                TaskTracker::new(Arc::new(MemoryPersistence::new()))
            }
        });
        let session_handle = self
            .session_handle
            .unwrap_or_else(|| SessionHandle::new(session_id));

        let mut task_tool_builder = TaskTool::new(task_tracker.clone());
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

        // Always available tools (Layer 1 core surface).
        let mut all_tools: Vec<Arc<dyn Tool>> = vec![
            task_tool,
            Arc::new(TaskOutputTool::new(task_tracker.clone())),
            Arc::new(super::TodoWriteTool::new(session_handle.clone(), session_id)),
            Arc::new(super::PlanTool::new(session_handle.clone())),
            Arc::new(super::AskUserQuestionTool),
            skill_tool,
        ];

        #[cfg(feature = "coding-tools")]
        let process_manager = {
            let pm = Arc::new(ProcessScheduler::new());
            all_tools.push(Arc::new(super::ReadTool));
            all_tools.push(Arc::new(super::WriteTool));
            all_tools.push(Arc::new(super::EditTool));
            all_tools.push(Arc::new(super::GlobTool));
            all_tools.push(Arc::new(super::GrepTool));
            all_tools.push(Arc::new(super::BashTool::new(pm.clone())));
            all_tools.push(Arc::new(super::KillShellTool::new(pm.clone())));
            pm
        };

        if self.session_manager.is_some() {
            all_tools.push(Arc::new(super::GraphHistoryTool));
        }

        all_tools.extend(self.custom_tools);

        #[allow(unused_mut)]
        let mut env = ToolExecutionEnv::new(context).with_session_handle(session_handle);

        #[cfg(feature = "coding-tools")]
        {
            env = env.with_process_manager(process_manager);
        }

        let mut registry = ToolRegistry::from_env(task_tracker, env);
        if let Some(store) = self.overflow_store {
            registry.set_overflow_store(store);
        }

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

#[cfg(all(test, feature = "coding-tools"))]
mod tests {
    use super::*;
    use crate::authorization::ToolPolicy;

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
    async fn explicit_default_tool_policy_still_denies_without_rules() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("value.txt");
        tokio::fs::write(&file, "visible").await.unwrap();

        let registry = ToolRegistryBuilder::new()
            .access(ToolSurface::only(["Read"]))
            .policy(ToolPolicy::default())
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
                .to_lowercase()
                .contains("no matching rule"),
            "expected permission-denied error, got {}",
            result.error_message()
        );
    }
}
