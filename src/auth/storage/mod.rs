//! Credential storage implementations.

mod file;
#[cfg(target_os = "macos")]
mod keychain;

use serde::{Deserialize, Serialize};

use super::OAuthCredential;
use crate::Result;

pub use file::FileStorage;
#[cfg(target_os = "macos")]
pub use keychain::KeychainStorage;

/// Claude Code CLI credentials file structure.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliCredentials {
    /// OAuth credentials.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_ai_oauth: Option<OAuthCredential>,
}

impl CliCredentials {
    /// Get OAuth credentials if present.
    pub fn oauth(&self) -> Option<&OAuthCredential> {
        self.claude_ai_oauth.as_ref()
    }
}

/// Save CLI credentials to platform-specific storage.
///
/// Always saves to file. On macOS, also attempts to save to Keychain
/// (logs a warning on failure since file storage is the fallback).
pub async fn save_cli_credentials(credentials: &CliCredentials) -> Result<()> {
    FileStorage::save(credentials).await?;

    #[cfg(target_os = "macos")]
    {
        if let Err(e) = KeychainStorage::save(credentials).await {
            tracing::warn!("Failed to save to keychain, file storage used: {}", e);
        }
    }

    Ok(())
}

/// Load CLI credentials from platform-specific storage.
pub async fn load_cli_credentials() -> Result<Option<CliCredentials>> {
    #[cfg(target_os = "macos")]
    {
        if let Some(creds) = KeychainStorage::load().await? {
            return Ok(Some(creds));
        }
    }

    FileStorage::load().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn test_cli_credentials_parse() {
        let json = r#"{
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-test",
                "refreshToken": "sk-ant-ort01-test",
                "expiresAt": 1234567890,
                "scopes": ["user:inference"],
                "subscriptionType": "pro"
            }
        }"#;

        let creds: CliCredentials = serde_json::from_str(json).unwrap();
        let oauth = creds.oauth().unwrap();
        assert_eq!(oauth.access_token.expose_secret(), "sk-ant-oat01-test");
        assert_eq!(oauth.subscription_type, Some("pro".to_string()));
    }
}
