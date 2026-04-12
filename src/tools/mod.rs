//! Built-in tools for the agent.

mod ask_user;
#[cfg(feature = "coding-tools")]
mod bash;
mod builder;
mod context;
#[cfg(feature = "coding-tools")]
mod edit;
mod env;
#[cfg(feature = "coding-tools")]
mod glob;
mod graph_history;
#[cfg(feature = "coding-tools")]
mod grep;
#[cfg(feature = "coding-tools")]
mod kill;
pub mod mcp;
mod plan;
#[cfg(feature = "coding-tools")]
mod process;
#[cfg(feature = "coding-tools")]
mod read;
mod registry;
pub mod search;
mod surface;
#[cfg(test)]
mod testing;
mod todo;
mod traits;
#[cfg(feature = "coding-tools")]
mod write;

pub use crate::common::{is_tool_allowed, matches_tool_pattern};
pub use ask_user::{AskUserQuestionInput, AskUserQuestionItem, AskUserQuestionTool};
#[cfg(feature = "coding-tools")]
pub use bash::BashTool;
pub use builder::ToolRegistryBuilder;
pub use context::{ExecutionContext, ProgressBuilder, ProgressStatus};
pub(crate) use context::{PROGRESS_CHANNEL_CAPACITY, ProgressEvent};
#[cfg(feature = "coding-tools")]
pub use edit::EditTool;
pub use env::ToolExecutionEnv;
#[cfg(feature = "coding-tools")]
pub use glob::GlobTool;
pub use graph_history::GraphHistoryTool;
#[cfg(feature = "coding-tools")]
pub use grep::GrepTool;
#[cfg(feature = "coding-tools")]
pub use kill::KillShellTool;
pub use mcp::{McpToolWrapper, create_mcp_tools, create_mcp_tools_with_access};
pub use plan::PlanTool;
#[cfg(feature = "coding-tools")]
pub use process::{ProcessId, ProcessInfo, ProcessScheduler};
#[cfg(feature = "coding-tools")]
pub use read::ReadTool;
pub use registry::ToolRegistry;
pub use search::{PreparedTools, SearchMode, ToolSearchConfig, ToolSearchManager};
pub use surface::ToolSurface;
pub use todo::TodoWriteTool;
pub use traits::{SchemaTool, Tool, ValidationError, ValidationResult};
#[cfg(feature = "coding-tools")]
pub use write::WriteTool;

pub use crate::network_sandbox::{DomainCheck, NetworkSandbox};
pub use crate::types::tool::{MemoryOverflowStore, OverflowRef, OverflowStore, preview_of};
pub use crate::types::{ToolOutput, ToolResult};
