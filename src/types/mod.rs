//! Domain types for the BranchForge SDK.
//!
//! Provider-neutral LLM protocol types live in [`crate::ir`]. This module
//! contains agent-domain types that aren't part of the wire protocol:
//! tool execution results, authorization records, server-tool aggregation,
//! per-model metrics, and compaction outcomes.

mod metrics;
pub mod provider;
mod server_tool;
pub mod tool;

pub use crate::models::context_window;
pub use metrics::ModelUsage;
pub use provider::UsageProvider;
pub use server_tool::ServerToolUse;
pub use tool::{
    ToolError, ToolInput, ToolOutput, ToolOutputBlock, ToolResult, ToolSpec, estimate_tool_tokens,
};
