//! Agent events and result types.

use super::state::{AgentMetrics, AgentState};
use crate::types::{Message, StopReason, Usage};

/// Events emitted during agent execution.
///
/// These events provide real-time visibility into the agent's progress:
/// text streaming, tool lifecycle, token consumption, and final result.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Incremental text output from the model.
    Text(String),
    /// Model thinking/reasoning output.
    Thinking(String),
    /// A tool is about to be executed.
    ToolStart {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// A tool requires user review before execution (emitted by Supervised/SupervisedFor execution modes).
    ToolReview {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// A tool execution completed.
    ToolComplete {
        id: String,
        name: String,
        output: String,
        is_error: bool,
        duration_ms: u64,
    },
    /// A tool was blocked by a security hook.
    ToolBlocked {
        id: String,
        name: String,
        reason: String,
    },
    /// Per-turn token usage (emitted after each API call).
    ///
    /// Fields match [`AgentMetrics`] naming for consistency.
    /// `cache_creation_tokens` = Anthropic's `cache_creation_input_tokens`.
    TurnUsage {
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
        total_input_tokens: u64,
        total_output_tokens: u64,
    },
    /// Final execution result.
    Complete(Box<AgentResult>),
}

/// Result of agent execution.
#[derive(Debug, Clone)]
pub struct AgentResult {
    pub text: String,
    pub usage: Usage,
    pub tool_calls: usize,
    pub iterations: usize,
    pub stop_reason: StopReason,
    pub state: AgentState,
    pub metrics: AgentMetrics,
    pub session_id: String,
    pub structured_output: Option<serde_json::Value>,
    pub messages: Vec<Message>,
    /// Unique identifier for this result (like CLI's uuid).
    pub uuid: String,
}

impl AgentResult {
    pub(crate) fn new(
        text: String,
        usage: Usage,
        iterations: usize,
        stop_reason: StopReason,
        metrics: AgentMetrics,
        session_id: String,
        structured_output: Option<serde_json::Value>,
        messages: Vec<Message>,
    ) -> Self {
        Self {
            tool_calls: metrics.tool_calls,
            state: AgentState::Completed,
            uuid: uuid::Uuid::new_v4().to_string(),
            text,
            usage,
            iterations,
            stop_reason,
            metrics,
            session_id,
            structured_output,
            messages,
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn total_tokens(&self) -> u32 {
        self.usage.total()
    }

    #[must_use]
    pub fn metrics(&self) -> &AgentMetrics {
        &self.metrics
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn extract<T: serde::de::DeserializeOwned>(&self) -> crate::Result<T> {
        let value = self
            .structured_output
            .as_ref()
            .ok_or_else(|| crate::Error::Parse("No structured output available".to_string()))?;
        serde_json::from_value(value.clone()).map_err(|e| crate::Error::Parse(e.to_string()))
    }
}
