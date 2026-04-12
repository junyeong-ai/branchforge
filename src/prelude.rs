//! Prelude module for convenient imports.
//!
//! This module re-exports the most commonly used types and traits for
//! building BranchForge applications. It is organised by layer so you can
//! see at a glance which symbols are always available vs. which live
//! behind a feature gate.
//!
//! # Usage
//!
//! ```rust
//! use branchforge::prelude::*;
//! ```
//!
//! # Layering (`docs/architecture/layering.md`)
//!
//! - **Layer 1** symbols are always re-exported — they work in a pure-core
//!   build (`cargo build --no-default-features`) with no filesystem, shell,
//!   or cloud-provider assumptions. Pure API agents (chat bots, customer
//!   support, server-side automation) need nothing else.
//! - **Layer 2a (`local-fs`)** symbols are re-exported only when the
//!   `local-fs` feature is active. They add workspace-aware file access
//!   (`Read`/`Write`/`Edit`/`Glob`/`Grep`), the filesystem-walking memory
//!   loader, and the file-backed memory provider. Good fit for research,
//!   knowledge-management, and data-analysis agents.
//! - **Layer 2b (`coding-tools`)** symbols are re-exported only when the
//!   `coding-tools` feature is active. Shell execution, AST-level bash
//!   safety, and related coding-agent niceties. Transitively enables
//!   `local-fs`.
//!
//! A user who writes `use branchforge::prelude::*;` gets exactly the
//! surface matching their enabled features, with no surprises and no
//! hidden symbols behind narrow `#[cfg]` blocks scattered through their
//! own code.

// =============================================================================
// Layer 1 — always-on
// =============================================================================

// -- Agent runtime — primary user surface --
pub use crate::Agent;
pub use crate::AgentBuilder;
pub use crate::AgentCheckpoint;
pub use crate::AgentEvent;
pub use crate::AgentResult;
pub use crate::AgentRuntime;
pub use crate::Error;
pub use crate::ExecutionMode;
pub use crate::Result;
pub use crate::RunConfig;
pub use crate::agent::{DEFAULT_MAX_TOKENS, RequestMetadata};

// -- Streaming aggregator — canonical consumer for `AgentEvent`
//    streams returned by `Agent::execute_stream()`. UIs and CLIs
//    should consume via `apply()` / `drain()` instead of re-
//    implementing tool-call state tracking.
pub use crate::{StreamAggregator, ToolCallState, ToolCallStatus};

// -- Observability event bus and the typed payloads emitted by
//    the agent runtime. `subscribe_typed::<D>(cb)` + `emit_typed(d)`
//    replace the untyped JSON dispatch for the built-in event
//    kinds. Custom events keep the raw `subscribe` / `emit_simple`
//    path.
pub use crate::{
    BudgetAlertPayload, Event, EventBus, EventKind, EventPayload, StreamChunkKind,
    StreamChunkPayload, TokensConsumedPayload, ToolExecutedPayload, ToolProgressPayload,
};

// -- Mock LLM for deterministic agent testing. Surfaces the
//    scripted-conversation helpers (`then_text`, `then_tool_call`,
//    `then_stream_text`, `then_stream_tool_call`) without forcing
//    consumers to reach into `crate::client::mock`.
pub use crate::{MockLlmCall, MockResponse};

// -- LLM call surface — IR-native trait + decorators --
pub use crate::client::codec::ModelCodec;
pub use crate::client::preset::{ProfileRegistry, ProviderProfile};
pub use crate::client::provider_client::{ChunkStream, ProviderClient};
pub use crate::client::transport::ModelTransport;
pub use crate::client::{CircuitBrokenClient, FallingBackClient, LlmCall, RetryingClient};

// -- IR types — the canonical domain model --
pub use crate::ir::{
    CacheControl, CacheMarker, ContentPart, FinishReason, Message, ModelRequest, ModelResponse,
    ModelSettings, ModelStreamChunk, ModelWarning, ProviderCapabilities, ProviderOptions, Role,
    Support, SystemBlock, SystemPrompt, ToolDefinition as IrToolDefinition, Usage,
};

// -- Authentication --
pub use crate::Auth;
pub use crate::Credential;

// -- Tools --
pub use crate::tools::{ExecutionContext, SchemaTool, Tool, ToolRegistry, ToolSurface};
pub use crate::types::ToolResult;

// -- Common patterns & utilities --
pub use crate::Extensions;
pub use crate::Workspace;
pub use crate::common::{ContentSource, Index, IndexRegistry, Named, SourceType, ToolRestricted};

// -- Session --
pub use crate::session::{Session, SessionConfig, SessionId};

// -- Context & prompt composition --
pub use crate::ContextBuilder;
pub use crate::PromptOrchestrator;
pub use crate::StaticContext;
pub use crate::context::{
    EnvironmentFact, EnvironmentSource, MemoryContent, MemoryContextProvider, MemoryProvider,
};

// -- Skills + Subagents (metadata layer) --
pub use crate::SubagentIndex;
pub use crate::skills::{SkillIndex, SkillResult, SkillRuntime};
pub use crate::subagents::{builtin_subagents, general_purpose_subagent};

// -- Hooks --
pub use crate::Hook;
pub use crate::HookContext;
pub use crate::HookEvent;
pub use crate::HookRegistry;

// -- Authorization --
pub use crate::ToolPolicy;

// -- Output style --
pub use crate::OutputStyle;

// =============================================================================
// Layer 2a — local-fs (filesystem tools for local-machine general agents)
// =============================================================================

/// File-backed memory provider that walks a directory for CLAUDE.md and
/// related Claude Code convention files. See also the Layer 1
/// [`MemoryContextProvider`] for programmatic in-memory population.
#[cfg(feature = "local-fs")]
pub use crate::context::{FileMemoryProvider, MemoryLoader};

/// Builtin exploration and planning subagents that operate over a
/// filesystem tree without requiring shell execution. Useful for research,
/// knowledge-management, and data-analysis agents.
#[cfg(feature = "local-fs")]
pub use crate::subagents::{explore_subagent, plan_subagent};

// =============================================================================
// Layer 2b — coding-tools (shell execution + process management)
// =============================================================================

/// Builtin coding subagent that owns shell execution. Transitively
/// enables the filesystem subagents via `local-fs`.
#[cfg(feature = "coding-tools")]
pub use crate::subagents::bash_subagent;
