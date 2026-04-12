//! Claude Code CLI credential provider.
//!
//! Reads credentials from Claude Code CLI storage (macOS Keychain or
//! `~/.claude/.credentials.json`) and refreshes OAuth tokens via
//! standard OAuth2 refresh_token grant (RFC 6749 Section 6).

use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::auth::refresh;
use crate::auth::storage::{CliCredentials, load_cli_credentials, save_cli_credentials};
use crate::auth::{Credential, CredentialProvider, OAuthCredential};
use crate::common::env::{EnvLookup, SystemEnv};
use crate::{Error, Result};

/// Default timeout for token refresh HTTP requests.
const DEFAULT_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// Phase I-1: Configuration for [`ClaudeCliProvider`].
///
/// All fields that were previously read from `std::env::var` — the
/// token endpoint URL and the OAuth client id — live here so they
/// can be resolved through an injected [`EnvLookup`] or constructed
/// explicitly in tests.
///
/// Production callers typically use [`ClaudeCliProvider::new()`],
/// which calls [`Self::from_env`] and then [`Self::from_env_with`]
/// with [`SystemEnv`]; tests use [`ClaudeCliProvider::with_config`]
/// directly with a hand-rolled config. The free helper
/// `auth::refresh::token_url()` that used to expose the token URL
/// as a process-wide function was removed in Phase I-1 — its
/// responsibility now lives on this struct.
#[derive(Debug, Clone)]
pub struct ClaudeCliConfig {
    /// OAuth2 token endpoint URL. Defaults to the Anthropic
    /// console endpoint; override via `BRANCHFORGE_TOKEN_URL`.
    pub token_url: String,
    /// Optional OAuth client id sent with the refresh_token grant.
    /// Defaults to `None`; override via `BRANCHFORGE_OAUTH_CLIENT_ID`.
    pub client_id: Option<String>,
    /// HTTP timeout applied to refresh requests.
    pub refresh_timeout: Duration,
}

/// Phase I-1: canonical default Claude OAuth token endpoint. Used
/// when neither the process environment nor the caller supplies an
/// override. This is the only `const` URL in the module — the
/// former `DEFAULT_TOKEN_URL` in `auth::refresh` was deleted.
const DEFAULT_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

impl Default for ClaudeCliConfig {
    fn default() -> Self {
        Self {
            token_url: DEFAULT_TOKEN_URL.to_string(),
            client_id: None,
            refresh_timeout: DEFAULT_REFRESH_TIMEOUT,
        }
    }
}

impl ClaudeCliConfig {
    /// Resolve against the process environment. Convenience wrapper
    /// around [`Self::from_env_with`] with [`SystemEnv`].
    pub fn from_env() -> Self {
        Self::from_env_with(&SystemEnv)
    }

    /// Resolve against an injected [`EnvLookup`]. Reads
    /// `BRANCHFORGE_TOKEN_URL` and `BRANCHFORGE_OAUTH_CLIENT_ID`
    /// through the seam; refresh timeout stays at the default
    /// (callers that need a custom timeout build the struct by
    /// hand).
    pub fn from_env_with(env: &dyn EnvLookup) -> Self {
        Self {
            token_url: env.get_or("BRANCHFORGE_TOKEN_URL", DEFAULT_TOKEN_URL),
            client_id: env.get("BRANCHFORGE_OAUTH_CLIENT_ID"),
            refresh_timeout: DEFAULT_REFRESH_TIMEOUT,
        }
    }
}

/// Provider that reads credentials from Claude Code CLI storage
/// and refreshes OAuth tokens directly via the token endpoint.
pub struct ClaudeCliProvider {
    http: reqwest::Client,
    /// Serializes refresh attempts to prevent concurrent refresh_token usage,
    /// which can cause failures when the server rotates refresh tokens.
    refresh_guard: Mutex<()>,
    config: ClaudeCliConfig,
}

impl ClaudeCliProvider {
    /// Create a provider with configuration read from the process
    /// environment. Equivalent to
    /// `Self::with_config(ClaudeCliConfig::from_env())`.
    pub fn new() -> Self {
        Self::with_config(ClaudeCliConfig::from_env())
    }

    /// Phase I-1: Create a provider with an explicit config. Tests
    /// use this to inject a mock token endpoint and client id
    /// without touching the process environment.
    pub fn with_config(config: ClaudeCliConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(config.refresh_timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            refresh_guard: Mutex::new(()),
            config,
        }
    }

    /// Merge server response with original metadata the server doesn't return.
    ///
    /// The OAuth2 token endpoint only returns token fields (access_token,
    /// refresh_token, expires_in). Application-level metadata like
    /// subscription_type and scopes must be preserved from the original.
    fn merge_metadata(refreshed: &mut OAuthCredential, original: &OAuthCredential) {
        refreshed
            .subscription_type
            .clone_from(&original.subscription_type);
        if refreshed.scopes.is_empty() {
            refreshed.scopes.clone_from(&original.scopes);
        }
        if refreshed.refresh_token.is_none() {
            refreshed.refresh_token.clone_from(&original.refresh_token);
        }
    }
}

impl Default for ClaudeCliProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CredentialProvider for ClaudeCliProvider {
    fn name(&self) -> &str {
        "claude_cli"
    }

    async fn resolve(&self) -> Result<Credential> {
        let creds = load_cli_credentials().await?.ok_or_else(|| {
            Error::auth("Claude CLI credentials not found. Run 'claude login' first.")
        })?;

        let oauth = creds
            .oauth()
            .ok_or_else(|| Error::auth("No OAuth credentials in Claude CLI config"))?;

        if oauth.needs_refresh() {
            return self.refresh().await;
        }

        Ok(Credential::OAuth(oauth.clone()))
    }

    async fn refresh(&self) -> Result<Credential> {
        // Serialize refresh: prevent concurrent refresh_token usage.
        // Same pattern as CachedProvider (cache.rs) for thundering herd prevention.
        let _guard = self.refresh_guard.lock().await;

        // Re-load from storage: another caller may have refreshed while we waited.
        let creds = load_cli_credentials()
            .await?
            .ok_or_else(|| Error::auth("Credentials not found"))?;

        let oauth = creds
            .oauth()
            .ok_or_else(|| Error::auth("No OAuth credentials found"))?;

        // Double-check after acquiring lock.
        if !oauth.needs_refresh() {
            return Ok(Credential::OAuth(oauth.clone()));
        }

        let refresh_token = oauth.refresh_token.as_ref().ok_or_else(|| {
            Error::auth("No refresh token available. Run 'claude login' to re-authenticate.")
        })?;

        let mut refreshed = refresh::refresh_access_token(
            &self.http,
            &self.config.token_url,
            refresh_token,
            self.config.client_id.as_deref(),
        )
        .await?;

        Self::merge_metadata(&mut refreshed, oauth);

        save_cli_credentials(&CliCredentials {
            claude_ai_oauth: Some(refreshed.clone()),
        })
        .await?;

        Ok(Credential::OAuth(refreshed))
    }

    fn supports_refresh(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::OAuthCredential;
    use secrecy::SecretString;

    #[test]
    fn test_merge_metadata_preserves_subscription_type() {
        let original = OAuthCredential {
            access_token: SecretString::from("old"),
            refresh_token: Some(SecretString::from("rt")),
            expires_at: None,
            scopes: vec!["user:inference".into()],
            subscription_type: Some("pro".into()),
        };

        let mut refreshed = OAuthCredential {
            access_token: SecretString::from("new"),
            refresh_token: Some(SecretString::from("new-rt")),
            expires_at: Some(9999999999),
            scopes: vec![],
            subscription_type: None,
        };

        ClaudeCliProvider::merge_metadata(&mut refreshed, &original);

        assert_eq!(refreshed.subscription_type, Some("pro".into()));
        assert_eq!(refreshed.scopes, vec!["user:inference".to_string()]);
        // New refresh_token is present, so it should NOT be overwritten
        assert!(refreshed.refresh_token.is_some());
    }

    #[test]
    fn test_merge_metadata_preserves_refresh_token_when_not_rotated() {
        let original = OAuthCredential {
            access_token: SecretString::from("old"),
            refresh_token: Some(SecretString::from("original-rt")),
            expires_at: None,
            scopes: vec![],
            subscription_type: None,
        };

        let mut refreshed = OAuthCredential {
            access_token: SecretString::from("new"),
            refresh_token: None, // Server didn't rotate
            expires_at: Some(9999999999),
            scopes: vec![],
            subscription_type: None,
        };

        ClaudeCliProvider::merge_metadata(&mut refreshed, &original);

        assert!(refreshed.refresh_token.is_some());
    }

    #[test]
    fn test_supports_refresh() {
        let provider = ClaudeCliProvider::new();
        assert!(provider.supports_refresh());
    }

    #[test]
    fn test_name() {
        let provider = ClaudeCliProvider::new();
        assert_eq!(provider.name(), "claude_cli");
    }

    // ── Phase I-1: ClaudeCliConfig injection tests ─────────────────

    /// In-memory [`EnvLookup`] fake used by the I-1 test suite.
    /// Local copy to avoid cross-module test dependencies.
    #[derive(Debug, Default)]
    struct FakeEnv(std::collections::HashMap<String, String>);

    impl FakeEnv {
        fn with(mut self, key: &str, val: &str) -> Self {
            self.0.insert(key.into(), val.into());
            self
        }
    }

    impl EnvLookup for FakeEnv {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn phase_i1_config_from_env_with_reads_token_url_override() {
        let env = FakeEnv::default().with("BRANCHFORGE_TOKEN_URL", "https://mock.invalid/token");
        let config = ClaudeCliConfig::from_env_with(&env);
        assert_eq!(config.token_url, "https://mock.invalid/token");
        assert!(config.client_id.is_none());
    }

    #[test]
    fn phase_i1_config_from_env_with_reads_client_id_override() {
        let env = FakeEnv::default().with("BRANCHFORGE_OAUTH_CLIENT_ID", "my-app-id");
        let config = ClaudeCliConfig::from_env_with(&env);
        assert_eq!(config.client_id.as_deref(), Some("my-app-id"));
    }

    #[test]
    fn phase_i1_config_from_env_with_falls_back_to_default_token_url() {
        let env = FakeEnv::default();
        let config = ClaudeCliConfig::from_env_with(&env);
        assert_eq!(config.token_url, DEFAULT_TOKEN_URL);
        assert!(config.client_id.is_none());
    }

    #[test]
    fn phase_i1_with_config_constructs_provider_with_custom_config() {
        let config = ClaudeCliConfig {
            token_url: "https://custom.invalid/oauth".into(),
            client_id: Some("explicit-id".into()),
            refresh_timeout: Duration::from_secs(5),
        };
        let provider = ClaudeCliProvider::with_config(config.clone());
        assert_eq!(provider.config.token_url, "https://custom.invalid/oauth");
        assert_eq!(provider.config.client_id.as_deref(), Some("explicit-id"));
        assert_eq!(provider.config.refresh_timeout, Duration::from_secs(5));
    }
}
