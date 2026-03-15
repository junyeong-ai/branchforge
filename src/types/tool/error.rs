//! Tool error types.

use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum ToolError {
    #[error("authorization denied: {tool} requires {permission}")]
    AuthorizationDenied { tool: String, permission: String },

    #[error("timeout after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    #[error("not found: {path}")]
    NotFound { path: String },

    #[error("invalid input: {message}")]
    InvalidInput { message: String },

    #[error("execution failed: {message}")]
    ExecutionFailed { message: String },

    #[error("blocked by hook: {reason}")]
    BlockedByHook { reason: String },

    #[error("security violation: {message}")]
    SecurityViolation { message: String },

    #[error("resource limit exceeded: {message}")]
    ResourceLimit { message: String },

    #[error("unknown tool: {name}")]
    UnknownTool { name: String },
}

impl ToolError {
    pub fn authorization_denied(tool: impl Into<String>, permission: impl Into<String>) -> Self {
        Self::AuthorizationDenied {
            tool: tool.into(),
            permission: permission.into(),
        }
    }

    pub fn timeout(timeout_ms: u64) -> Self {
        Self::Timeout { timeout_ms }
    }

    pub fn not_found(path: impl Into<String>) -> Self {
        Self::NotFound { path: path.into() }
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    pub fn execution_failed(message: impl Into<String>) -> Self {
        Self::ExecutionFailed {
            message: message.into(),
        }
    }

    pub fn blocked_by_hook(reason: impl Into<String>) -> Self {
        Self::BlockedByHook {
            reason: reason.into(),
        }
    }

    pub fn security_violation(message: impl Into<String>) -> Self {
        Self::SecurityViolation {
            message: message.into(),
        }
    }

    pub fn resource_limit(message: impl Into<String>) -> Self {
        Self::ResourceLimit {
            message: message.into(),
        }
    }

    pub fn unknown_tool(name: impl Into<String>) -> Self {
        Self::UnknownTool { name: name.into() }
    }

    pub fn contains(&self, pattern: &str) -> bool {
        self.to_string().contains(pattern)
    }
}
