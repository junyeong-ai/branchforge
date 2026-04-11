//! Agent execution engine.

mod checkpoint;
mod common;
mod config;
mod contract;
mod delegation;
mod events;
mod execution;
mod executor;
pub mod model_config;
mod options;
pub mod recovery_executor;
pub mod recovery_recipes;
mod request;
pub mod run_config;
pub mod runtime;
pub mod server_tools;
mod state;
mod state_formatter;
mod streaming;
mod subagent_fsm;
mod task;
mod task_budget;
mod task_output;
mod task_registry;
mod task_registry_types;
pub mod types;

#[cfg(test)]
mod tests;

pub use checkpoint::AgentCheckpoint;
pub use config::{
    AgentConfig, AgentModelConfig, BudgetConfig, CacheConfig, CacheStrategy, ExecutionConfig,
    PromptConfig, SecurityConfig, SystemPromptMode,
};
pub use contract::{AgentContract, TypedAgentInvoker};
pub(crate) use delegation::{DelegationRuntime, DelegationRuntimeConfig};
pub use events::{AgentEvent, AgentResult};
pub use executor::Agent;
pub use model_config::{
    BetaConfig, BetaFeature, CloudProvider, DEFAULT_FAST_MODEL, DEFAULT_MODEL,
    DEFAULT_REASONING_MODEL, ModelConfig, ModelType, ProviderConfig,
};
pub use options::{AgentBuilder, DEFAULT_COMPACT_KEEP_MESSAGES};
pub use recovery_executor::{RecoveryExecutor, RecoveryOutcome};
pub use recovery_recipes::{
    RecipeDecision, RecipeRegistry, RecoveryAction, RecoveryDecisionInput, RecoveryRecipe,
    builtin_general_recipes,
};
pub use run_config::RunConfig;
pub use runtime::AgentRuntime;
pub use server_tools::{
    CitationsConfig, ServerTool, ToolSearchTool, UserLocation, WebFetchTool, WebSearchTool,
};
pub use state::{AgentMetrics, AgentState, ToolCallRecord, ToolStats};
pub use subagent_fsm::{SubagentState, SubagentTransitionError};
pub use task::{TaskInput, TaskOutput, TaskTool};
pub use task_budget::TaskBudget;
pub use task_output::{TaskOutputInput, TaskOutputResult, TaskOutputTool, TaskStatus};
pub use task_registry::{
    TaskAssistantMetadata, TaskExecutionSummary, TaskRegistry, TaskResultSnapshot,
};
pub use types::{DEFAULT_MAX_TOKENS, MIN_THINKING_BUDGET, RequestMetadata};
