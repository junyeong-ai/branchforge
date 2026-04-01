//! macOS Keychain credential storage.

use std::process::Command;

use super::CliCredentials;
use crate::Result;

const SERVICE_NAME: &str = "Claude Code-credentials";

/// Resolve the account name for keychain operations.
///
/// Matches the account used by Claude Code CLI (the OS username).
fn account_name() -> String {
    std::env::var("USER").unwrap_or_default()
}

/// macOS Keychain storage.
pub struct KeychainStorage;

impl KeychainStorage {
    /// Save credentials to Keychain using security command.
    ///
    /// Runs the `security` CLI in a blocking thread to avoid stalling
    /// the tokio runtime. Uses `-U` flag to update if an entry already exists.
    pub async fn save(credentials: &CliCredentials) -> Result<()> {
        let json = serde_json::to_string(credentials)
            .map_err(|e| crate::Error::auth(format!("Failed to serialize credentials: {}", e)))?;

        let account = account_name();
        tokio::task::spawn_blocking(move || {
            let output = Command::new("security")
                .args([
                    "add-generic-password",
                    "-U",
                    "-s",
                    SERVICE_NAME,
                    "-a",
                    &account,
                    "-w",
                    &json,
                ])
                .output()
                .map_err(|e| {
                    crate::Error::auth(format!("Failed to execute security command: {}", e))
                })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(crate::Error::auth(format!(
                    "Failed to save to keychain: {}",
                    stderr
                )));
            }

            Ok(())
        })
        .await
        .map_err(|e| crate::Error::auth(format!("Keychain task panicked: {}", e)))?
    }

    /// Load credentials from Keychain using security command.
    ///
    /// Runs the `security` CLI in a blocking thread to avoid stalling
    /// the tokio runtime.
    pub async fn load() -> Result<Option<CliCredentials>> {
        tokio::task::spawn_blocking(|| {
            let output = match Command::new("security")
                .args(["find-generic-password", "-s", SERVICE_NAME, "-w"])
                .output()
            {
                Ok(o) => o,
                Err(e) => {
                    tracing::debug!("Failed to execute security command: {}", e);
                    return Ok(None);
                }
            };

            if !output.status.success() {
                tracing::debug!("Keychain entry not found for service: {}", SERVICE_NAME);
                return Ok(None);
            }

            let secret = String::from_utf8_lossy(&output.stdout);
            let secret = secret.trim();

            if secret.is_empty() {
                return Ok(None);
            }

            let creds: CliCredentials = serde_json::from_str(secret).map_err(|e| {
                crate::Error::auth(format!("Failed to parse keychain credentials: {}", e))
            })?;

            Ok(Some(creds))
        })
        .await
        .map_err(|e| crate::Error::auth(format!("Keychain task panicked: {}", e)))?
    }
}
