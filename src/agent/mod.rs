//! Agent execution engine.

mod common;
mod config;
mod delegation;
mod events;
mod execution;
mod executor;
mod options;
mod request;
pub mod run_config;
pub mod run_descriptor;
pub mod runtime;
mod state;
mod state_formatter;
mod streaming;
mod task;
mod task_output;
mod task_registry;

#[cfg(test)]
mod tests;

pub use config::{
    AgentConfig, AgentModelConfig, BudgetConfig, CacheConfig, CacheStrategy, ExecutionConfig,
    PromptConfig, SecurityConfig, SystemPromptMode,
};
pub(crate) use delegation::{DelegationRuntime, DelegationRuntimeConfig};
pub use events::{AgentEvent, AgentResult};
pub use executor::Agent;
pub use options::{AgentBuilder, DEFAULT_COMPACT_KEEP_MESSAGES};
pub use run_config::RunConfig;
pub use run_descriptor::{RunDescriptor, RuntimeEventRecorder};
pub use runtime::AgentRuntime;
pub use state::{AgentMetrics, AgentState, ToolCallRecord, ToolStats};
pub use task::{TaskInput, TaskOutput, TaskTool};
pub use task_output::{TaskOutputInput, TaskOutputResult, TaskOutputTool, TaskStatus};
pub use task_registry::{
    TaskAssistantMetadata, TaskExecutionSummary, TaskRegistry, TaskResultSnapshot,
};
