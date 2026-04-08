//! Prelude module for convenient imports.
//!
//! This module re-exports the most commonly used types and traits
//! for building branchforge applications.
//!
//! # Usage
//!
//! ```rust
//! use branchforge::prelude::*;
//! ```

// =============================================================================
// Agent runtime — primary user surface
// =============================================================================
pub use crate::Agent;
pub use crate::AgentBuilder;
pub use crate::AgentEvent;
pub use crate::AgentResult;
pub use crate::AgentRuntime;
pub use crate::Error;
pub use crate::ExecutionMode;
pub use crate::Result;
pub use crate::RunConfig;
pub use crate::agent::{DEFAULT_MAX_TOKENS, RequestMetadata};

// =============================================================================
// LLM call surface — IR-native trait + decorators
// =============================================================================
pub use crate::client::codec::ModelCodec;
pub use crate::client::preset::Preset;
pub use crate::client::provider_client::{ChunkStream, ProviderClient};
pub use crate::client::transport::ModelTransport;
pub use crate::client::{CircuitBrokenClient, FallingBackClient, LlmCall, RetryingClient};

// =============================================================================
// IR types — the canonical domain model
// =============================================================================
pub use crate::ir::{
    CacheControl, CacheMarker, ContentPart, FinishReason, Message, ModelRequest, ModelResponse,
    ModelSettings, ModelStreamChunk, ModelWarning, ProviderCapabilities, ProviderOptions, Role,
    Support, SystemBlock, SystemPrompt, ToolDefinition as IrToolDefinition, Usage,
};

// =============================================================================
// Authentication
// =============================================================================
pub use crate::Auth;
pub use crate::Credential;

// =============================================================================
// Tools
// =============================================================================
pub use crate::tools::{ExecutionContext, SchemaTool, Tool, ToolRegistry, ToolSurface};
pub use crate::types::ToolResult;

// =============================================================================
// Common patterns
// =============================================================================
pub use crate::common::{ContentSource, Index, IndexRegistry, Named, SourceType, ToolRestricted};

// =============================================================================
// Session
// =============================================================================
pub use crate::session::{Session, SessionConfig, SessionId};

// =============================================================================
// Context
// =============================================================================
pub use crate::ContextBuilder;
pub use crate::PromptOrchestrator;
pub use crate::StaticContext;

// =============================================================================
// Skills + Subagents
// =============================================================================
pub use crate::SubagentIndex;
pub use crate::skills::{SkillIndex, SkillResult, SkillRuntime};

// =============================================================================
// Hooks
// =============================================================================
pub use crate::Hook;
pub use crate::HookContext;
pub use crate::HookEvent;
pub use crate::HookManager;

// =============================================================================
// Authorization
// =============================================================================
pub use crate::ToolPolicy;

// =============================================================================
// Output style
// =============================================================================
pub use crate::OutputStyle;
