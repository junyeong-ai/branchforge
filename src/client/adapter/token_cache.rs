//! Shared authentication caching for cloud provider adapters.

use std::time::Duration;
#[cfg(any(feature = "gcp", feature = "azure"))]
use std::time::Instant;
#[cfg(feature = "aws")]
use std::time::SystemTime;

#[cfg(any(feature = "gcp", feature = "azure"))]
use secrecy::ExposeSecret;
use secrecy::SecretString;
use tokio::sync::RwLock;

const TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// GCP / Azure token cache
// ---------------------------------------------------------------------------

#[cfg(any(feature = "gcp", feature = "azure"))]
pub struct CachedToken {
    token: SecretString,
    expires_at: Instant,
}

#[cfg(any(feature = "gcp", feature = "azure"))]
impl std::fmt::Debug for CachedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedToken")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[cfg(any(feature = "gcp", feature = "azure"))]
impl CachedToken {
    pub fn new(token: String, ttl: Duration) -> Self {
        Self {
            token: SecretString::from(token),
            expires_at: Instant::now() + ttl - TOKEN_REFRESH_MARGIN,
        }
    }

    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }

    pub fn token(&self) -> &str {
        self.token.expose_secret()
    }
}

#[cfg(any(feature = "gcp", feature = "azure"))]
pub type TokenCache = RwLock<Option<CachedToken>>;

#[cfg(any(feature = "gcp", feature = "azure"))]
pub fn new_token_cache() -> TokenCache {
    RwLock::new(None)
}

// ---------------------------------------------------------------------------
// AWS credentials cache
// ---------------------------------------------------------------------------

#[cfg(feature = "aws")]
#[derive(Clone)]
pub struct CachedAwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: SecretString,
    pub session_token: Option<SecretString>,
    expiry: Option<SystemTime>,
}

#[cfg(feature = "aws")]
impl std::fmt::Debug for CachedAwsCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedAwsCredentials")
            .field("expiry", &self.expiry)
            .finish()
    }
}

#[cfg(feature = "aws")]
impl CachedAwsCredentials {
    pub fn new(
        access_key_id: String,
        secret_access_key: String,
        session_token: Option<String>,
        expiry: Option<SystemTime>,
    ) -> Self {
        Self {
            access_key_id,
            secret_access_key: SecretString::from(secret_access_key),
            session_token: session_token.map(SecretString::from),
            expiry,
        }
    }

    pub fn is_expired(&self) -> bool {
        self.expiry
            .map(|exp| SystemTime::now() + TOKEN_REFRESH_MARGIN > exp)
            .unwrap_or(false)
    }

    pub fn expiry(&self) -> Option<SystemTime> {
        self.expiry
    }
}

#[cfg(feature = "aws")]
pub type AwsCredentialsCache = RwLock<Option<CachedAwsCredentials>>;

#[cfg(feature = "aws")]
pub fn new_aws_credentials_cache() -> AwsCredentialsCache {
    RwLock::new(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(feature = "gcp", feature = "azure"))]
    fn test_cached_token_not_expired() {
        let token = CachedToken::new("test".into(), Duration::from_secs(3600));
        assert!(!token.is_expired());
        assert_eq!(token.token(), "test");
    }

    #[test]
    #[cfg(any(feature = "gcp", feature = "azure"))]
    fn test_cached_token_expired() {
        let token = CachedToken::new("test".into(), Duration::from_secs(0));
        assert!(token.is_expired());
    }
}
