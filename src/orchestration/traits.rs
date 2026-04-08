//! Coordination trait and supporting types.

use std::sync::Arc;

use super::directory::AgentDirectory;
use crate::agent::ModelConfig;
use crate::tools::Tool;

/// Context provided to [`Coordination`] methods.
pub struct CoordinationContext<'a> {
    /// Registry of active agents for inter-agent messaging.
    pub agent_directory: &'a AgentDirectory,
    /// Names of tools available to the coordinator agent.
    pub available_tools: &'a [String],
    /// Model configuration for worker selection.
    pub model_config: &'a ModelConfig,
}

/// Pluggable multi-agent coordination pattern.
///
/// Implementations configure the *environment* for multi-agent work:
/// system prompt supplements, additional tools, and worker constraints.
/// The LLM itself decides how to decompose tasks and spawn workers.
///
/// This is NOT an orchestration algorithm — it is an environment configurator.
/// The coordinator agent uses tools (Task, SendMessage) to manage workers.
pub trait Coordination: Send + Sync {
    /// Human-readable name for logging and diagnostics.
    fn name(&self) -> &str;

    /// Additional instructions appended to the system prompt.
    ///
    /// Should describe the coordination workflow, worker capabilities,
    /// and synthesis expectations.
    fn system_prompt_supplement(&self, ctx: &CoordinationContext) -> String;

    /// Additional tools made available when this coordination is active.
    ///
    /// Typically includes [`SendMessageTool`](super::SendMessageTool) for inter-agent messaging.
    fn additional_tools(&self, ctx: &CoordinationContext) -> Vec<Arc<dyn Tool>>;

    /// Constraints applied when spawning worker agents.
    fn worker_constraints(&self) -> super::worker::WorkerConstraints;
}
