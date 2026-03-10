//! Tool registry for managing and executing tools.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::ProcessManager;
use super::access::ToolAccess;
use super::builder::ToolRegistryBuilder;
use super::context::ExecutionContext;
use super::env::ToolExecutionEnv;
use super::traits::Tool;
use crate::agent::TaskRegistry;
use crate::permissions::PermissionPolicy;
use crate::session::MemoryPersistence;
use crate::types::{ToolDefinition, ToolOutput, ToolResult};
use std::path::PathBuf;

#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    task_registry: TaskRegistry,
    env: ToolExecutionEnv,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            task_registry: TaskRegistry::new(Arc::new(MemoryPersistence::new())),
            env: ToolExecutionEnv::default(),
        }
    }

    pub(crate) fn from_env(task_registry: TaskRegistry, env: ToolExecutionEnv) -> Self {
        Self {
            tools: HashMap::new(),
            task_registry,
            env,
        }
    }

    pub fn builder() -> ToolRegistryBuilder {
        ToolRegistryBuilder::new()
    }

    pub fn from_context(context: ExecutionContext) -> Self {
        Self {
            tools: HashMap::new(),
            task_registry: TaskRegistry::new(Arc::new(MemoryPersistence::new())),
            env: ToolExecutionEnv::new(context),
        }
    }

    pub fn default_tools(
        access: ToolAccess,
        working_dir: Option<PathBuf>,
        policy: Option<PermissionPolicy>,
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
    pub fn get_context(&self) -> &ExecutionContext {
        &self.env.context
    }

    #[inline]
    pub fn tool_state(&self) -> Option<&crate::session::session_state::ToolState> {
        self.env.tool_state.as_ref()
    }

    #[inline]
    pub fn process_manager(&self) -> Option<&Arc<ProcessManager>> {
        self.env.process_manager.as_ref()
    }

    #[inline]
    pub fn env(&self) -> &ToolExecutionEnv {
        &self.env
    }

    #[inline]
    pub fn task_registry(&self) -> &TaskRegistry {
        &self.task_registry
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    #[inline]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub async fn execute(&self, name: &str, input: serde_json::Value) -> ToolResult {
        let tool = match self.tools.get(name) {
            Some(t) => t,
            None => return ToolResult::unknown_tool(name),
        };

        let decision = self.env.context.check_permission(name, &input);
        if !decision.is_allowed() {
            return ToolResult::permission_denied(name, decision.reason);
        }

        if let Err(e) = self.env.context.validate_security(name, &input) {
            return ToolResult::security_error(e);
        }

        let limits = self.env.context.limits_for(name);
        let timeout_ms = limits.timeout_ms.unwrap_or(120_000);

        let result = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            tool.execute(input, &self.env.context),
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
        limits: &crate::permissions::ToolLimits,
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
        self.tools.values().map(|t| t.definition()).collect()
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn register_dynamic(&mut self, tool: Arc<dyn Tool>) -> crate::Result<()> {
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

    pub fn register_or_replace(&mut self, tool: Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
        let name = tool.name().to_string();
        self.tools.insert(name, tool)
    }

    pub fn unregister(&mut self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.remove(name)
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
    use crate::tools::access::ToolAccess;

    #[test]
    fn test_tool_output() {
        assert!(!ToolOutput::success("ok").is_error());
        assert!(ToolOutput::error("fail").is_error());
        assert!(!ToolOutput::empty().is_error());
    }

    #[test]
    fn test_default_tools_count() {
        let registry = ToolRegistry::default_tools(ToolAccess::All, None, None);
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
        assert!(registry.contains("GraphHistory"));
    }

    #[test]
    fn test_tool_access_filtering() {
        let registry = ToolRegistry::default_tools(ToolAccess::only(["Read", "Write"]), None, None);
        assert!(registry.contains("Read"));
        assert!(registry.contains("Write"));
        assert!(!registry.contains("Bash"));
    }

    #[test]
    fn test_register_dynamic() {
        let mut registry = ToolRegistry::new();
        let tool: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);

        assert!(registry.register_dynamic(tool.clone()).is_ok());
        assert!(registry.contains("Read"));

        let result = registry.register_dynamic(tool);
        assert!(result.is_err());
    }

    #[test]
    fn test_register_or_replace() {
        let mut registry = ToolRegistry::new();
        let tool1: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);
        let tool2: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);

        let old = registry.register_or_replace(tool1);
        assert!(old.is_none());

        let old = registry.register_or_replace(tool2);
        assert!(old.is_some());
    }

    #[test]
    fn test_unregister() {
        let mut registry = ToolRegistry::new();
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
