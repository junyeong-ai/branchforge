//! Authentication module for Claude API.
//!
//! The `Auth` enum is the primary entry point for all authentication methods.
//! SDK users should use `Auth` to configure authentication, which internally
//! resolves to `Credential` for API requests.

mod cache;
mod config;
mod credential;
mod helper;
mod provider;
mod providers;
#[cfg(feature = "cli-auth")]
mod storage;

use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};

pub use cache::CachedProvider;
pub use config::{CLAUDE_CODE_BETA, OAuthConfig, OAuthConfigBuilder};
pub use credential::{Credential, OAuthCredential};
pub use helper::{ApiKeyHelper, AwsCredentialRefresh, AwsCredentials, CredentialManager};
pub use provider::CredentialProvider;
#[cfg(feature = "cli-auth")]
pub use providers::ClaudeCliProvider;
pub use providers::{ChainProvider, EnvironmentProvider, ExplicitProvider};
#[cfg(feature = "cli-auth")]
pub use storage::CliCredentials;

use crate::Result;

/// Primary authentication configuration for SDK usage.
///
/// `Auth` provides a unified interface for all authentication methods.
/// Use this enum to configure how the SDK authenticates with Claude API.
///
/// # Variants
///
/// - `ApiKey`: Direct API key authentication
/// - `FromEnv`: Load API key from ANTHROPIC_API_KEY environment variable
/// - `ClaudeCli`: Use credentials from Claude Code CLI (requires `cli-auth` feature)
/// - `OAuth`: OAuth token authentication
/// - `Resolved`: Pre-resolved credential (for testing or credential reuse)
/// - `Bedrock`: AWS Bedrock (requires `aws` feature)
/// - `Vertex`: GCP Vertex AI (requires `gcp` feature)
/// - `Foundry`: Azure Foundry (requires `azure` feature)
#[derive(Clone, Default)]
pub enum Auth {
    ApiKey(SecretString),
    #[default]
    FromEnv,
    #[cfg(feature = "cli-auth")]
    ClaudeCli,
    OAuth {
        token: SecretString,
    },
    Resolved(Credential),
    #[cfg(feature = "aws")]
    Bedrock {
        region: String,
    },
    #[cfg(feature = "gcp")]
    Vertex {
        project: String,
        region: String,
    },
    #[cfg(feature = "azure")]
    Foundry {
        resource: String,
    },
    #[cfg(feature = "openai")]
    OpenAi {
        api_key: SecretString,
    },
    #[cfg(feature = "gemini")]
    Gemini {
        api_key: SecretString,
    },
}

impl Auth {
    pub fn api_key(key: impl Into<String>) -> Self {
        Self::ApiKey(SecretString::from(key.into()))
    }

    pub fn from_env() -> Self {
        Self::FromEnv
    }

    #[cfg(feature = "cli-auth")]
    pub fn claude_cli() -> Self {
        Self::ClaudeCli
    }

    pub fn oauth(token: impl Into<String>) -> Self {
        Self::OAuth {
            token: SecretString::from(token.into()),
        }
    }

    #[cfg(feature = "aws")]
    pub fn bedrock(region: impl Into<String>) -> Self {
        Self::Bedrock {
            region: region.into(),
        }
    }

    #[cfg(feature = "gcp")]
    pub fn vertex(project: impl Into<String>, region: impl Into<String>) -> Self {
        Self::Vertex {
            project: project.into(),
            region: region.into(),
        }
    }

    #[cfg(feature = "azure")]
    pub fn foundry(resource: impl Into<String>) -> Self {
        Self::Foundry {
            resource: resource.into(),
        }
    }

    #[cfg(feature = "openai")]
    pub fn openai(api_key: impl Into<String>) -> Self {
        Self::OpenAi {
            api_key: SecretString::from(api_key.into()),
        }
    }

    #[cfg(feature = "gemini")]
    pub fn gemini(api_key: impl Into<String>) -> Self {
        Self::Gemini {
            api_key: SecretString::from(api_key.into()),
        }
    }

    pub fn resolved(credential: Credential) -> Self {
        Self::Resolved(credential)
    }

    pub async fn resolve(&self) -> Result<Credential> {
        match self {
            Self::ApiKey(key) => Ok(Credential::api_key(key.expose_secret())),
            Self::FromEnv => EnvironmentProvider::new().resolve().await,
            #[cfg(feature = "cli-auth")]
            Self::ClaudeCli => ClaudeCliProvider::new().resolve().await,
            Self::OAuth { token } => Ok(Credential::oauth(token.expose_secret())),
            Self::Resolved(credential) => Ok(credential.clone()),
            #[cfg(feature = "aws")]
            Self::Bedrock { .. } => Ok(Credential::placeholder()),
            #[cfg(feature = "gcp")]
            Self::Vertex { .. } => Ok(Credential::placeholder()),
            #[cfg(feature = "azure")]
            Self::Foundry { .. } => Ok(Credential::placeholder()),
            #[cfg(feature = "openai")]
            Self::OpenAi { api_key } => Ok(Credential::api_key(api_key.expose_secret())),
            #[cfg(feature = "gemini")]
            Self::Gemini { api_key } => Ok(Credential::api_key(api_key.expose_secret())),
        }
    }

    /// Returns (credential, optional_provider) for auth methods that support token refresh.
    pub async fn resolve_with_provider(
        &self,
    ) -> Result<(Credential, Option<Arc<dyn CredentialProvider>>)> {
        match self {
            Self::ApiKey(key) => Ok((Credential::api_key(key.expose_secret()), None)),
            Self::FromEnv => {
                let provider = EnvironmentProvider::new();
                let credential = provider.resolve().await?;
                Ok((credential, None))
            }
            #[cfg(feature = "cli-auth")]
            Self::ClaudeCli => {
                let provider = Arc::new(ClaudeCliProvider::new());
                let credential = provider.resolve().await?;
                Ok((credential, Some(provider)))
            }
            Self::OAuth { token } => Ok((Credential::oauth(token.expose_secret()), None)),
            Self::Resolved(credential) => Ok((credential.clone(), None)),
            #[cfg(feature = "aws")]
            Self::Bedrock { .. } => Ok((Credential::placeholder(), None)),
            #[cfg(feature = "gcp")]
            Self::Vertex { .. } => Ok((Credential::placeholder(), None)),
            #[cfg(feature = "azure")]
            Self::Foundry { .. } => Ok((Credential::placeholder(), None)),
            #[cfg(feature = "openai")]
            Self::OpenAi { api_key } => Ok((Credential::api_key(api_key.expose_secret()), None)),
            #[cfg(feature = "gemini")]
            Self::Gemini { api_key } => Ok((Credential::api_key(api_key.expose_secret()), None)),
        }
    }

    pub fn is_cloud_provider(&self) -> bool {
        #[allow(unreachable_patterns)]
        match self {
            #[cfg(feature = "aws")]
            Self::Bedrock { .. } => true,
            #[cfg(feature = "gcp")]
            Self::Vertex { .. } => true,
            #[cfg(feature = "azure")]
            Self::Foundry { .. } => true,
            #[cfg(feature = "openai")]
            Self::OpenAi { .. } => true,
            #[cfg(feature = "gemini")]
            Self::Gemini { .. } => true,
            _ => false,
        }
    }

    pub fn is_oauth(&self) -> bool {
        match self {
            Self::OAuth { .. } => true,
            #[cfg(feature = "cli-auth")]
            Self::ClaudeCli => true,
            Self::Resolved(cred) => cred.is_oauth(),
            _ => false,
        }
    }

    /// Check if Anthropic's server-side tools (WebSearch, WebFetch) are available.
    ///
    /// Server-side tools are available with Anthropic direct API (API Key or OAuth)
    /// but NOT with cloud providers (Bedrock, Vertex, Foundry).
    pub fn supports_server_tools(&self) -> bool {
        !self.is_cloud_provider()
    }
}

impl From<&str> for Auth {
    fn from(key: &str) -> Self {
        Self::ApiKey(SecretString::from(key))
    }
}

impl From<String> for Auth {
    fn from(key: String) -> Self {
        Self::ApiKey(SecretString::from(key))
    }
}

impl From<Credential> for Auth {
    fn from(credential: Credential) -> Self {
        Self::Resolved(credential)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_from_str() {
        let auth: Auth = "sk-test-key".into();
        assert!(matches!(auth, Auth::ApiKey(_)));
    }

    #[test]
    fn test_auth_from_string() {
        let auth: Auth = "sk-test-key".to_string().into();
        assert!(matches!(auth, Auth::ApiKey(_)));
    }

    #[test]
    fn test_auth_default() {
        let auth = Auth::default();
        assert!(matches!(auth, Auth::FromEnv));
    }

    #[test]
    fn test_auth_constructors() {
        assert!(matches!(Auth::api_key("key"), Auth::ApiKey(_)));
        assert!(matches!(Auth::from_env(), Auth::FromEnv));
        #[cfg(feature = "cli-auth")]
        assert!(matches!(Auth::claude_cli(), Auth::ClaudeCli));
        assert!(matches!(Auth::oauth("token"), Auth::OAuth { .. }));
        assert!(matches!(
            Auth::resolved(Credential::api_key("key")),
            Auth::Resolved(_)
        ));
    }

    #[test]
    fn test_auth_from_credential() {
        let cred = Credential::api_key("test-key");
        let auth: Auth = cred.into();
        assert!(matches!(auth, Auth::Resolved(_)));
    }

    #[tokio::test]
    async fn test_auth_resolved_resolve() {
        let cred = Credential::api_key("test-key");
        let auth = Auth::resolved(cred);
        let resolved = auth.resolve().await.unwrap();
        assert!(!resolved.is_placeholder());
    }
}
