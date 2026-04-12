//! # branchforge
//!
//! Rust runtime for building stateful coding agents.
//!
//! This crate provides a production-ready, provider-agnostic runtime for long-lived
//! agent workflows with durable session graphs, safe tool execution, and explicit
//! provider capabilities.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use branchforge::query;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), branchforge::Error> {
//!     let response = query("What is 2 + 2?").await?;
//!     println!("{}", response);
//!     Ok(())
//! }
//! ```
//!
//! ## Full Agent Example
//!
//! ```rust,no_run
//! use branchforge::{Agent, AgentEvent, ToolSurface};
//! use futures::StreamExt;
//! use std::pin::pin;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), branchforge::Error> {
//!     // `ToolSurface::coding()` selects the full coding tool surface
//!     // (filesystem + shell). For a research or knowledge agent use
//!     // `ToolSurface::local_fs()`; for a pure-API agent use
//!     // `ToolSurface::core()`.
//!     let agent = Agent::builder()
//!         .model("claude-sonnet-4-5")
//!         .tools(ToolSurface::coding())
//!         .working_dir("./project")
//!         .build()
//!         .await?;
//!
//!     let stream = agent.execute_stream("Fix the bug").await?;
//!     let mut stream = pin!(stream);
//!     while let Some(event) = stream.next().await {
//!         match event? {
//!             AgentEvent::Text { delta } => print!("{}", delta),
//!             AgentEvent::Complete(result) => {
//!                 println!("Done: {} tokens", result.total_tokens());
//!             }
//!             _ => {}
//!         }
//!     }
//!     Ok(())
//! }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod agent;
pub mod auth;
pub mod authorization;
pub mod budget;
pub mod client;
pub mod common;
pub mod config;
pub mod context;
pub mod context_scope;
pub mod decision;
pub mod events;
pub mod graph;
pub mod hooks;
pub mod ir;
pub mod mcp;
pub mod models;
pub mod network_sandbox;
pub mod observability;
pub mod orchestration;
pub mod output_style;
#[cfg(feature = "plugins")]
pub mod plugins;
pub mod prelude;
pub mod prompts;
#[cfg(feature = "scheduling")]
pub mod scheduling;
#[cfg(feature = "local-fs")]
pub mod security;
pub mod session;
pub mod skills;
pub mod subagents;
pub mod tokens;
pub mod tools;
pub mod types;
pub mod workspace;

// =========================================================================
// Core API re-exports (user-facing types)
// =========================================================================

pub use agent::{
    Agent, AgentBuilder, AgentCheckpoint, AgentConfig, AgentEvent, AgentEventSink, AgentInitTool,
    AgentResult, AgentRuntime, ChannelSink, DroppingSink, InitialState, NdjsonSink, NoopSink,
    RunConfig, SinkError, SseSink, StreamAggregator, StreamUsage, ToolCallState, ToolCallStatus,
    ToolProgressEntry, drive_stream_into_sink, event_is_critical,
};
pub use auth::{Auth, Credential};
pub use auth::{CredentialKind, CredentialRecord};
pub use authorization::{
    ElicitationRequest, ElicitationResponse, ExecutionMode, HumanInteractionError,
    HumanInteractionHandler, HumanInteractionResult, Question, QuestionRequest, QuestionResponse,
    ToolApprovalRequest, ToolApprovalResponse, ToolPolicy,
};
pub use events::{
    BranchForkedPayload, BudgetAlertPayload, CacheBreakObservedPayload, CheckpointCreatedPayload,
    Event, EventBus, EventKind, EventPayload, SessionCompactedPayload, StreamChunkKind,
    StreamChunkPayload, TokensConsumedPayload, ToolExecutedPayload, ToolProgressPayload,
};

// Provider client stack — the only LLM call surface. There is no longer
// a monolithic `Client` type; applications either compose a
// `ProviderClient` directly or look up a named [`ProviderProfile`]
// from the [`ProfileRegistry`] and let it resolve the
// (codec, transport, credential) triple.
pub use client::codec::{
    AnthropicMessagesCodec, BedrockConverseCodec, EncodedRequest, EndpointShape,
    GeminiGenerateCodec, InvocationMode, ModelCodec, OpenAiChatCodec, OpenAiResponsesCodec,
};
pub use client::llm_call::{CircuitBrokenClient, FallingBackClient, LlmCall, RetryingClient};
pub use client::mock::{MockLlmCall, MockResponse};
pub use client::preset::{
    CredentialHint, ProfileRegistry, ProviderProfile, from_env as profile_from_env,
};
pub use client::provider_client::{ChunkStream, ProviderClient};
#[cfg(feature = "aws")]
pub use client::transport::BedrockTransport;
#[cfg(feature = "azure")]
pub use client::transport::FoundryTransport;
#[cfg(feature = "gcp")]
pub use client::transport::VertexTransport;
pub use client::transport::{DirectAuth, DirectTransport, Endpoint, ModelTransport};
pub use context::PromptFrame;
pub use context_scope::{ContextScope, SharedContextScope};
pub use graph::{
    Bookmark, BookmarkId, Branch, BranchExport, BranchId, Checkpoint, ExportBookmark, ExportNode,
    GraphError, GraphEvent, GraphEventBody, GraphMaterializer, GraphNode, NodeId, NodeKind,
    ReplayInput, SessionGraph,
};
pub use tools::{
    ExecutionContext, ProgressBuilder, ProgressStatus, SchemaTool, Tool, ToolRegistry, ToolSurface,
};
// IR types are the canonical user-facing domain model. Re-export at the
// crate root for ergonomic access.
pub use ir::{
    CacheControl, CacheMarker, ContentPart, FinishReason, Message, ModelRequest, ModelResponse,
    ModelSettings, ModelStreamChunk, ModelWarning, ProviderCapabilities, ProviderOptions, Role,
    Support, SystemBlock, SystemPrompt, ToolDefinition, Usage,
};

// Tool execution types live in `types/tool/` (provider-neutral by design)
// and are re-exported here for convenience.
pub use types::{ToolError, ToolOutput, ToolResult};

// =========================================================================
// Commonly used configuration re-exports
// =========================================================================

pub use agent::{
    AgentMetrics, AgentModelConfig, AgentState, BudgetConfig, CacheConfig, CacheStrategy,
    ExecutionConfig, PromptConfig, SecurityConfig, SystemPromptMode, ToolStats,
};
pub use auth::{CredentialProvider, OAuthConfig};
pub use budget::report::{CostSummary, ModelCostEntry};
pub use client::{FallbackConfig, RetryPolicy};
pub use common::circuit::{CircuitBreaker, CircuitConfig, CircuitState};
pub use common::{
    ContentSource, Extensions, Index, IndexRegistry, Named, SourceType, ToolRestricted,
};
#[cfg(feature = "local-fs")]
pub use context::MemoryLoader;
pub use context::{ContextBuilder, MemoryProvider, PromptOrchestrator, RuleIndex, StaticContext};
pub use hooks::{CommandHook, Hook, HookContext, HookEvent, HookOutput, HookRegistry};
pub use output_style::OutputStyle;
pub use session::{
    InMemoryStore, MemoryEntry, MemoryStore, ScopedSessionManager, Session, SessionConfig,
    SessionId, SessionManager, SessionMessage, SessionState, ToolState,
};
pub use skills::{SkillIndex, SkillResult, SkillRuntime};
pub use subagents::{SubagentIndex, builtin_subagents};
pub use workspace::Workspace;

#[cfg(feature = "cli-auth")]
pub use auth::ClaudeCliProvider;
#[cfg(feature = "file-resources")]
pub use output_style::OutputStyleLoader;
pub use output_style::SystemPromptGenerator;
#[cfg(feature = "plugins")]
pub use plugins::{PluginDescriptor, PluginDiscovery, PluginError, PluginLoader, PluginManifest};
#[cfg(feature = "file-resources")]
pub use subagents::{SubagentFrontmatter, SubagentIndexLoader};

/// Error type for branchforge operations.
///
/// All errors include actionable context to help diagnose and resolve issues.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Authentication failed.
    #[error("Authentication failed: {message}")]
    Authentication { message: String },

    /// Network connectivity or request failed.
    #[error("Network request failed: {0}")]
    Network(#[from] reqwest::Error),

    /// JSON serialization or deserialization failed.
    #[error("JSON parsing failed: {0}")]
    Json(#[from] serde_json::Error),

    /// Failed to parse response or configuration.
    #[error("Parse error: {0}")]
    Parse(String),

    /// Tool execution failed.
    #[error("Tool execution failed: {0}")]
    Tool(#[from] types::ToolError),

    /// Invalid or missing configuration.
    #[error("Configuration error: {0}")]
    Config(String),

    /// File system operation failed.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// API rate limit exceeded.
    #[error("Rate limit exceeded{}", match retry_after {
        Some(d) => format!(", retry in {:.0}s", d.as_secs_f64()),
        None => String::new(),
    })]
    RateLimit {
        retry_after: Option<std::time::Duration>,
    },

    /// Context window would be exceeded by request.
    #[error("Context window exceeded: {estimated} tokens > {limit} limit (overage: {overage})")]
    ContextWindowExceeded {
        estimated: u64,
        limit: u64,
        overage: u64,
    },

    /// Operation exceeded timeout.
    #[error("Operation timed out after {:.1}s", .0.as_secs_f64())]
    Timeout(std::time::Duration),

    /// Request parameters are invalid.
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    /// Streaming response error.
    #[error("Stream error: {0}")]
    Stream(String),

    /// Required environment variable missing or invalid.
    #[error("Environment variable error: {0}")]
    Env(#[from] std::env::VarError),

    /// Operation not supported by the current provider.
    #[error("{operation} is not supported by {provider}")]
    NotSupported {
        provider: &'static str,
        operation: &'static str,
    },

    /// Operation blocked by permission policy.
    #[error("Authorization denied: {0}")]
    Authorization(String),

    /// Budget limit exceeded.
    #[error("Budget exceeded: ${used} used (limit: ${limit})")]
    BudgetExceeded {
        used: rust_decimal::Decimal,
        limit: rust_decimal::Decimal,
    },

    /// Model is temporarily overloaded.
    #[error("Model {model} is overloaded, try again later")]
    ModelOverloaded { model: String },

    /// Session operation failed.
    #[error("Session error: {0}")]
    Session(#[from] session::SessionError),

    /// MCP server communication failed.
    #[error("MCP error: {0}")]
    Mcp(mcp::McpError),

    /// System resource limit reached (memory, processes, etc.)
    #[error("Resource exhausted: {0}")]
    ResourceExhausted(String),

    /// Hook execution failed (blockable hooks only).
    #[error("Hook '{hook}' failed: {reason}")]
    HookFailed { hook: String, reason: String },

    /// Hook timed out (blockable hooks only).
    #[error("Hook '{hook}' timed out after {duration_secs}s")]
    HookTimeout { hook: String, duration_secs: u64 },

    /// Circuit breaker is open, requests are being rejected.
    #[error("Circuit breaker is open")]
    CircuitOpen,

    /// Plugin system error.
    #[cfg(feature = "plugins")]
    #[error("Plugin error: {0}")]
    Plugin(#[from] plugins::PluginError),

    /// Classified provider error from the new codec/transport stack.
    /// Carries an actionable hint where the failure mode is well-known.
    #[error("{provider} {kind:?}: {message}{}", hint.map(|h| format!(" — hint: {h}")).unwrap_or_default())]
    Provider {
        provider: &'static str,
        kind: error::ProviderErrorKind,
        message: String,
        hint: Option<&'static str>,
        retryable: bool,
        status: Option<u16>,
        /// Phase D E-2: rate-limit accounting snapshot parsed from
        /// the failing response's headers, when the transport publishes
        /// them. Recovery recipes use `seconds_until_reset` to pick a
        /// data-driven backoff instead of blind exponential retry.
        /// Absent for transports that don't publish headers (Vertex,
        /// Bedrock, Foundry) and for error paths where the snapshot
        /// could not be parsed.
        /// Boxed so `Error` stays small — clippy's `result_large_err`
        /// lint fires above ~128 bytes, and `RateLimitSnapshot` has
        /// six `Option<_>` fields. `Option<Box<_>>` keeps the variant
        /// a single pointer wide when no snapshot is attached.
        rate_limit: Option<Box<ir::RateLimitSnapshot>>,
    },

    /// A `(codec, transport)` composition is invalid (pin violation,
    /// unsupported codec on the transport, …). Raised at `ProviderClient`
    /// construction time.
    #[error("invalid codec/transport composition: {codec} × {transport}: {reason}")]
    InvalidComposition {
        codec: &'static str,
        transport: &'static str,
        reason: &'static str,
    },

    /// A provider response violated the declared `ResponseFormat::JsonSchema`
    /// at runtime. Raised by `ProviderClient::send` when `spec.strict` is
    /// true and the decoded body fails post-decode validation — either the
    /// body is not JSON at all or the JSON does not conform to the schema.
    ///
    /// `pointer` is an RFC 6901 JSON pointer into the response document
    /// (empty string for the root). When the body was unparseable, the
    /// pointer is empty and `reason` carries the serde_json parser message.
    #[error("structured output invalid at {}: {reason}", if pointer.is_empty() { "root" } else { pointer.as_str() })]
    StructuredOutputInvalid { pointer: String, reason: String },

    /// Retry budget for [`Self::StructuredOutputInvalid`] exhausted in
    /// a single user turn. Raised by the agent loop after the model
    /// has failed schema validation `attempts` times without
    /// producing a conforming response. This is the terminal error
    /// for the retry-on-invalid-structured-output path: at this
    /// point additional retries are not going to succeed and the
    /// caller needs to either relax the schema, switch model, or
    /// inspect the last failing response.
    #[error(
        "structured output validation failed {attempts} times in a single turn; last error: {last_reason}"
    )]
    StructuredOutputExhausted { attempts: u32, last_reason: String },
}

/// Error helpers for the new codec/transport stack.
pub mod error {
    /// Classified kind of a [`super::Error::Provider`] failure.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ProviderErrorKind {
        /// Authentication or authorization failure (401/403, expired token).
        Auth,
        /// Quota project missing or quota exhausted (Vertex
        /// `x-goog-user-project`, OpenAI org quotas).
        Quota,
        /// 429 — rate limited; should be retried with backoff.
        RateLimit,
        /// 4xx other than auth/rate-limit.
        BadRequest,
        /// 5xx server error.
        Server,
        /// Network / TLS / DNS failure.
        Network,
        /// Request was cancelled before completion.
        Cancelled,
        /// Output blocked by a content filter / safety policy.
        ContentFilter,
        /// 413 — request payload too large for the model's context window.
        PayloadTooLarge,
    }
}

/// Coarse-grained failure classification for unified error handling,
/// recovery policies, and observability vocabularies.
///
/// `FailureCategory` is the single source of truth mapped from every
/// [`Error`] variant via [`Error::category`]. It exists for three
/// distinct consumers that all need *the same* vocabulary:
///
/// 1. **OpenTelemetry `error.category` attribute** — spans and metrics
///    that record a failure embed the variant's
///    [`as_str`][FailureCategory::as_str] form so dashboards can group
///    by cause without inspecting the full error message.
/// 2. **Recovery Recipe selection** (Phase 2+ follow-up) — a recipe
///    engine can match on category to decide whether to retry, escalate,
///    or compact context.
/// 3. **User-facing error handling** — library consumers can branch on
///    a small finite set of categories without exhaustively matching
///    every `Error` variant.
///
/// This replaces the older 6-variant `ErrorCategory` which was too
/// coarse to drive recovery decisions (e.g. it could not distinguish
/// `RateLimit` from `Quota` from `Network`, all three of which need
/// different retry strategies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FailureCategory {
    /// 401/403 authentication or expired OAuth token.
    Auth,
    /// Authorization denied by policy (HITL deny, deny rule, hook
    /// returning `{decision: "block"}`).
    PolicyDenied,
    /// Network / TLS / DNS failure before the request reached the
    /// provider.
    Transport,
    /// 429 rate-limited.
    RateLimit,
    /// Billing quota / account-level limit (not per-request rate).
    Quota,
    /// Context window exceeded (pre-flight or 413 from provider).
    ContextWindow,
    /// Budget cap exceeded by an in-flight or future request.
    Budget,
    /// Output blocked by content / safety filter.
    ContentPolicy,
    /// Response did not conform to the requested JSON Schema (NEW-3
    /// response validation surface).
    SchemaMismatch,
    /// 4xx request shape rejected by provider (invalid model, bad
    /// argument, …).
    BadRequest,
    /// 5xx provider-side server error.
    ProviderServer,
    /// Request was cancelled before completion (client abort, parent
    /// task drop).
    Cancelled,
    /// Per-call deadline elapsed.
    Timeout,
    /// Tool execution raised an error (internal to the tool, not a
    /// framework-level issue).
    ToolRuntime,
    /// Pre- / post-tool-use hook failed, timed out, or returned a
    /// non-recoverable error.
    HookFailure,
    /// MCP handshake / tool discovery failure (stdio or HTTP transport
    /// could not be brought up).
    McpHandshake,
    /// MCP tool invocation failed after a successful handshake.
    McpInvocation,
    /// Session persistence or checkpoint / fork operation failed.
    Persistence,
    /// Configuration, parsing, or environment-variable lookup error.
    Config,
    /// Circuit breaker is open; upstream is being protected from load.
    CircuitOpen,
    /// Operating-system resource exhausted (memory, file descriptors,
    /// child processes).
    Resource,
    /// Catch-all for IO/JSON/panics and other unexpected internal
    /// states. Observability should alert on elevated rates.
    Internal,
}

impl FailureCategory {
    /// Stable lowercase string form for OTel `error.category` attribute
    /// and log keys. These strings are part of the public contract and
    /// **must not change** without a major version bump — dashboards,
    /// alert rules, and recovery recipes depend on them.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::PolicyDenied => "policy_denied",
            Self::Transport => "transport",
            Self::RateLimit => "rate_limit",
            Self::Quota => "quota",
            Self::ContextWindow => "context_window",
            Self::Budget => "budget",
            Self::ContentPolicy => "content_policy",
            Self::SchemaMismatch => "schema_mismatch",
            Self::BadRequest => "bad_request",
            Self::ProviderServer => "provider_server",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::ToolRuntime => "tool_runtime",
            Self::HookFailure => "hook_failure",
            Self::McpHandshake => "mcp_handshake",
            Self::McpInvocation => "mcp_invocation",
            Self::Persistence => "persistence",
            Self::Config => "config",
            Self::CircuitOpen => "circuit_open",
            Self::Resource => "resource",
            Self::Internal => "internal",
        }
    }

    /// Whether a recovery recipe can reasonably retry the same request
    /// against the same provider without mutating the input.
    ///
    /// This is a *category*-level hint, not an absolute guarantee; an
    /// individual [`Error::Provider`] carries its own `retryable` flag
    /// that may override this default for vendor-specific edge cases.
    pub const fn is_transient(self) -> bool {
        matches!(
            self,
            Self::Transport
                | Self::RateLimit
                | Self::ProviderServer
                | Self::Timeout
                | Self::CircuitOpen
        )
    }

    /// Whether the failure was caused by a policy decision (user
    /// authorization, hook denial, content filter, bad request shape).
    /// These are *not* retryable — the same input will keep failing
    /// until the user or upstream policy changes.
    pub const fn is_user_actionable(self) -> bool {
        matches!(
            self,
            Self::Auth
                | Self::PolicyDenied
                | Self::BadRequest
                | Self::ContentPolicy
                | Self::Config
                | Self::Quota
                | Self::ContextWindow
                | Self::Budget
                | Self::SchemaMismatch
        )
    }
}

impl std::fmt::Display for FailureCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Error {
    pub fn auth(message: impl Into<String>) -> Self {
        Error::Authentication {
            message: message.into(),
        }
    }

    /// Classify this error into a coarse-grained [`FailureCategory`] for
    /// observability, recovery, and user-facing dispatch.
    ///
    /// The match is intentionally exhaustive — adding a new [`Error`]
    /// variant requires updating this function, which is the whole
    /// point: every error must map somewhere in the vocabulary.
    pub fn category(&self) -> FailureCategory {
        match self {
            // -- Authentication / authorization --
            Error::Authentication { .. } => FailureCategory::Auth,
            Error::Authorization(_) => FailureCategory::PolicyDenied,
            Error::HookFailed { .. } | Error::HookTimeout { .. } => FailureCategory::HookFailure,

            // -- Configuration & request shape --
            Error::Config(_)
            | Error::Parse(_)
            | Error::Env(_)
            | Error::InvalidRequest(_)
            | Error::InvalidComposition { .. }
            | Error::StructuredOutputInvalid { .. }
            | Error::StructuredOutputExhausted { .. } => FailureCategory::Config,
            Error::NotSupported { .. } => FailureCategory::BadRequest,

            // -- Transport / rate limiting / circuit --
            Error::Network(_) => FailureCategory::Transport,
            Error::RateLimit { .. } => FailureCategory::RateLimit,
            Error::ModelOverloaded { .. } => FailureCategory::ProviderServer,
            Error::CircuitOpen => FailureCategory::CircuitOpen,

            // -- Resource & budget limits --
            Error::BudgetExceeded { .. } => FailureCategory::Budget,
            Error::ContextWindowExceeded { .. } => FailureCategory::ContextWindow,
            Error::Timeout(_) => FailureCategory::Timeout,
            Error::ResourceExhausted(_) => FailureCategory::Resource,

            // -- Stateful subsystems --
            Error::Session(_) => FailureCategory::Persistence,
            Error::Mcp(_) => FailureCategory::McpInvocation,
            Error::Stream(_) => FailureCategory::Transport,
            Error::Tool(_) => FailureCategory::ToolRuntime,

            // -- Internal / unexpected --
            Error::Io(_) | Error::Json(_) => FailureCategory::Internal,

            // -- Provider-side classified errors --
            Error::Provider { kind, .. } => match kind {
                error::ProviderErrorKind::Auth => FailureCategory::Auth,
                error::ProviderErrorKind::Quota => FailureCategory::Quota,
                error::ProviderErrorKind::RateLimit => FailureCategory::RateLimit,
                error::ProviderErrorKind::Server => FailureCategory::ProviderServer,
                error::ProviderErrorKind::Network => FailureCategory::Transport,
                error::ProviderErrorKind::BadRequest => FailureCategory::BadRequest,
                error::ProviderErrorKind::ContentFilter => FailureCategory::ContentPolicy,
                error::ProviderErrorKind::Cancelled => FailureCategory::Cancelled,
                error::ProviderErrorKind::PayloadTooLarge => FailureCategory::ContextWindow,
            },

            #[cfg(feature = "plugins")]
            Error::Plugin(_) => FailureCategory::Config,
        }
    }

    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Error::Authentication { .. })
    }

    pub fn is_overloaded(&self) -> bool {
        matches!(self, Error::ModelOverloaded { .. })
    }

    pub fn status_code(&self) -> Option<u16> {
        match self {
            Error::Provider { status, .. } => *status,
            _ => None,
        }
    }

    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Error::RateLimit { retry_after } => *retry_after,
            _ => None,
        }
    }

    /// Phase D E-2: rate-limit snapshot carried on an
    /// [`Error::Provider`] failure, if the transport published it.
    /// Used by [`crate::agent::recovery_recipes::RateLimitBackoffRecipe`]
    /// to pick a data-driven retry delay from
    /// `seconds_until_reset` instead of falling through to
    /// exponential backoff.
    pub fn rate_limit_snapshot(&self) -> Option<&ir::RateLimitSnapshot> {
        match self {
            Error::Provider {
                rate_limit: Some(snap),
                ..
            } => Some(snap.as_ref()),
            _ => None,
        }
    }

    /// Whether this error is transient and the same request may succeed on retry.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::RateLimit { .. }
                | Error::ModelOverloaded { .. }
                | Error::Timeout(_)
                | Error::Network(_)
                | Error::CircuitOpen
        ) || matches!(
            self,
            Error::Provider {
                retryable: true,
                ..
            }
        )
    }
}

impl From<config::ConfigError> for Error {
    fn from(err: config::ConfigError) -> Self {
        match err {
            config::ConfigError::NotFound { key } => {
                Error::Config(format!("Key not found: {}", key))
            }
            config::ConfigError::InvalidValue { key, message } => {
                Error::Config(format!("Invalid value for {}: {}", key, message))
            }
            config::ConfigError::Serialization(e) => Error::Json(e),
            config::ConfigError::Io(e) => Error::Io(e),
            config::ConfigError::Env(e) => Error::Env(e),
            config::ConfigError::Provider { message } => Error::Config(message),
            config::ConfigError::ValidationErrors(errors) => Error::Config(errors.to_string()),
        }
    }
}

impl From<context::ContextError> for Error {
    fn from(err: context::ContextError) -> Self {
        match err {
            context::ContextError::Source { message } => Error::Config(message),
            context::ContextError::TokenBudgetExceeded { current, limit } => {
                Error::ContextWindowExceeded {
                    estimated: current,
                    limit,
                    overage: current.saturating_sub(limit),
                }
            }
            context::ContextError::SkillNotFound { name } => {
                Error::Config(format!("Skill not found: {}", name))
            }
            context::ContextError::RuleNotFound { name } => {
                Error::Config(format!("Rule not found: {}", name))
            }
            context::ContextError::Parse { message } => Error::Parse(message),
            context::ContextError::Io(e) => Error::Io(e),
        }
    }
}

// From<SessionError> is auto-derived via #[from] on Error::Session.

impl From<graph::GraphError> for Error {
    fn from(err: graph::GraphError) -> Self {
        Error::Config(err.to_string())
    }
}

#[cfg(feature = "local-fs")]
impl From<security::SecurityError> for Error {
    fn from(err: security::SecurityError) -> Self {
        match err {
            security::SecurityError::Io(e) => Error::Io(e),
            security::SecurityError::ResourceLimit(msg) => Error::ResourceExhausted(msg),
            security::SecurityError::BashBlocked(msg) => Error::Authorization(msg),
            security::SecurityError::DeniedPath(path) => {
                Error::Authorization(format!("Denied path: {}", path.display()))
            }
            security::SecurityError::PathEscape(path) => {
                Error::Authorization(format!("Path escapes sandbox: {}", path.display()))
            }
            security::SecurityError::NotWithinSandbox(path) => {
                Error::Authorization(format!("Path not within sandbox: {}", path.display()))
            }
            security::SecurityError::InvalidPath(msg) => Error::Config(msg),
            security::SecurityError::AbsoluteSymlink(path) => Error::Authorization(format!(
                "Absolute symlink outside sandbox: {}",
                path.display()
            )),
            security::SecurityError::SymlinkDepthExceeded { path, max } => Error::Authorization(
                format!("Symlink depth exceeded (max {}): {}", max, path.display()),
            ),
        }
    }
}

#[cfg(feature = "local-fs")]
impl From<security::sandbox::SandboxError> for Error {
    fn from(err: security::sandbox::SandboxError) -> Self {
        match err {
            security::sandbox::SandboxError::Io(e) => Error::Io(e),
            security::sandbox::SandboxError::NotSupported => {
                Error::Config("Sandbox not supported on this platform".into())
            }
            security::sandbox::SandboxError::NotAvailable(msg) => {
                Error::Config(format!("Sandbox not available: {}", msg))
            }
            security::sandbox::SandboxError::Creation(msg) => {
                Error::Config(format!("Sandbox creation failed: {}", msg))
            }
            security::sandbox::SandboxError::RuleApplication(msg) => {
                Error::Config(format!("Sandbox rule application failed: {}", msg))
            }
            security::sandbox::SandboxError::PathNotAccessible(path) => {
                Error::Authorization(format!("Sandbox path not accessible: {}", path.display()))
            }
            security::sandbox::SandboxError::InvalidConfig(msg) => {
                Error::Config(format!("Invalid sandbox config: {}", msg))
            }
        }
    }
}

impl From<mcp::McpError> for Error {
    fn from(err: mcp::McpError) -> Self {
        match err {
            mcp::McpError::Io(e) => Error::Io(e),
            mcp::McpError::Json(e) => Error::Json(e),
            other => Error::Mcp(other),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Simple one-shot query helper.
///
/// Resolves a [`ProviderProfile`] from `BRANCHFORGE_PROVIDER` (and
/// the profile's declared credential source), sends a single user
/// message via [`ProviderClient`], and returns the joined text
/// content. The model is taken from `BRANCHFORGE_MODEL` if set.
pub async fn query(prompt: &str) -> Result<String> {
    let pc = client::preset::from_env().await?;
    let model = std::env::var("BRANCHFORGE_MODEL").ok();
    query_with_provider(&pc, model.as_deref(), prompt).await
}

/// Query with a specific model id, resolving the profile from env vars.
pub async fn query_with_model(model: &str, prompt: &str) -> Result<String> {
    let pc = client::preset::from_env().await?;
    query_with_provider(&pc, Some(model), prompt).await
}

async fn query_with_provider(
    pc: &client::ProviderClient,
    model: Option<&str>,
    prompt: &str,
) -> Result<String> {
    let model = model.unwrap_or(crate::agent::DEFAULT_MODEL);
    let req = ir::ModelRequest::new(model, vec![ir::Message::user(prompt)]);
    let resp = pc.send(&req).await?;
    Ok(resp.text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = Error::Provider {
            provider: "anthropic",
            kind: error::ProviderErrorKind::Auth,
            message: "Invalid API key".to_string(),
            hint: None,
            retryable: false,
            status: Some(401),
            rate_limit: None,
        };
        assert!(err.to_string().contains("Invalid API key"));
    }

    #[test]
    fn test_failure_category_maps_each_error_variant() {
        // Representative sample covering each major FailureCategory
        // discriminant. The full exhaustive match lives in
        // `Error::category` so any newly added variant forces an update
        // there; this test guards that the observable classification
        // stays stable across refactors.
        assert_eq!(Error::auth("bad").category(), FailureCategory::Auth);
        assert_eq!(
            Error::Authorization("denied".into()).category(),
            FailureCategory::PolicyDenied
        );
        assert_eq!(
            Error::RateLimit { retry_after: None }.category(),
            FailureCategory::RateLimit
        );
        assert_eq!(
            Error::ModelOverloaded { model: "x".into() }.category(),
            FailureCategory::ProviderServer
        );
        assert_eq!(
            Error::Timeout(std::time::Duration::from_secs(1)).category(),
            FailureCategory::Timeout
        );
        assert_eq!(Error::CircuitOpen.category(), FailureCategory::CircuitOpen);
        assert_eq!(
            Error::BudgetExceeded {
                used: rust_decimal::Decimal::ONE,
                limit: rust_decimal::Decimal::ONE
            }
            .category(),
            FailureCategory::Budget
        );
        assert_eq!(
            Error::ContextWindowExceeded {
                estimated: 10,
                limit: 5,
                overage: 5
            }
            .category(),
            FailureCategory::ContextWindow
        );
        assert_eq!(
            Error::ResourceExhausted("oom".into()).category(),
            FailureCategory::Resource
        );
        assert_eq!(
            Error::HookFailed {
                hook: "pre".into(),
                reason: "bad".into()
            }
            .category(),
            FailureCategory::HookFailure
        );
        assert_eq!(
            Error::Config("missing key".into()).category(),
            FailureCategory::Config
        );
        assert_eq!(
            Error::NotSupported {
                provider: "openai",
                operation: "cache_control"
            }
            .category(),
            FailureCategory::BadRequest
        );
    }

    #[test]
    fn test_failure_category_provider_kinds() {
        let cases = [
            (error::ProviderErrorKind::Auth, FailureCategory::Auth),
            (error::ProviderErrorKind::Quota, FailureCategory::Quota),
            (
                error::ProviderErrorKind::RateLimit,
                FailureCategory::RateLimit,
            ),
            (
                error::ProviderErrorKind::Server,
                FailureCategory::ProviderServer,
            ),
            (
                error::ProviderErrorKind::Network,
                FailureCategory::Transport,
            ),
            (
                error::ProviderErrorKind::BadRequest,
                FailureCategory::BadRequest,
            ),
            (
                error::ProviderErrorKind::ContentFilter,
                FailureCategory::ContentPolicy,
            ),
            (
                error::ProviderErrorKind::Cancelled,
                FailureCategory::Cancelled,
            ),
            (
                error::ProviderErrorKind::PayloadTooLarge,
                FailureCategory::ContextWindow,
            ),
        ];
        for (kind, expected) in cases {
            let err = Error::Provider {
                provider: "test",
                kind,
                message: "case".into(),
                hint: None,
                retryable: false,
                status: None,
                rate_limit: None,
            };
            assert_eq!(
                err.category(),
                expected,
                "provider kind {kind:?} should map to {expected:?}"
            );
        }
    }

    #[test]
    fn test_failure_category_transient_hints() {
        // Categories that a recovery recipe may treat as "retry after
        // backoff".
        assert!(FailureCategory::Transport.is_transient());
        assert!(FailureCategory::RateLimit.is_transient());
        assert!(FailureCategory::ProviderServer.is_transient());
        assert!(FailureCategory::Timeout.is_transient());
        assert!(FailureCategory::CircuitOpen.is_transient());

        // Categories that are never retryable without user action.
        assert!(!FailureCategory::Auth.is_transient());
        assert!(!FailureCategory::BadRequest.is_transient());
        assert!(!FailureCategory::ContentPolicy.is_transient());
        assert!(!FailureCategory::Budget.is_transient());
        assert!(!FailureCategory::ContextWindow.is_transient());
    }

    #[test]
    fn test_failure_category_user_actionable() {
        // "User-actionable" = the user (or upstream policy) needs to do
        // something; no retry will fix it.
        assert!(FailureCategory::Auth.is_user_actionable());
        assert!(FailureCategory::PolicyDenied.is_user_actionable());
        assert!(FailureCategory::Budget.is_user_actionable());
        assert!(FailureCategory::ContextWindow.is_user_actionable());
        assert!(FailureCategory::SchemaMismatch.is_user_actionable());

        // Transient failures are NOT user-actionable — they self-heal.
        assert!(!FailureCategory::Transport.is_user_actionable());
        assert!(!FailureCategory::ProviderServer.is_user_actionable());
    }

    #[test]
    fn test_failure_category_as_str_stable_contract() {
        // These strings are part of the public OTel attribute contract
        // and must not change between releases without a version bump.
        assert_eq!(FailureCategory::Auth.as_str(), "auth");
        assert_eq!(FailureCategory::RateLimit.as_str(), "rate_limit");
        assert_eq!(FailureCategory::ContextWindow.as_str(), "context_window");
        assert_eq!(FailureCategory::ContentPolicy.as_str(), "content_policy");
        assert_eq!(FailureCategory::SchemaMismatch.as_str(), "schema_mismatch");
        assert_eq!(FailureCategory::McpHandshake.as_str(), "mcp_handshake");
        assert_eq!(FailureCategory::CircuitOpen.as_str(), "circuit_open");

        // Display is identical to as_str.
        assert_eq!(format!("{}", FailureCategory::Auth), "auth");
    }

    #[test]
    fn test_config_error_conversion() {
        let config_err = config::ConfigError::NotFound {
            key: "api_key".to_string(),
        };
        let err: Error = config_err.into();
        assert!(matches!(err, Error::Config(_)));
    }
}
