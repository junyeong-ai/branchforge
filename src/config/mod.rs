//! Pluggable configuration provider system.
//!
//! ```rust,no_run
//! use branchforge::config::ConfigBuilder;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = ConfigBuilder::new()
//!     .env()
//!     .file(".claude/settings.json")
//!     .build()
//!     .await?;
//! # Ok(())
//! # }
//! ```

pub mod cloud;
pub mod composite;
pub mod env;
pub mod file;
pub mod memory;
pub mod provider;
pub mod settings;
pub mod validator;

pub use cloud::{BedrockConfig, CloudConfig, FoundryConfig, TokenLimits, VertexConfig};
pub use composite::CompositeConfigProvider;
pub use env::EnvConfigProvider;
pub use file::FileConfigProvider;
pub use memory::MemoryConfigProvider;
pub use provider::{ConfigProvider, ConfigProviderExt};
pub use settings::{
    AuthorizationConfig, HookConfig, HooksConfig, NetworkSandboxConfig, SandboxConfig, Settings,
    SettingsLoader, SettingsSource, ToolSearchConfig,
};
pub use validator::{ConfigValidator, ValueType};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Key not found: {key}")]
    NotFound { key: String },

    #[error("Invalid value for {key}: {message}")]
    InvalidValue { key: String, message: String },

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Environment error: {0}")]
    Env(#[from] std::env::VarError),

    #[error("Provider error: {message}")]
    Provider { message: String },

    #[error("{0}")]
    ValidationErrors(ValidationErrors),
}

#[derive(Debug)]
pub struct ValidationErrors(pub Vec<ConfigError>);

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Validation failed: ")?;
        let msgs: Vec<String> = self.0.iter().map(|e| e.to_string()).collect();
        write!(f, "{}", msgs.join("; "))
    }
}

pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

pub struct ConfigBuilder {
    providers: Vec<Box<dyn ConfigProvider>>,
}

impl ConfigBuilder {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn env(mut self) -> Self {
        self.providers.push(Box::new(EnvConfigProvider::new()));
        self
    }

    pub fn env_with_prefix(mut self, prefix: &str) -> Self {
        self.providers
            .push(Box::new(EnvConfigProvider::prefixed(prefix)));
        self
    }

    pub fn file(mut self, path: impl AsRef<std::path::Path>) -> Self {
        self.providers.push(Box::new(FileConfigProvider::new(
            path.as_ref().to_path_buf(),
        )));
        self
    }

    pub fn memory(mut self, provider: MemoryConfigProvider) -> Self {
        self.providers.push(Box::new(provider));
        self
    }

    pub fn provider(mut self, provider: Box<dyn ConfigProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    pub async fn build(self) -> ConfigResult<CompositeConfigProvider> {
        let mut composite = CompositeConfigProvider::new();
        for provider in self.providers {
            composite.add_provider(provider);
        }
        Ok(composite)
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_error_display() {
        let err = ConfigError::NotFound {
            key: "api_key".to_string(),
        };
        assert!(err.to_string().contains("api_key"));
    }

    #[test]
    fn test_config_builder() {
        let builder = ConfigBuilder::new().env().env_with_prefix("CLAUDE_");
        assert!(!builder.providers.is_empty());
    }
}
