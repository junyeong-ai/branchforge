//! Built-in tools for the agent.

mod access;
mod bash;
mod builder;
mod context;
mod edit;
mod env;
mod glob;
mod graph_history;
mod grep;
mod kill;
pub mod mcp;
mod plan;
mod process;
mod read;
mod registry;
pub mod search;
#[cfg(test)]
mod testing;
mod todo;
mod traits;
mod write;

pub use crate::common::{is_tool_allowed, matches_tool_pattern};
pub use access::ToolAccess;
pub use bash::BashTool;
pub use builder::ToolRegistryBuilder;
pub use context::ExecutionContext;
pub use edit::EditTool;
pub use env::ToolExecutionEnv;
pub use glob::GlobTool;
pub use graph_history::GraphHistoryTool;
pub use grep::GrepTool;
pub use kill::KillShellTool;
pub use mcp::{McpToolWrapper, create_mcp_tools};
pub use plan::PlanTool;
pub use process::{ProcessId, ProcessInfo, ProcessManager};
pub use read::ReadTool;
pub use registry::ToolRegistry;
pub use search::{PreparedTools, SearchMode, ToolSearchConfig, ToolSearchManager};
pub use todo::TodoWriteTool;
pub use traits::{SchemaTool, Tool};
pub use write::WriteTool;

pub use crate::security::sandbox::{DomainCheck, NetworkSandbox};
pub use crate::types::{ToolOutput, ToolResult, ToolSearchTool, WebFetchTool, WebSearchTool};
