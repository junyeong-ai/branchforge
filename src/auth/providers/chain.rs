//! Chain credential provider.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

#[cfg(feature = "cli-auth")]
use crate::auth::ClaudeCliProvider;
use crate::auth::{Credential, CredentialProvider, EnvironmentProvider};
use crate::{Error, Result};

pub struct ChainProvider {
    providers: Vec<Arc<dyn CredentialProvider>>,
    last_successful: RwLock<Option<Arc<dyn CredentialProvider>>>,
}

impl ChainProvider {
    pub fn new(providers: Vec<Box<dyn CredentialProvider>>) -> Self {
        Self {
            providers: providers.into_iter().map(Arc::from).collect(),
            last_successful: RwLock::new(None),
        }
    }

    pub fn provider<P: CredentialProvider + 'static>(mut self, provider: P) -> Self {
        self.providers.push(Arc::new(provider));
        self
    }
}

#[cfg(feature = "cli-auth")]
impl Default for ChainProvider {
    fn default() -> Self {
        Self {
            providers: vec![
                Arc::new(EnvironmentProvider::new()),
                Arc::new(ClaudeCliProvider::new()),
            ],
            last_successful: RwLock::new(None),
        }
    }
}

#[cfg(not(feature = "cli-auth"))]
impl Default for ChainProvider {
    fn default() -> Self {
        Self {
            providers: vec![Arc::new(EnvironmentProvider::new())],
            last_successful: RwLock::new(None),
        }
    }
}

#[async_trait]
impl CredentialProvider for ChainProvider {
    fn name(&self) -> &str {
        "chain"
    }

    async fn resolve(&self) -> Result<Credential> {
        let mut errors = Vec::new();

        for provider in &self.providers {
            match provider.resolve().await {
                Ok(cred) => {
                    tracing::debug!("Credential resolved from: {}", provider.name());
                    *self.last_successful.write().await = Some(Arc::clone(provider));
                    return Ok(cred);
                }
                Err(e) => {
                    tracing::debug!("Provider {} failed: {}", provider.name(), e);
                    errors.push(format!("{}: {}", provider.name(), e));
                }
            }
        }

        Err(Error::auth(format!(
            "No credentials found. Tried: {}",
            errors.join(", ")
        )))
    }

    async fn refresh(&self) -> Result<Credential> {
        let provider = self.last_successful.read().await;
        match provider.as_ref() {
            Some(p) if p.supports_refresh() => p.refresh().await,
            Some(_) => Err(Error::auth(
                "Last successful provider does not support refresh",
            )),
            None => Err(Error::auth("No provider has successfully resolved yet")),
        }
    }

    fn supports_refresh(&self) -> bool {
        self.last_successful
            .try_read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|p| p.supports_refresh()))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ExplicitProvider;
    use secrecy::ExposeSecret;

    #[tokio::test]
    async fn test_chain_first_success() {
        let chain = ChainProvider::new(vec![])
            .provider(ExplicitProvider::api_key("first"))
            .provider(ExplicitProvider::api_key("second"));

        let cred = chain.resolve().await.unwrap();
        assert!(matches!(&cred, Credential::ApiKey(k) if k.expose_secret() == "first"));
    }

    #[tokio::test]
    async fn test_chain_fallback() {
        let chain = ChainProvider::new(vec![])
            .provider(EnvironmentProvider::from_var("NONEXISTENT_VAR"))
            .provider(ExplicitProvider::api_key("fallback"));

        let cred = chain.resolve().await.unwrap();
        assert!(matches!(&cred, Credential::ApiKey(k) if k.expose_secret() == "fallback"));
    }

    #[tokio::test]
    async fn test_chain_all_fail() {
        let chain = ChainProvider::new(vec![])
            .provider(EnvironmentProvider::from_var("NONEXISTENT_VAR_1"))
            .provider(EnvironmentProvider::from_var("NONEXISTENT_VAR_2"));

        assert!(chain.resolve().await.is_err());
    }

    struct RefreshableProvider;

    #[async_trait]
    impl CredentialProvider for RefreshableProvider {
        fn name(&self) -> &str {
            "refreshable"
        }

        async fn resolve(&self) -> Result<Credential> {
            Ok(Credential::api_key("refreshable-key"))
        }

        fn supports_refresh(&self) -> bool {
            true
        }

        async fn refresh(&self) -> Result<Credential> {
            Ok(Credential::api_key("refreshed-key"))
        }
    }

    #[tokio::test]
    async fn test_supports_refresh_after_resolve() {
        let chain = ChainProvider::new(vec![]).provider(RefreshableProvider);

        assert!(!chain.supports_refresh());

        let _ = chain.resolve().await.unwrap();

        assert!(chain.supports_refresh());
    }

    #[tokio::test]
    async fn test_supports_refresh_with_non_refreshable() {
        let chain = ChainProvider::new(vec![]).provider(ExplicitProvider::api_key("key"));

        let _ = chain.resolve().await.unwrap();

        assert!(!chain.supports_refresh());
    }

    #[tokio::test]
    async fn test_chain_tracks_last_successful() {
        let chain = ChainProvider::new(vec![])
            .provider(EnvironmentProvider::from_var("NONEXISTENT_VAR"))
            .provider(ExplicitProvider::api_key("fallback"));

        let _ = chain.resolve().await.unwrap();

        let last = chain.last_successful.read().await;
        assert!(last.is_some());
        assert_eq!(last.as_ref().unwrap().name(), "explicit");
    }
}
