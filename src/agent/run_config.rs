//! Per-execution configuration overrides.

use std::time::Duration;

use crate::authorization::ExecutionMode;

/// Overrides applied to a single execution without mutating the shared [`super::AgentConfig`].
///
/// Any field set to `Some` overrides the corresponding value from the agent's
/// configuration for that execution only.
#[derive(Clone, Debug, Default)]
pub struct RunConfig {
    model: Option<String>,
    max_tokens: Option<u32>,
    max_iterations: Option<usize>,
    timeout: Option<Duration>,
    execution_mode: Option<ExecutionMode>,
    system_prompt_override: Option<String>,
}

impl RunConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = Some(max);
        self
    }

    pub fn max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = Some(max);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = Some(mode);
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt_override = Some(prompt.into());
        self
    }

    // Accessors — public so SDK users can inspect the configuration.

    /// Returns the model override, or `default` if none was set.
    pub fn effective_model<'a>(&'a self, default: &'a str) -> &'a str {
        self.model.as_deref().unwrap_or(default)
    }

    /// Returns the max-iterations override, or `default` if none was set.
    pub fn effective_max_iterations(&self, default: usize) -> usize {
        self.max_iterations.unwrap_or(default)
    }

    /// Returns the execution-mode override, or `default` if none was set.
    pub fn effective_execution_mode<'a>(&'a self, default: &'a ExecutionMode) -> &'a ExecutionMode {
        self.execution_mode.as_ref().unwrap_or(default)
    }

    /// Returns the system-prompt override, if set.
    pub fn system_prompt_override(&self) -> Option<&str> {
        self.system_prompt_override.as_deref()
    }

    /// Returns the model override, if set.
    pub fn model_override(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Returns the max-tokens override, if set.
    pub fn max_tokens_override(&self) -> Option<u32> {
        self.max_tokens
    }

    /// Returns the timeout override, if set.
    pub fn timeout_override(&self) -> Option<Duration> {
        self.timeout
    }
}
