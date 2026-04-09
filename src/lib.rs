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
//!     let agent = Agent::builder()
//!         .model("claude-sonnet-4-5")
//!         .tools(ToolSurface::core())
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
pub mod events;
pub mod graph;
pub mod hooks;
pub mod ir;
pub mod mcp;
pub mod models;
pub mod observability;
pub mod orchestration;
pub mod output_style;
#[cfg(feature = "plugins")]
pub mod plugins;
pub mod prelude;
pub mod prompts;
#[cfg(feature = "scheduling")]
pub mod scheduling;
pub mod security;
pub mod session;
pub mod skills;
pub mod subagents;
pub mod tokens;
pub mod tools;
pub mod types;

// =========================================================================
// Core API re-exports (user-facing types)
// =========================================================================

pub use agent::{
    Agent, AgentBuilder, AgentCheckpoint, AgentConfig, AgentEvent, AgentResult, AgentRuntime,
    RunConfig,
};
pub use auth::{Auth, Credential};
pub use auth::{CredentialKind, CredentialRecord};
pub use authorization::{
    ApprovalReceiver, ApprovalRequest, ApprovalResponse, ApprovalSender, ExecutionMode, ToolPolicy,
    approval_channel,
};

// Provider client stack — the only LLM call surface. There is no longer
// a monolithic `Client` type; applications either compose a
// `ProviderClient` directly or pick an opinionated [`Preset`] and let
// the agent runtime resolve the right (codec, transport) pair.
pub use client::codec::{
    AnthropicMessagesCodec, BedrockConverseCodec, EncodedRequest, EndpointShape,
    GeminiGenerateCodec, InvocationMode, ModelCodec, OpenAiChatCodec, OpenAiResponsesCodec,
};
pub use client::llm_call::{CircuitBrokenClient, FallingBackClient, LlmCall, RetryingClient};
pub use client::preset::{Preset, from_env as preset_from_env};
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
    Bookmark, Branch, BranchExport, BranchId, Checkpoint, ExportBookmark, ExportNode, GraphError,
    GraphEvent, GraphEventBody, GraphMaterializer, GraphNode, NodeId, NodeKind, ReplayInput,
    SessionGraph,
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
pub use common::{ContentSource, Index, IndexRegistry, Named, SourceType, ToolRestricted};
pub use context::{
    ContextBuilder, MemoryLoader, MemoryProvider, PromptOrchestrator, RuleIndex, StaticContext,
};
pub use hooks::{CommandHook, Hook, HookContext, HookEvent, HookOutput, HookRegistry};
pub use output_style::OutputStyle;
pub use session::{
    InMemoryStore, MemoryEntry, MemoryStore, ScopedSessionManager, Session, SessionConfig,
    SessionId, SessionManager, SessionMessage, SessionState, ToolState,
};
pub use skills::{SkillIndex, SkillResult, SkillRuntime};
pub use subagents::{SubagentIndex, builtin_subagents};

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

/// Error category for unified error handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Authentication or authorization failures (401, 403)
    Authorization,
    /// Configuration, parsing, or setup errors
    Configuration,
    /// Network, rate limit, or transient errors that may succeed on retry
    Transient,
    /// Session, MCP, or other stateful operation errors
    Stateful,
    /// Internal errors (IO, JSON, unexpected states)
    Internal,
    /// Resource limits (budget, context, timeout)
    ResourceLimit,
}

impl Error {
    pub fn auth(message: impl Into<String>) -> Self {
        Error::Authentication {
            message: message.into(),
        }
    }

    pub fn category(&self) -> ErrorCategory {
        match self {
            Error::Authentication { .. } => ErrorCategory::Authorization,
            Error::Authorization(_) | Error::HookFailed { .. } | Error::HookTimeout { .. } => {
                ErrorCategory::Authorization
            }

            Error::Config(_) | Error::Parse(_) | Error::Env(_) | Error::InvalidRequest(_) => {
                ErrorCategory::Configuration
            }

            Error::Network(_)
            | Error::RateLimit { .. }
            | Error::ModelOverloaded { .. }
            | Error::CircuitOpen => ErrorCategory::Transient,

            Error::Session(_) | Error::Mcp(_) | Error::Stream(_) => ErrorCategory::Stateful,

            Error::BudgetExceeded { .. }
            | Error::ContextWindowExceeded { .. }
            | Error::Timeout(_)
            | Error::ResourceExhausted(_) => ErrorCategory::ResourceLimit,

            Error::Io(_) | Error::Json(_) | Error::Tool(_) | Error::NotSupported { .. } => {
                ErrorCategory::Internal
            }

            Error::Provider { kind, .. } => match kind {
                error::ProviderErrorKind::Auth | error::ProviderErrorKind::Quota => {
                    ErrorCategory::Authorization
                }
                error::ProviderErrorKind::RateLimit
                | error::ProviderErrorKind::Server
                | error::ProviderErrorKind::Network => ErrorCategory::Transient,
                error::ProviderErrorKind::BadRequest
                | error::ProviderErrorKind::ContentFilter
                | error::ProviderErrorKind::Cancelled => ErrorCategory::Configuration,
                error::ProviderErrorKind::PayloadTooLarge => ErrorCategory::ResourceLimit,
            },
            Error::InvalidComposition { .. } => ErrorCategory::Configuration,

            #[cfg(feature = "plugins")]
            Error::Plugin(_) => ErrorCategory::Configuration,
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
/// Resolves a [`Preset`] from `BRANCHFORGE_PROVIDER` (and the vendor's
/// usual env vars), sends a single user message via [`ProviderClient`],
/// and returns the joined text content. The model is taken from
/// `BRANCHFORGE_MODEL` if set.
pub async fn query(prompt: &str) -> Result<String> {
    let pc = client::preset::from_env().await?;
    let model = std::env::var("BRANCHFORGE_MODEL").ok();
    query_with_provider(&pc, model.as_deref(), prompt).await
}

/// Query with a specific model id, resolving the preset from env vars.
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
        };
        assert!(err.to_string().contains("Invalid API key"));
    }

    #[test]
    fn test_error_category() {
        let rate_limit = Error::RateLimit { retry_after: None };
        assert_eq!(rate_limit.category(), ErrorCategory::Transient);

        let server_error = Error::Provider {
            provider: "anthropic",
            kind: error::ProviderErrorKind::Server,
            message: "Internal error".to_string(),
            hint: None,
            retryable: true,
            status: Some(500),
        };
        assert_eq!(server_error.category(), ErrorCategory::Transient);

        let auth_error = Error::auth("Invalid token");
        assert_eq!(auth_error.category(), ErrorCategory::Authorization);
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
