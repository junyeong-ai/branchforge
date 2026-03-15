//! Bash tool - shell command execution with security hardening.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use super::SchemaTool;
use super::context::ExecutionContext;
use super::process::ProcessManager;
use crate::types::ToolResult;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BashInput {
    /// The command to execute
    pub command: String,
    /// Clear, concise description of what this command does in 5-10 words, in active voice.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional timeout in milliseconds (max 600000)
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Set to true to run this command in the background. Use TaskOutput to read the output later.
    #[serde(default)]
    pub run_in_background: Option<bool>,
    /// Set this to true to dangerously override sandbox mode and run commands without sandboxing.
    #[serde(default, rename = "dangerouslyDisableSandbox")]
    pub dangerously_disable_sandbox: Option<bool>,
}

pub struct BashTool {
    process_manager: Arc<ProcessManager>,
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            process_manager: Arc::new(ProcessManager::new()),
        }
    }

    pub fn process_manager(manager: Arc<ProcessManager>) -> Self {
        Self {
            process_manager: manager,
        }
    }

    pub fn get_process_manager(&self) -> &Arc<ProcessManager> {
        &self.process_manager
    }

    fn should_bypass(&self, input: &BashInput, context: &ExecutionContext) -> bool {
        if input.dangerously_disable_sandbox.unwrap_or(false) {
            return context.can_bypass_sandbox();
        }
        false
    }

    async fn execute_foreground(
        &self,
        command: &str,
        timeout_ms: u64,
        context: &ExecutionContext,
        bypass_sandbox: bool,
    ) -> ToolResult {
        let timeout_duration = Duration::from_millis(timeout_ms);
        let env = context.sanitized_env_with_sandbox();
        let limits = context.resource_limits().clone();

        let wrapped_command = if bypass_sandbox {
            command.to_string()
        } else {
            match context.wrap_command(command) {
                Ok(cmd) => cmd,
                Err(e) => return ToolResult::error(format!("Sandbox error: {}", e)),
            }
        };

        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(&wrapped_command);
        cmd.current_dir(context.root());
        cmd.env_clear();
        cmd.envs(env);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(move || {
                if let Err(e) = limits.apply() {
                    eprintln!("Warning: resource limits not applied: {e}");
                }
                Ok(())
            });
        }

        // Ensure process is killed when dropped (safety net)
        cmd.kill_on_drop(true);

        // Spawn explicitly for proper cleanup on timeout
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return ToolResult::error(format!("Failed to spawn: {}", e)),
        };

        // Take stdout/stderr handles before waiting (allows reading after wait)
        let mut stdout_handle = child.stdout.take();
        let mut stderr_handle = child.stderr.take();

        match timeout(timeout_duration, child.wait()).await {
            Ok(Ok(status)) => {
                // Read output from taken handles
                let mut stdout_buf = Vec::new();
                let mut stderr_buf = Vec::new();

                if let Some(ref mut handle) = stdout_handle {
                    let _ = handle.read_to_end(&mut stdout_buf).await;
                }
                if let Some(ref mut handle) = stderr_handle {
                    let _ = handle.read_to_end(&mut stderr_buf).await;
                }

                let stdout = String::from_utf8_lossy(&stdout_buf);
                let stderr = String::from_utf8_lossy(&stderr_buf);

                let mut combined = String::new();

                if !stdout.is_empty() {
                    combined.push_str(&stdout);
                }

                if !stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push_str("\n--- stderr ---\n");
                    }
                    combined.push_str(&stderr);
                }

                const MAX_OUTPUT: usize = 30_000;
                if combined.len() > MAX_OUTPUT {
                    combined.truncate(MAX_OUTPUT);
                    combined.push_str("\n... (output truncated)");
                }

                if combined.is_empty() {
                    combined = "(no output)".to_string();
                }

                if !status.success() {
                    let code = status.code().unwrap_or(-1);
                    combined = format!("Exit code: {}\n{}", code, combined);
                }

                ToolResult::success(combined)
            }
            Ok(Err(e)) => ToolResult::error(format!("Failed to execute command: {}", e)),
            Err(_) => {
                // Timeout: explicitly kill and wait to prevent zombie process
                let _ = child.kill().await;
                let _ = child.wait().await;
                ToolResult::error(format!(
                    "Command timed out after {} seconds",
                    timeout_ms / 1000
                ))
            }
        }
    }

    async fn execute_background(
        &self,
        command: &str,
        context: &ExecutionContext,
        bypass_sandbox: bool,
    ) -> ToolResult {
        let env = context.sanitized_env_with_sandbox();

        let wrapped_command = if bypass_sandbox {
            command.to_string()
        } else {
            match context.wrap_command(command) {
                Ok(cmd) => cmd,
                Err(e) => return ToolResult::error(format!("Sandbox error: {}", e)),
            }
        };

        match self
            .process_manager
            .spawn_with_env(&wrapped_command, context.root(), env)
            .await
        {
            Ok(id) => ToolResult::success(format!(
                "Background process started with ID: {}\nUse TaskOutput tool to monitor output.",
                id
            )),
            Err(e) => ToolResult::error(e),
        }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SchemaTool for BashTool {
    type Input = BashInput;

    const NAME: &'static str = "Bash";
    const DESCRIPTION: &'static str = "Execute a bash command with optional timeout (default 120s, max 600s). Use `run_in_background: true` for long-running commands. Quote paths with spaces. Output is truncated at 30000 characters.";

    async fn handle(&self, input: BashInput, context: &ExecutionContext) -> ToolResult {
        let bypass = self.should_bypass(&input, context);

        if input.run_in_background.unwrap_or(false) {
            self.execute_background(&input.command, context, bypass)
                .await
        } else {
            let timeout_ms = input.timeout.unwrap_or(120_000).min(600_000);
            self.execute_foreground(&input.command, timeout_ms, context, bypass)
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testing::helpers::TestContext;
    use crate::tools::{ExecutionContext, Tool};
    use crate::types::ToolOutput;

    #[tokio::test]
    async fn test_simple_command() {
        let tool = BashTool::new();
        let context = ExecutionContext::permissive();
        let result = tool
            .execute(
                serde_json::json!({"command": "echo 'hello world'"}),
                &context,
            )
            .await;

        assert!(
            matches!(&result.output, ToolOutput::Success(output) if output.contains("hello world")),
            "Expected success with 'hello world', got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_background_command() {
        let tool = BashTool::new();
        let context = ExecutionContext::permissive();
        let result = tool
            .execute(
                serde_json::json!({
                    "command": "echo done",
                    "run_in_background": true
                }),
                &context,
            )
            .await;

        assert!(
            matches!(&result.output, ToolOutput::Success(output) if output.contains("Background process started")),
            "Expected background process started, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_stderr_output() {
        let tool = BashTool::new();
        let context = ExecutionContext::permissive();
        let result = tool
            .execute(
                serde_json::json!({"command": "echo 'stdout' && echo 'stderr' >&2"}),
                &context,
            )
            .await;

        assert!(
            matches!(&result.output, ToolOutput::Success(output) if output.contains("stdout") && output.contains("stderr")),
            "Expected stdout and stderr, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_exit_code_nonzero() {
        let tool = BashTool::new();
        let context = ExecutionContext::permissive();
        let result = tool
            .execute(serde_json::json!({"command": "exit 42"}), &context)
            .await;

        assert!(
            matches!(&result.output, ToolOutput::Success(output) if output.contains("Exit code: 42")),
            "Expected exit code 42, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_short_timeout() {
        let tool = BashTool::new();
        let context = ExecutionContext::permissive();
        let result = tool
            .execute(
                serde_json::json!({
                    "command": "sleep 10",
                    "timeout": 100
                }),
                &context,
            )
            .await;

        assert!(result.is_error(), "Expected timeout error");
        assert!(
            matches!(&result.output, ToolOutput::Error(e) if e.to_string().contains("timed out")),
            "Expected timeout message, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_working_directory() {
        let test_context = TestContext::new();
        test_context.write_file("testfile.txt", "content");

        let tool = BashTool::new();
        let result = tool
            .execute(
                serde_json::json!({"command": "ls testfile.txt"}),
                &test_context.context,
            )
            .await;

        assert!(
            matches!(&result.output, ToolOutput::Success(output) if output.contains("testfile.txt")),
            "Expected testfile.txt in output, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_shared_process_manager() {
        let manager = Arc::new(ProcessManager::new());
        let tool1 = BashTool::process_manager(manager.clone());
        let tool2 = BashTool::process_manager(manager.clone());

        assert!(Arc::ptr_eq(
            tool1.get_process_manager(),
            tool2.get_process_manager()
        ));
    }
}
