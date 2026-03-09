//! Prelude module for convenient imports.
//!
//! This module re-exports the most commonly used types and traits
//! for building Claude-powered applications.
//!
//! # Usage
//!
//! ```rust
//! use claude_agent::prelude::*;
//! ```

// Core types
pub use crate::Agent;
pub use crate::AgentBuilder;
pub use crate::AgentEvent;
pub use crate::AgentResult;
pub use crate::Error;
pub use crate::Result;

// Authentication
pub use crate::Auth;
pub use crate::Credential;

// Client
pub use crate::Client;
pub use crate::ClientBuilder;

// Common - Index pattern types
pub use crate::common::{ContentSource, Index, IndexRegistry, Named, SourceType, ToolRestricted};

// Tools
pub use crate::tools::{ExecutionContext, SchemaTool, Tool, ToolAccess, ToolRegistry};
pub use crate::types::ToolResult;

// Types
pub use crate::types::{ApiResponse, ContentBlock, Message, Role, StopReason, Usage};

// Session
pub use crate::session::{Session, SessionConfig, SessionId};

// Context
pub use crate::ContextBuilder;
pub use crate::PromptOrchestrator;
pub use crate::StaticContext;

// Skills
pub use crate::skills::{SkillIndex, SkillResult, SkillRuntime};

// Subagents
pub use crate::SubagentIndex;

// Hooks
pub use crate::Hook;
pub use crate::HookContext;
pub use crate::HookEvent;
pub use crate::HookManager;

// Output
pub use crate::OutputStyle;
