//! Context management with progressive disclosure for optimal token usage.

pub mod builder;
pub mod import_extractor;
pub mod level;
pub mod memory_loader;
pub mod orchestrator;
pub mod prompt_frame;
pub mod provider;
pub mod routing;
pub mod rule_index;
pub mod static_context;

pub use crate::types::TokenUsage;
pub use builder::ContextBuilder;
pub use import_extractor::ImportExtractor;
pub use level::{LeveledMemoryProvider, enterprise_base_path, user_base_path};
pub use memory_loader::{MemoryContent, MemoryLoader, MemoryLoaderConfig};
pub use orchestrator::PromptOrchestrator;
pub use prompt_frame::PromptFrame;
pub use provider::{FileMemoryProvider, MemoryContextProvider, MemoryProvider};
pub use routing::RoutingStrategy;
pub use rule_index::RuleIndex;
pub use static_context::{McpToolMeta, StaticContext};

// Re-export SkillIndex from skills module for convenience
pub use crate::skills::SkillIndex;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ContextError {
    #[error("Source error: {message}")]
    Source { message: String },

    #[error("Token budget exceeded: {current} > {limit}")]
    TokenBudgetExceeded { current: u64, limit: u64 },

    #[error("Skill not found: {name}")]
    SkillNotFound { name: String },

    #[error("Rule not found: {name}")]
    RuleNotFound { name: String },

    #[error("Parse error: {message}")]
    Parse { message: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type ContextResult<T> = std::result::Result<T, ContextError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_error_display() {
        let err = ContextError::SkillNotFound {
            name: "test-skill".to_string(),
        };
        assert!(err.to_string().contains("test-skill"));
    }

    #[test]
    fn test_token_budget_error() {
        let err = ContextError::TokenBudgetExceeded {
            current: 250_000,
            limit: 200_000,
        };
        assert!(err.to_string().contains("250000"));
        assert!(err.to_string().contains("200000"));
    }
}
