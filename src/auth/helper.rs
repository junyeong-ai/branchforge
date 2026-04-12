//! API Key Helper for dynamic credential generation.

#![allow(missing_docs)]

use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use secrecy::{ExposeSecret, SecretString};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::{Error, Result};

use std::fmt;

async fn run_shell_command(cmd: &str, context: &str) -> Result<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| Error::auth(format!("{} failed: {}", context, e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::auth(format!(
            "{} failed: {}",
            context,
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Debug)]
pub struct ApiKeyHelper {
    command: String,
    ttl: Duration,
    cache: Mutex<Option<CachedKey>>,
}

#[derive(Clone)]
struct CachedKey {
    key: SecretString,
    expires_at: Instant,
}

impl fmt::Debug for CachedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedKey")
            .field("key", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl ApiKeyHelper {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            ttl: Duration::from_secs(3600),
            cache: Mutex::new(None),
        }
    }

    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.ttl = Duration::from_millis(ttl_ms);
        self
    }

    /// Read helper configuration from the process environment.
    /// Returns `None` when `ANTHROPIC_API_KEY_HELPER` is unset —
    /// callers treat absent as "no helper configured" and fall
    /// back to other credential sources.
    ///
    /// Delegates to [`Self::from_env_with`] with
    /// [`crate::common::env::SystemEnv`].
    pub fn from_env() -> Option<Self> {
        Self::from_env_with(&crate::common::env::SystemEnv)
    }

    /// Phase I-1: Read helper configuration from an injected
    /// [`crate::common::env::EnvLookup`]. Keeps tests hermetic by
    /// avoiding any direct `std::env::var` call — the entire
    /// "helper configured?" decision flows through the seam.
    pub fn from_env_with(env: &dyn crate::common::env::EnvLookup) -> Option<Self> {
        let command = env.get("ANTHROPIC_API_KEY_HELPER")?;
        let ttl_ms = env
            .get("CLAUDE_CODE_API_KEY_HELPER_TTL_MS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(3_600_000);

        Some(Self::new(command).ttl_ms(ttl_ms))
    }

    pub async fn key(&self) -> Result<SecretString> {
        let mut cache = self.cache.lock().await;

        if let Some(ref cached) = *cache
            && Instant::now() < cached.expires_at
        {
            return Ok(cached.key.clone());
        }

        let key = run_shell_command(&self.command, "API key helper").await?;

        if key.is_empty() {
            return Err(Error::auth("API key helper returned empty key"));
        }

        let secret_key = SecretString::from(key);

        *cache = Some(CachedKey {
            key: secret_key.clone(),
            expires_at: Instant::now() + self.ttl,
        });

        Ok(secret_key)
    }

    pub async fn invalidate(&self) {
        *self.cache.lock().await = None;
    }
}

#[derive(Debug)]
pub struct AwsCredentialRefresh {
    auth_refresh_cmd: Option<String>,
    credential_export_cmd: Option<String>,
}

impl AwsCredentialRefresh {
    pub fn new() -> Self {
        Self {
            auth_refresh_cmd: None,
            credential_export_cmd: None,
        }
    }

    pub fn from_settings(
        auth_refresh: Option<String>,
        credential_export: Option<String>,
    ) -> Option<Self> {
        if auth_refresh.is_none() && credential_export.is_none() {
            return None;
        }

        Some(Self {
            auth_refresh_cmd: auth_refresh,
            credential_export_cmd: credential_export,
        })
    }

    pub async fn refresh(&self) -> Result<Option<AwsCredentials>> {
        if let Some(ref cmd) = self.credential_export_cmd {
            return self.export_credentials(cmd).await.map(Some);
        }

        if let Some(ref cmd) = self.auth_refresh_cmd {
            run_shell_command(cmd, "AWS auth refresh").await?;
        }

        Ok(None)
    }

    async fn export_credentials(&self, cmd: &str) -> Result<AwsCredentials> {
        let stdout = run_shell_command(cmd, "AWS credential export").await?;

        let json: serde_json::Value = serde_json::from_str(&stdout)
            .map_err(|e| Error::auth(format!("Invalid credential JSON: {}", e)))?;

        let creds = json
            .get("Credentials")
            .ok_or_else(|| Error::auth("Missing Credentials in response"))?;

        Ok(AwsCredentials {
            access_key_id: creds
                .get("AccessKeyId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::auth("Missing AccessKeyId"))?
                .to_string(),
            secret_access_key: SecretString::from(
                creds
                    .get("SecretAccessKey")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::auth("Missing SecretAccessKey"))?
                    .to_string(),
            ),
            session_token: creds
                .get("SessionToken")
                .and_then(|v| v.as_str())
                .map(|s| SecretString::from(s.to_string())),
        })
    }
}

impl Default for AwsCredentialRefresh {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    secret_access_key: SecretString,
    session_token: Option<SecretString>,
}

impl AwsCredentials {
    pub fn secret_access_key(&self) -> &str {
        self.secret_access_key.expose_secret()
    }

    pub fn session_token(&self) -> Option<&str> {
        self.session_token.as_ref().map(|s| s.expose_secret())
    }
}

impl fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AwsCredentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[redacted]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

#[derive(Debug)]
pub struct CredentialResolver {
    api_key_helper: Option<Arc<ApiKeyHelper>>,
    aws_refresh: Option<Arc<AwsCredentialRefresh>>,
}

impl CredentialResolver {
    pub fn new() -> Self {
        Self {
            api_key_helper: None,
            aws_refresh: None,
        }
    }

    pub fn api_key_helper(mut self, helper: ApiKeyHelper) -> Self {
        self.api_key_helper = Some(Arc::new(helper));
        self
    }

    pub fn aws_refresh(mut self, refresh: AwsCredentialRefresh) -> Self {
        self.aws_refresh = Some(Arc::new(refresh));
        self
    }

    pub async fn api_key(&self) -> Result<Option<SecretString>> {
        match &self.api_key_helper {
            Some(helper) => helper.key().await.map(Some),
            None => Ok(None),
        }
    }

    pub async fn refresh_aws(&self) -> Result<Option<AwsCredentials>> {
        match &self.aws_refresh {
            Some(refresh) => refresh.refresh().await,
            None => Ok(None),
        }
    }
}

impl Default for CredentialResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_api_key_helper_echo() {
        let helper = ApiKeyHelper::new("echo test-key");
        let key = helper.key().await.unwrap();
        assert_eq!(key.expose_secret(), "test-key");
    }

    #[tokio::test]
    async fn test_api_key_helper_caching() {
        let helper = ApiKeyHelper::new("echo test-key").ttl(Duration::from_secs(60));

        let key1 = helper.key().await.unwrap();
        let key2 = helper.key().await.unwrap();
        assert_eq!(key1.expose_secret(), key2.expose_secret());
    }

    #[tokio::test]
    async fn test_api_key_helper_failure() {
        let helper = ApiKeyHelper::new("exit 1");
        assert!(helper.key().await.is_err());
    }

    #[test]
    fn test_credential_manager_default() {
        let manager = CredentialResolver::default();
        assert!(manager.api_key_helper.is_none());
        assert!(manager.aws_refresh.is_none());
    }
}
