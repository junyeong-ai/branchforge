//! Tool executor contract types.
//!
//! These are the value types every local [`crate::tools::Tool`] uses to
//! communicate with the agent runtime: input, output, error, and the
//! local-runtime spec ([`ToolSpec`]). Provider-side built-in tool
//! configurations (`WebSearchTool`, `WebFetchTool`, …) live in
//! [`crate::agent::server_tools`] because they are an agent-config
//! concern, not a local executor concern.

mod definition;
mod error;
mod output;

pub use definition::{ToolSpec, estimate_tool_tokens};
pub use error::ToolError;
pub use output::{ToolInput, ToolOutput, ToolOutputBlock, ToolResult};
