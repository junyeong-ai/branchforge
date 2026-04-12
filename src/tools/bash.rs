//! Bash tool - shell command execution with security hardening.

#![allow(missing_docs)]

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
use super::process::ProcessScheduler;
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
    process_manager: Arc<ProcessScheduler>,
}

impl BashTool {
    pub fn new(manager: Arc<ProcessScheduler>) -> Self {
        Self {
            process_manager: manager,
        }
    }

    pub fn process_manager(&self) -> &Arc<ProcessScheduler> {
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

        // Race timeout, child wait, and optional cancellation
        enum WaitOutcome {
            Completed(std::io::Result<std::process::ExitStatus>),
            TimedOut,
            Cancelled,
        }

        let cancel_token = context.cancel_token().cloned();
        let outcome = tokio::select! {
            r = timeout(timeout_duration, child.wait()) => {
                match r {
                    Ok(status) => WaitOutcome::Completed(status),
                    Err(_) => WaitOutcome::TimedOut,
                }
            }
            _ = async {
                if let Some(ref token) = cancel_token {
                    token.cancelled().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                WaitOutcome::Cancelled
            }
        };

        match outcome {
            WaitOutcome::Completed(Ok(status)) => {
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
            WaitOutcome::Completed(Err(e)) => {
                ToolResult::error(format!("Failed to execute command: {}", e))
            }
            WaitOutcome::TimedOut => {
                // Timeout: explicitly kill and wait to prevent zombie process
                let _ = child.kill().await;
                let _ = child.wait().await;
                ToolResult::error(format!(
                    "Command timed out after {} seconds",
                    timeout_ms / 1000
                ))
            }
            WaitOutcome::Cancelled => {
                // Cancellation: kill child process and clean up
                let _ = child.kill().await;
                let _ = child.wait().await;
                ToolResult::error("Command cancelled")
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
        Self::new(Arc::new(ProcessScheduler::new()))
    }
}

#[async_trait]
impl SchemaTool for BashTool {
    type Input = BashInput;

    const NAME: &'static str = "Bash";
    const SEARCH_HINT: Option<&'static str> = Some("execute a bash shell command");
    const DESCRIPTION: &'static str = "Execute a bash command with optional timeout (default 120s, max 600s). Use `run_in_background: true` for long-running commands. Quote paths with spaces. Output is truncated at 30000 characters.";

    fn is_read_only_typed(&self, input: &BashInput) -> bool {
        // Input-aware: a Bash command is read-only only when the
        // shell AST contains no mutating primitive. Reuse the
        // existing BashAnalyzer heuristics via its `is_read_only`
        // classification helper if available; default to false.
        crate::security::bash::BashAnalyzer::classify_read_only(&input.command)
    }

    fn is_concurrency_safe_typed(&self, input: &BashInput) -> bool {
        // A command is parallel-safe only if it is read-only AND
        // does not touch a shared resource outside the workspace.
        // Conservative: same as read-only status for now; refine
        // with per-command analysis in a follow-up.
        crate::security::bash::BashAnalyzer::classify_read_only(&input.command)
    }

    fn is_destructive_typed(&self, input: &BashInput) -> bool {
        // Destructive ≡ irreversible without recovery: `rm`, `dd`,
        // `mkfs`, `git reset --hard`, `truncate`, `>file`, …
        crate::security::bash::BashAnalyzer::classify_destructive(&input.command)
    }

    fn permission_subjects_typed(&self, input: &BashInput) -> Vec<String> {
        // The first non-whitespace token is the command name. The
        // permission DSL matches `Bash(rm:*)` against the prefix,
        // so we surface the bare command name and let the engine
        // do pattern matching.
        input
            .command
            .split_whitespace()
            .next()
            .map(|s| s.to_string())
            .into_iter()
            .collect()
    }

    async fn handle(&self, input: BashInput, context: &ExecutionContext) -> ToolResult {
        let bypass = self.should_bypass(&input, context);

        // Layered bash validation: parse the command through the
        // tree-sitter + regex [`BashAnalyzer`] and reject it if it
        // hits any configured security concern (rm -rf, fork bombs,
        // privilege escalation, reverse shells, remote-pipe-to-sh,
        // path traversal, container escape …). Runs BEFORE sandbox
        // wrapping so blocked commands never reach the shell at all.
        //
        // An explicit `dangerouslyDisableSandbox: true` from a
        // caller with sufficient privilege also bypasses validation
        // — the same escape hatch that disables sandbox wrapping.
        if !bypass && let Err(reason) = context.validate_bash(&input.command) {
            tracing::warn!(
                command = %input.command,
                reason = %reason,
                "Bash command rejected by BashAnalyzer preflight"
            );
            return ToolResult::error(format!("Blocked by bash validator: {reason}"));
        }

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
        let tool = BashTool::default();
        let context =
            ExecutionContext::try_permissive().expect("failed to create permissive context");
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
        let tool = BashTool::default();
        let context =
            ExecutionContext::try_permissive().expect("failed to create permissive context");
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
        let tool = BashTool::default();
        let context =
            ExecutionContext::try_permissive().expect("failed to create permissive context");
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
        let tool = BashTool::default();
        let context =
            ExecutionContext::try_permissive().expect("failed to create permissive context");
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
        let tool = BashTool::default();
        let context =
            ExecutionContext::try_permissive().expect("failed to create permissive context");
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

        let tool = BashTool::default();
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
        let manager = Arc::new(ProcessScheduler::new());
        let tool1 = BashTool::new(manager.clone());
        let tool2 = BashTool::new(manager.clone());

        assert!(Arc::ptr_eq(
            tool1.process_manager(),
            tool2.process_manager()
        ));
    }

    /// Layered safety: `rm -rf /` matches the `DangerousCommand`
    /// pattern set and must be rejected by the preflight validator
    /// before reaching the shell. Without this wiring BashAnalyzer
    /// existed but was never called.
    #[tokio::test]
    async fn rejects_dangerous_rm_rf_root() {
        let tool = BashTool::default();
        let context =
            ExecutionContext::try_permissive().expect("failed to create permissive context");
        let result = tool
            .execute(serde_json::json!({"command": "rm -rf /"}), &context)
            .await;

        assert!(result.is_error(), "dangerous command must be rejected");
        match &result.output {
            ToolOutput::Error(msg) => assert!(
                msg.contains("Blocked by bash validator"),
                "expected validator rejection, got {msg}"
            ),
            other => panic!("expected Error output, got {other:?}"),
        }
    }

    /// Fork-bomb pattern must not reach the shell.
    #[tokio::test]
    async fn rejects_fork_bomb() {
        let tool = BashTool::default();
        let context =
            ExecutionContext::try_permissive().expect("failed to create permissive context");
        let result = tool
            .execute(serde_json::json!({"command": ":(){ :|:& };:"}), &context)
            .await;
        assert!(result.is_error(), "fork bomb must be rejected");
    }

    /// `sudo` (privilege escalation concern) must be rejected by the
    /// default policy even though the concern is emulated-allowed
    /// in the permissive bash policy. `ExecutionContext::try_permissive`
    /// uses `BashPolicy::default()` (not `BashPolicy::permissive()`),
    /// which denies every concern including privilege escalation.
    #[tokio::test]
    async fn rejects_privilege_escalation() {
        let tool = BashTool::default();
        let context =
            ExecutionContext::try_permissive().expect("failed to create permissive context");
        let result = tool
            .execute(serde_json::json!({"command": "sudo rm /tmp/x"}), &context)
            .await;
        assert!(result.is_error(), "sudo must be rejected by default policy");
    }
}
