//! # claude-agent
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
//! use claude_agent::query;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), claude_agent::Error> {
//!     let response = query("What is 2 + 2?").await?;
//!     println!("{}", response);
//!     Ok(())
//! }
//! ```
//!
//! ## Full Agent Example
//!
//! ```rust,no_run
//! use claude_agent::{Agent, AgentEvent, ToolAccess};
//! use futures::StreamExt;
//! use std::pin::pin;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), claude_agent::Error> {
//!     let agent = Agent::builder()
//!         .model("claude-sonnet-4-5")
//!         .tools(ToolAccess::all())
//!         .working_dir("./project")
//!         .build()
//!         .await?;
//!
//!     let stream = agent.execute_stream("Fix the bug").await?;
//!     let mut stream = pin!(stream);
//!     while let Some(event) = stream.next().await {
//!         match event? {
//!             AgentEvent::Text(text) => print!("{}", text),
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
pub mod budget;
pub mod client;
pub mod common;
pub mod config;
pub mod context;
pub mod credentials;
pub mod graph;
pub mod hooks;
pub mod mcp;
pub mod models;
pub mod observability;
pub mod output_style;
pub mod permissions;
#[cfg(feature = "plugins")]
pub mod plugins;
pub mod prelude;
pub mod prompting;
pub mod prompts;
pub mod provider;
pub mod runtime;
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

pub use agent::{Agent, AgentBuilder, AgentConfig, AgentEvent, AgentResult};
pub use auth::{Auth, Credential};
pub use client::{Client, ClientBuilder};
pub use credentials::{CredentialKind, CredentialRecord};
pub use graph::{
    Bookmark, Branch, BranchExport, BranchId, Checkpoint, ExportBookmark, ExportNode, GraphEvent,
    GraphEventBody, GraphMaterializer, GraphNode, NodeId, NodeKind, ReplayInput, SessionGraph,
};
pub use permissions::{PermissionMode, PermissionPolicy};
pub use prompting::PromptFrame;
pub use provider::{CapabilitySupport, ProviderProfile};
pub use runtime::{RunDescriptor, RuntimeEventRecorder};
pub use tools::{ExecutionContext, SchemaTool, Tool, ToolAccess, ToolRegistry};
pub use types::{ContentBlock, Message, Role, ToolDefinition, ToolError, ToolOutput, ToolResult};

// =========================================================================
// Commonly used configuration re-exports
// =========================================================================

pub use agent::{
    AgentMetrics, AgentModelConfig, AgentState, BudgetConfig, CacheConfig, CacheStrategy,
    ExecutionConfig, PromptConfig, SecurityConfig, SystemPromptMode, ToolStats,
};
pub use auth::{CredentialProvider, OAuthConfig};
pub use client::{
    BetaConfig, BetaFeature, CloudProvider, EffortLevel, FallbackConfig, ModelConfig, ModelType,
    OutputConfig, ProviderConfig,
};
pub use common::{ContentSource, Index, IndexRegistry, Named, SourceType, ToolRestricted};
pub use context::{
    ContextBuilder, MemoryLoader, MemoryProvider, PromptOrchestrator, RuleIndex, StaticContext,
};
pub use hooks::{CommandHook, Hook, HookContext, HookEvent, HookManager, HookOutput};
pub use output_style::OutputStyle;
pub use session::{
    Session, SessionConfig, SessionId, SessionManager, SessionMessage, SessionState, ToolState,
};
pub use skills::{SkillIndex, SkillResult, SkillRuntime};
pub use subagents::{SubagentIndex, builtin_subagents};

#[cfg(feature = "cli-integration")]
pub use auth::ClaudeCliProvider;
#[cfg(feature = "aws")]
pub use client::BedrockAdapter;
#[cfg(feature = "azure")]
pub use client::FoundryAdapter;
#[cfg(feature = "gcp")]
pub use client::VertexAdapter;
#[cfg(feature = "cli-integration")]
pub use output_style::{OutputStyleLoader, SystemPromptGenerator};
#[cfg(feature = "plugins")]
pub use plugins::{PluginDescriptor, PluginDiscovery, PluginError, PluginManager, PluginManifest};
#[cfg(feature = "cli-integration")]
pub use subagents::{SubagentFrontmatter, SubagentIndexLoader};

/// Error type for claude-agent operations.
///
/// All errors include actionable context to help diagnose and resolve issues.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// API returned an error response.
    #[error("API error (HTTP {status}): {message}", status = status.map(|s| s.to_string()).unwrap_or_else(|| "unknown".into()))]
    Api {
        message: String,
        status: Option<u16>,
        error_type: Option<String>,
    },

    /// Authentication failed.
    #[error("Authentication failed: {message}")]
    Auth { message: String },

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

    /// Context window token limit exceeded.
    #[error("Context limit exceeded: {current}/{max} tokens ({:.0}% used)", (*current as f64 / *max as f64) * 100.0)]
    ContextOverflow { current: usize, max: usize },

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

    /// Token configuration validation failed.
    #[error("Token validation failed: {0}")]
    TokenValidation(#[from] client::messages::TokenValidationError),

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
    #[error("Permission denied: {0}")]
    Permission(String),

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
    Session(String),

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
        Error::Auth {
            message: message.into(),
        }
    }

    pub fn category(&self) -> ErrorCategory {
        match self {
            Error::Auth { .. } => ErrorCategory::Authorization,
            Error::Api {
                status: Some(401 | 403),
                ..
            } => ErrorCategory::Authorization,
            Error::Permission(_) | Error::HookFailed { .. } | Error::HookTimeout { .. } => {
                ErrorCategory::Authorization
            }

            Error::Config(_)
            | Error::Parse(_)
            | Error::Env(_)
            | Error::InvalidRequest(_)
            | Error::TokenValidation(_) => ErrorCategory::Configuration,

            Error::Network(_)
            | Error::RateLimit { .. }
            | Error::ModelOverloaded { .. }
            | Error::CircuitOpen => ErrorCategory::Transient,
            Error::Api {
                status: Some(500..=599),
                ..
            } => ErrorCategory::Transient,

            Error::Session(_) | Error::Mcp(_) | Error::Stream(_) => ErrorCategory::Stateful,

            Error::BudgetExceeded { .. }
            | Error::ContextOverflow { .. }
            | Error::ContextWindowExceeded { .. }
            | Error::Timeout(_)
            | Error::ResourceExhausted(_) => ErrorCategory::ResourceLimit,

            Error::Io(_)
            | Error::Json(_)
            | Error::Tool(_)
            | Error::Api { .. }
            | Error::NotSupported { .. } => ErrorCategory::Internal,

            #[cfg(feature = "plugins")]
            Error::Plugin(_) => ErrorCategory::Configuration,
        }
    }

    pub fn is_unauthorized(&self) -> bool {
        matches!(
            self,
            Error::Api {
                status: Some(401),
                ..
            } | Error::Auth { .. }
        )
    }

    pub fn is_overloaded(&self) -> bool {
        match self {
            Error::Api {
                status: Some(529 | 503),
                ..
            } => true,
            Error::Api {
                error_type: Some(t),
                ..
            } if t.contains("overloaded") => true,
            Error::Api { message, .. } if message.to_lowercase().contains("overloaded") => true,
            Error::ModelOverloaded { .. } => true,
            _ => false,
        }
    }

    pub fn status_code(&self) -> Option<u16> {
        match self {
            Error::Api { status, .. } => *status,
            _ => None,
        }
    }

    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Error::RateLimit { retry_after } => *retry_after,
            _ => None,
        }
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
                Error::ContextOverflow {
                    current: current as usize,
                    max: limit as usize,
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

impl From<session::SessionError> for Error {
    fn from(err: session::SessionError) -> Self {
        match err {
            session::SessionError::NotFound { id } => {
                Error::Config(format!("Session not found: {}", id))
            }
            session::SessionError::Expired { id } => {
                Error::Config(format!("Session expired: {}", id))
            }
            session::SessionError::Storage { message } => Error::Config(message),
            session::SessionError::Serialization(e) => Error::Json(e),
            session::SessionError::Compact { message } => Error::Config(message),
            session::SessionError::Context(e) => e.into(),
        }
    }
}

impl From<security::SecurityError> for Error {
    fn from(err: security::SecurityError) -> Self {
        match err {
            security::SecurityError::Io(e) => Error::Io(e),
            security::SecurityError::ResourceLimit(msg) => Error::ResourceExhausted(msg),
            security::SecurityError::BashBlocked(msg) => Error::Permission(msg),
            security::SecurityError::DeniedPath(path) => {
                Error::Permission(format!("Denied path: {}", path.display()))
            }
            security::SecurityError::PathEscape(path) => {
                Error::Permission(format!("Path escapes sandbox: {}", path.display()))
            }
            security::SecurityError::NotWithinSandbox(path) => {
                Error::Permission(format!("Path not within sandbox: {}", path.display()))
            }
            security::SecurityError::InvalidPath(msg) => Error::Config(msg),
            security::SecurityError::AbsoluteSymlink(path) => Error::Permission(format!(
                "Absolute symlink outside sandbox: {}",
                path.display()
            )),
            security::SecurityError::SymlinkDepthExceeded { path, max } => Error::Permission(
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
                Error::Permission(format!("Sandbox path not accessible: {}", path.display()))
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

/// Simple query function for one-shot requests
pub async fn query(prompt: &str) -> Result<String> {
    let client = Client::builder().auth(Auth::FromEnv).await?.build().await?;
    client.query(prompt).await
}

/// Query with a specific model
pub async fn query_with_model(model: &str, prompt: &str) -> Result<String> {
    use client::CreateMessageRequest;
    let client = Client::builder().auth(Auth::FromEnv).await?.build().await?;
    let request =
        CreateMessageRequest::new(model, vec![types::Message::user(prompt)]).max_tokens(8192);
    let response = client.send(request).await?;
    Ok(response.text())
}

/// Stream a response for one-shot requests
pub async fn stream(
    prompt: &str,
) -> Result<impl futures::Stream<Item = Result<String>> + Send + 'static + use<>> {
    let client = Client::builder().auth(Auth::FromEnv).await?.build().await?;
    client.stream(prompt).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = Error::Api {
            message: "Invalid API key".to_string(),
            status: Some(401),
            error_type: None,
        };
        assert!(err.to_string().contains("Invalid API key"));
    }

    #[test]
    fn test_error_category() {
        let rate_limit = Error::RateLimit { retry_after: None };
        assert_eq!(rate_limit.category(), ErrorCategory::Transient);

        let server_error = Error::Api {
            message: "Internal error".to_string(),
            status: Some(500),
            error_type: None,
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
