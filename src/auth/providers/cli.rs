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
use crate::{Error, Result};

/// Timeout for token refresh HTTP requests.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// Provider that reads credentials from Claude Code CLI storage
/// and refreshes OAuth tokens directly via the token endpoint.
pub struct ClaudeCliProvider {
    http: reqwest::Client,
    /// Serializes refresh attempts to prevent concurrent refresh_token usage,
    /// which can cause failures when the server rotates refresh tokens.
    refresh_guard: Mutex<()>,
}

impl ClaudeCliProvider {
    /// Create a new CLI provider.
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REFRESH_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            refresh_guard: Mutex::new(()),
        }
    }

    fn client_id() -> Option<String> {
        std::env::var("BRANCHFORGE_OAUTH_CLIENT_ID").ok()
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

        let client_id = Self::client_id();
        let token_url = refresh::token_url();
        let mut refreshed = refresh::refresh_access_token(
            &self.http,
            &token_url,
            refresh_token,
            client_id.as_deref(),
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
}
