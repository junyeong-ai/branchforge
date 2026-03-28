//! Command-based hooks that execute shell commands.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{Hook, HookContext, HookEvent, HookInput, HookOutput};
use crate::config::{HookConfig, HooksConfig};

pub struct CommandHook {
    name: String,
    command: String,
    events: Vec<HookEvent>,
    tool_pattern: Option<Regex>,
    timeout_secs: u64,
    extra_env: HashMap<String, String>,
}

impl CommandHook {
    pub fn new(
        name: impl Into<String>,
        command: impl Into<String>,
        events: Vec<HookEvent>,
    ) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            events,
            tool_pattern: None,
            timeout_secs: 60,
            extra_env: HashMap::new(),
        }
    }

    pub fn matcher(mut self, pattern: &str) -> Self {
        self.tool_pattern = Regex::new(pattern).ok();
        self
    }

    pub fn timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.insert(key.into(), value.into());
        self
    }

    pub fn from_settings(settings: &HooksConfig) -> Vec<Self> {
        let mut hooks = Vec::new();

        for (name, config) in &settings.pre_tool_use {
            hooks.push(Self::from_event_config(name, HookEvent::PreToolUse, config));
        }

        for (name, config) in &settings.post_tool_use {
            hooks.push(Self::from_event_config(
                name,
                HookEvent::PostToolUse,
                config,
            ));
        }

        for (i, config) in settings.session_start.iter().enumerate() {
            hooks.push(Self::from_event_config(
                format!("session-start-{}", i),
                HookEvent::SessionStart,
                config,
            ));
        }

        for (i, config) in settings.session_end.iter().enumerate() {
            hooks.push(Self::from_event_config(
                format!("session-end-{}", i),
                HookEvent::SessionEnd,
                config,
            ));
        }

        hooks
    }

    pub fn from_event_config(
        name: impl Into<String>,
        event: HookEvent,
        config: &HookConfig,
    ) -> Self {
        let (command, matcher, timeout) = Self::parse_config(config);
        let mut hook = Self::new(name, command, vec![event]);
        if let Some(m) = matcher {
            hook = hook.matcher(&m);
        }
        if let Some(t) = timeout {
            hook = hook.timeout(t);
        }
        hook
    }

    fn parse_config(config: &HookConfig) -> (String, Option<String>, Option<u64>) {
        match config {
            HookConfig::Command(cmd) => (cmd.clone(), None, None),
            HookConfig::Full {
                command,
                timeout_secs,
                matcher,
            } => (command.clone(), matcher.clone(), *timeout_secs),
        }
    }
}

#[async_trait]
impl Hook for CommandHook {
    fn name(&self) -> &str {
        &self.name
    }

    fn events(&self) -> &[HookEvent] {
        &self.events
    }

    fn tool_matcher(&self) -> Option<&Regex> {
        self.tool_pattern.as_ref()
    }

    fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    async fn execute(
        &self,
        input: HookInput,
        hook_context: &HookContext,
    ) -> Result<HookOutput, crate::Error> {
        let input_json = serde_json::to_string(&InputPayload::from_input(&input))
            .map_err(|e| crate::Error::Config(format!("Failed to serialize hook input: {}", e)))?;

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&self.command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .current_dir(
                hook_context
                    .cwd
                    .as_deref()
                    .unwrap_or(std::path::Path::new(".")),
            )
            .envs(&hook_context.env)
            .envs(&self.extra_env)
            .spawn()
            .map_err(|e| crate::Error::Config(format!("Failed to spawn hook command: {}", e)))?;

        if let Some(mut stdin) = child.stdin.take() {
            // Write input and drop stdin to send EOF.
            // The child may exit before consuming stdin (e.g., `echo` ignores stdin),
            // which causes BrokenPipe — this is expected, not an error.
            // Only the child's exit code determines success.
            if let Err(e) = stdin.write_all(input_json.as_bytes()).await
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                tracing::warn!(hook = %self.name, error = %e, "Hook stdin write failed");
            }
            drop(stdin);
        }

        let timeout = Duration::from_secs(self.timeout_secs);
        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| crate::Error::Timeout(timeout))?
            .map_err(|e| crate::Error::Config(format!("Hook command failed: {}", e)))?;

        if !output.status.success() {
            return Ok(HookOutput::block(format!(
                "Hook '{}' failed with exit code: {:?}",
                self.name,
                output.status.code()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Ok(HookOutput::allow());
        }

        match serde_json::from_str::<OutputPayload>(stdout.trim()) {
            Ok(payload) => Ok(payload.into_output()),
            Err(_) => Ok(HookOutput::allow()),
        }
    }
}

#[derive(serde::Serialize)]
struct InputPayload {
    event: String,
    session_id: String,
    tool_name: Option<String>,
    tool_input: Option<serde_json::Value>,
}

impl InputPayload {
    fn from_input(input: &HookInput) -> Self {
        Self {
            event: input.event_type().to_string(),
            session_id: input.session_id.clone(),
            tool_name: input.tool_name().map(String::from),
            tool_input: input.data.tool_input().cloned(),
        }
    }
}

#[derive(serde::Deserialize)]
struct OutputPayload {
    #[serde(default = "default_true")]
    continue_execution: bool,
    stop_reason: Option<String>,
    updated_input: Option<serde_json::Value>,
}

use crate::common::serde_defaults::default_true;

impl OutputPayload {
    fn into_output(self) -> HookOutput {
        HookOutput {
            continue_execution: self.continue_execution,
            stop_reason: self.stop_reason,
            updated_input: self.updated_input,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_hook_creation() {
        let hook = CommandHook::new("test", "echo hello", vec![HookEvent::PreToolUse])
            .matcher("Bash")
            .timeout(30);

        assert_eq!(hook.name(), "test");
        assert!(hook.tool_matcher().is_some());
        assert_eq!(hook.timeout_secs(), 30);
    }

    #[test]
    fn test_from_settings() {
        let mut settings = HooksConfig::default();
        settings.pre_tool_use.insert(
            "security".to_string(),
            HookConfig::Full {
                command: "check-security.sh".to_string(),
                timeout_secs: Some(10),
                matcher: Some("Bash".to_string()),
            },
        );

        let hooks = CommandHook::from_settings(&settings);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].name(), "security");
        assert_eq!(hooks[0].timeout_secs(), 10);
    }

    #[tokio::test]
    async fn test_command_hook_execution() {
        // echo ignores stdin and exits immediately — exercises the BrokenPipe path
        let hook = CommandHook::new("echo-test", "echo hook-ok", vec![HookEvent::PreToolUse]);

        let input = HookInput::pre_tool_use("test-session", "Read", serde_json::json!({}));
        let hook_context = HookContext::new("test-session");

        let output = hook.execute(input, &hook_context).await.unwrap();
        assert!(output.continue_execution);
    }
}
