//! Tool registry for managing and executing tools.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

/// Default timeout for tool execution in milliseconds (2 minutes).
const DEFAULT_TOOL_TIMEOUT_MS: u64 = 120_000;

use super::ProcessManager;
use super::builder::ToolRegistryBuilder;
use super::context::ExecutionContext;
use super::env::ToolExecutionEnv;
use super::surface::ToolSurface;
use super::traits::Tool;
use crate::agent::TaskRegistry;
use crate::authorization::ToolPolicy;
use crate::session::MemoryPersistence;
use crate::types::{ToolDefinition, ToolOutput, ToolResult};
use std::path::PathBuf;

#[derive(Clone)]
pub struct ToolRegistry {
    tools: DashMap<String, Arc<dyn Tool>>,
    task_registry: TaskRegistry,
    env: ToolExecutionEnv,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: DashMap::new(),
            task_registry: TaskRegistry::new(Arc::new(MemoryPersistence::new())),
            env: ToolExecutionEnv::default(),
        }
    }

    pub(crate) fn from_env(task_registry: TaskRegistry, env: ToolExecutionEnv) -> Self {
        Self {
            tools: DashMap::new(),
            task_registry,
            env,
        }
    }

    pub fn builder() -> ToolRegistryBuilder {
        ToolRegistryBuilder::new()
    }

    pub fn from_context(context: ExecutionContext) -> Self {
        Self {
            tools: DashMap::new(),
            task_registry: TaskRegistry::new(Arc::new(MemoryPersistence::new())),
            env: ToolExecutionEnv::new(context),
        }
    }

    pub fn default_tools(
        access: ToolSurface,
        working_dir: Option<PathBuf>,
        policy: Option<ToolPolicy>,
    ) -> Self {
        let mut builder = ToolRegistryBuilder::new().access(access);
        if let Some(dir) = working_dir {
            builder = builder.working_dir(dir);
        }
        if let Some(p) = policy {
            builder = builder.policy(p);
        }
        builder.build()
    }

    #[inline]
    pub fn context(&self) -> &ExecutionContext {
        self.env.context()
    }

    #[inline]
    pub fn tool_state(&self) -> Option<&crate::session::session_state::ToolState> {
        self.env.tool_state()
    }

    #[inline]
    pub fn process_manager(&self) -> Option<&Arc<ProcessManager>> {
        self.env.process_manager()
    }

    #[inline]
    pub fn env(&self) -> &ToolExecutionEnv {
        &self.env
    }

    #[inline]
    pub fn task_registry(&self) -> &TaskRegistry {
        &self.task_registry
    }

    pub fn register(&self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    #[inline]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).map(|r| Arc::clone(r.value()))
    }

    pub async fn execute(&self, name: &str, input: serde_json::Value) -> ToolResult {
        let tool = match self.tools.get(name) {
            Some(t) => Arc::clone(t.value()),
            None => return ToolResult::unknown_tool(name),
        };

        // Security validation first — catches structural violations
        // regardless of tool policy
        if let Err(e) = self.env.context().validate_security(name, &input) {
            return ToolResult::security_error(e);
        }

        let decision = self.env.context().check_tool_policy(name, &input);
        if decision.is_denied() {
            return ToolResult::authorization_denied(name, decision.reason());
        }

        let limits = self.env.context().limits_for(name);
        let timeout_ms = limits.timeout_ms.unwrap_or(DEFAULT_TOOL_TIMEOUT_MS);

        let result = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            tool.execute(input, self.env.context()),
        )
        .await;

        match result {
            Ok(tool_result) => self.apply_output_limits(tool_result, &limits),
            Err(_) => ToolResult::timeout(timeout_ms),
        }
    }

    fn apply_output_limits(
        &self,
        mut result: ToolResult,
        limits: &crate::authorization::ToolLimits,
    ) -> ToolResult {
        if let Some(max_size) = limits.max_output_size
            && let ToolOutput::Success(ref content) = result.output
            && content.len() > max_size
        {
            let truncated = format!(
                "{}...\n(output truncated at {} bytes)",
                &content[..content.floor_char_boundary(max_size)],
                max_size
            );
            result.output = ToolOutput::Success(truncated);
        }
        result
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|r| r.value().definition()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|r| r.key().clone()).collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn register_dynamic(&self, tool: Arc<dyn Tool>) -> crate::Result<()> {
        let name = tool.name().to_string();
        if self.tools.contains_key(&name) {
            return Err(crate::Error::Config(format!(
                "Tool already registered: {}",
                name
            )));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    pub fn register_or_replace(&self, tool: Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
        let name = tool.name().to_string();
        self.tools.insert(name, tool)
    }

    pub fn unregister(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.remove(name).map(|(_, v)| v)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::surface::ToolSurface;

    #[test]
    fn test_tool_output() {
        assert!(!ToolOutput::success("ok").is_error());
        assert!(ToolOutput::error("fail").is_error());
        assert!(!ToolOutput::empty().is_error());
    }

    #[test]
    fn test_default_tools_count() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        assert!(registry.contains("Read"));
        assert!(registry.contains("Write"));
        assert!(registry.contains("Edit"));
        assert!(registry.contains("Glob"));
        assert!(registry.contains("Grep"));
        assert!(registry.contains("Bash"));
        assert!(registry.contains("KillShell"));
        assert!(registry.contains("Task"));
        assert!(registry.contains("TaskOutput"));
        assert!(registry.contains("TodoWrite"));
        assert!(registry.contains("Plan"));
        assert!(registry.contains("Skill"));
        assert!(!registry.contains("GraphHistory"));
    }

    #[test]
    fn test_tool_surface_filtering() {
        let registry =
            ToolRegistry::default_tools(ToolSurface::only(["Read", "Write"]), None, None);
        assert!(registry.contains("Read"));
        assert!(registry.contains("Write"));
        assert!(!registry.contains("Bash"));
    }

    #[test]
    fn test_register_dynamic() {
        let registry = ToolRegistry::new();
        let tool: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);

        assert!(registry.register_dynamic(tool.clone()).is_ok());
        assert!(registry.contains("Read"));

        let result = registry.register_dynamic(tool);
        assert!(result.is_err());
    }

    #[test]
    fn test_register_or_replace() {
        let registry = ToolRegistry::new();
        let tool1: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);
        let tool2: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);

        let old = registry.register_or_replace(tool1);
        assert!(old.is_none());

        let old = registry.register_or_replace(tool2);
        assert!(old.is_some());
    }

    #[test]
    fn test_execution_mode_plan_allows_and_blocks() {
        use crate::authorization::ExecutionMode;

        let mode = ExecutionMode::Plan;

        // Plan mode allows exploration tools
        assert!(mode.allows_tool("Read"));
        assert!(mode.allows_tool("Glob"));
        assert!(mode.allows_tool("Grep"));
        assert!(mode.allows_tool("TodoWrite"));
        assert!(mode.allows_tool("Plan"));
        assert!(mode.allows_tool("GraphHistory"));

        // Plan mode blocks mutation tools
        assert!(!mode.allows_tool("Write"));
        assert!(!mode.allows_tool("Edit"));
        assert!(!mode.allows_tool("Bash"));

        // Non-plan modes allow all tools
        let auto = ExecutionMode::Auto;
        assert!(auto.allows_tool("Write"));
        assert!(auto.allows_tool("Bash"));
    }

    #[test]
    fn test_unregister() {
        let registry = ToolRegistry::new();
        let tool: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);

        registry.register(tool);
        assert!(registry.contains("Read"));

        let removed = registry.unregister("Read");
        assert!(removed.is_some());
        assert!(!registry.contains("Read"));

        let removed = registry.unregister("NonExistent");
        assert!(removed.is_none());
    }
}
