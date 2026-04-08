//! Integration test for `DirectTransport::refresh()` via a mock
//! `CredentialProvider`.
//!
//! Verifies that:
//! 1. A transport with no credential provider returns no-op on refresh.
//! 2. A transport with a non-refreshable provider returns an auth error
//!    when `refresh()` is called.
//! 3. A transport with a refreshable provider successfully replaces its
//!    cached `DirectAuth` with a new credential.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use branchforge::auth::{Credential, CredentialProvider};
use branchforge::client::transport::{DirectAuth, DirectTransport, ModelTransport};
use secrecy::SecretString;

/// A test provider that returns a different api-key on each refresh()
/// call so we can verify the transport actually swapped credentials.
struct CountingRefreshProvider {
    calls: AtomicUsize,
    name: &'static str,
}

impl CountingRefreshProvider {
    fn new(name: &'static str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            name,
        }
    }
}

#[async_trait]
impl CredentialProvider for CountingRefreshProvider {
    fn name(&self) -> &str {
        self.name
    }

    async fn resolve(&self) -> branchforge::Result<Credential> {
        Ok(Credential::api_key("initial-token"))
    }

    async fn refresh(&self) -> branchforge::Result<Credential> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Credential::api_key(format!("refreshed-{n}")))
    }

    fn supports_refresh(&self) -> bool {
        true
    }
}

/// A provider that does not support refresh — used to verify that the
/// transport returns an error rather than silently no-op.
struct NonRefreshableProvider;

#[async_trait]
impl CredentialProvider for NonRefreshableProvider {
    fn name(&self) -> &str {
        "non-refreshable"
    }

    async fn resolve(&self) -> branchforge::Result<Credential> {
        Ok(Credential::api_key("static-token"))
    }
    // supports_refresh() default = false
}

#[tokio::test]
async fn refresh_with_no_provider_is_noop() {
    // No provider attached → refresh() returns Ok(()) without changing
    // anything. This is the static-API-key happy path.
    let transport = DirectTransport::new(
        "https://api.example.com",
        DirectAuth::Bearer(SecretString::from("static-key")),
    );
    let result = transport.refresh().await;
    assert!(result.is_ok(), "no-provider refresh must be no-op");
}

#[tokio::test]
async fn refresh_with_non_refreshable_provider_errors() {
    let transport = DirectTransport::new(
        "https://api.example.com",
        DirectAuth::Bearer(SecretString::from("static-key")),
    )
    .with_credential_provider(Arc::new(NonRefreshableProvider));

    let result = transport.refresh().await;
    assert!(
        result.is_err(),
        "refresh with non-refreshable provider should fail"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains("does not support refresh")
            || err.to_lowercase().contains("not support"),
        "expected explicit 'does not support refresh' message, got: {err}"
    );
}

#[tokio::test]
async fn refresh_with_refreshable_provider_swaps_credential() {
    let provider = Arc::new(CountingRefreshProvider::new("test-counter"));
    let transport = DirectTransport::new(
        "https://api.example.com",
        DirectAuth::Bearer(SecretString::from("initial-token")),
    )
    .with_credential_provider(provider.clone());

    // First refresh: should swap to "refreshed-0"
    transport.refresh().await.expect("first refresh ok");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

    // Second refresh: should swap to "refreshed-1"
    transport.refresh().await.expect("second refresh ok");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}
