//! KillShell tool - terminates background shell processes.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use super::SchemaTool;
use super::context::ExecutionContext;
use super::process::ProcessManager;
use crate::types::ToolResult;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct KillShellInput {
    /// The ID of the background shell to kill
    pub shell_id: String,
}

pub struct KillShellTool {
    process_manager: Arc<ProcessManager>,
}

impl KillShellTool {
    pub fn new(manager: Arc<ProcessManager>) -> Self {
        Self {
            process_manager: manager,
        }
    }

    pub fn process_manager(&self) -> &Arc<ProcessManager> {
        &self.process_manager
    }
}

impl Default for KillShellTool {
    fn default() -> Self {
        Self::new(Arc::new(ProcessManager::new()))
    }
}

#[async_trait]
impl SchemaTool for KillShellTool {
    type Input = KillShellInput;

    const NAME: &'static str = "KillShell";
    const DESCRIPTION: &'static str = r#"
- Kills a running background bash shell by its ID
- Takes a shell_id parameter identifying the shell to kill
- Returns a success or failure status
- Use this tool when you need to terminate a long-running shell
- Shell IDs can be obtained from Bash tool responses when using run_in_background"#;

    async fn handle(&self, input: KillShellInput, _context: &ExecutionContext) -> ToolResult {
        match self.process_manager.kill(&input.shell_id).await {
            Ok(()) => ToolResult::success(format!("Process '{}' terminated", input.shell_id)),
            Err(e) => ToolResult::error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use crate::types::ToolOutput;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_kill_nonexistent_shell() {
        let tool = KillShellTool::default();
        let context = ExecutionContext::default();
        let result = tool
            .execute(
                serde_json::json!({"shell_id": "nonexistent_shell"}),
                &context,
            )
            .await;
        assert!(result.is_error());
    }

    #[tokio::test]
    async fn test_kill_running_process() {
        let mgr = Arc::new(ProcessManager::new());
        let id = mgr.spawn("sleep 10", &PathBuf::from("/tmp")).await.unwrap();

        let tool = KillShellTool::new(mgr.clone());
        let context = ExecutionContext::default();
        let result = tool
            .execute(serde_json::json!({"shell_id": id}), &context)
            .await;

        match &result.output {
            ToolOutput::Success(msg) => assert!(msg.contains("terminated")),
            _ => panic!("Expected success"),
        }

        assert!(!mgr.is_running(&id).await);
    }
}
