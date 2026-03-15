//! Core types for the Branchforge SDK.

pub mod citations;
pub mod content;
pub mod document;
mod message;
mod response;
pub mod search;
mod tool;

pub use crate::models::context_window;
pub use citations::{
    CharLocationCitation, Citation, CitationsConfig, ContentBlockLocationCitation,
    PageLocationCitation, SearchResultLocationCitation,
};
pub use content::{
    ContentBlock, ImageSource, ServerToolError, ServerToolUseBlock, ThinkingBlock, ToolResultBlock,
    ToolResultContent, ToolResultContentBlock, ToolUseBlock, WebFetchResultItem,
    WebFetchToolResultBlock, WebFetchToolResultContent, WebSearchResultItem,
    WebSearchToolResultBlock, WebSearchToolResultContent,
};
pub use document::{DocumentBlock, DocumentContentBlock, DocumentSource};
pub use message::{CacheControl, CacheTtl, CacheType, Message, Role, SystemBlock, SystemPrompt};
pub use response::{
    ApiResponse, AuthorizationDenied, CompactResult, ContentDelta, MessageDeltaData,
    MessageStartData, ModelUsage, ServerToolUse, ServerToolUseUsage, StopReason, StreamError,
    StreamEvent, TokenUsage, Usage,
};
pub use search::{SearchResultBlock, SearchResultContentBlock};
pub use tool::{
    ServerTool, ToolDefinition, ToolError, ToolInput, ToolOutput, ToolOutputBlock, ToolResult,
    ToolSearchTool, UserLocation, WebFetchTool, WebSearchTool, estimate_tool_tokens,
};
