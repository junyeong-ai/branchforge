//! `FoundryTransport` — Azure AI Foundry with Entra ID or api-key auth.
//!
//! Pairs with [`AnthropicMessagesCodec`](crate::client::codec::AnthropicMessagesCodec).
//! Foundry hosts the Anthropic Messages API at
//! `https://{resource}.services.ai.azure.com/anthropic/v1/messages` (or a
//! caller-provided base URL). Auth is either an `api-key` header or an
//! Entra ID bearer token.
//!
//! Feature-gated behind `azure` because of `azure_identity`.

#![cfg(feature = "azure")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use azure_identity::DeveloperToolsCredential;
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::RwLock;

use super::{Endpoint, ModelTransport};
use crate::client::codec::{EndpointShape, HeaderSource, InvocationMode};
use crate::{Error, Result};

const SCOPE: &str = "https://cognitiveservices.azure.com/.default";
const TOKEN_TTL: Duration = Duration::from_secs(3300);

/// Authentication scheme for Foundry.
enum FoundryAuth {
    /// Static `api-key: <key>` header.
    ApiKey(SecretString),
    /// Entra ID via the Azure DeveloperToolsCredential chain.
    Entra(Arc<DeveloperToolsCredential>),
}

impl std::fmt::Debug for FoundryAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey(_) => f.debug_tuple("ApiKey").field(&"[redacted]").finish(),
            Self::Entra(_) => f.debug_tuple("Entra").field(&"<credential>").finish(),
        }
    }
}

struct CachedToken {
    token: SecretString,
    expires_at: Instant,
}

impl CachedToken {
    fn new(token: String) -> Self {
        Self {
            token: SecretString::from(token),
            expires_at: Instant::now() + TOKEN_TTL,
        }
    }

    fn is_fresh(&self) -> bool {
        Instant::now() < self.expires_at
    }
}

/// Foundry transport.
pub struct FoundryTransport {
    base_url: String,
    auth: FoundryAuth,
    cached_token: RwLock<Option<CachedToken>>,
}

impl std::fmt::Debug for FoundryTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FoundryTransport")
            .field("base_url", &self.base_url)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl FoundryTransport {
    /// Construct from a fully-formed Foundry base URL (everything before
    /// `/v1/messages`).
    fn new(base_url: impl Into<String>, auth: FoundryAuth) -> Self {
        Self {
            base_url: trim_trailing_slash(base_url.into()),
            auth,
            cached_token: RwLock::new(None),
        }
    }

    /// Construct from environment: `AZURE_AI_BASE_URL` (or
    /// `AZURE_AI_RESOURCE` to derive `https://{resource}.services.ai.azure.com/anthropic`).
    /// Auth: `AZURE_AI_API_KEY` if set, otherwise the Entra ID
    /// `DeveloperToolsCredential` chain.
    pub fn from_env() -> Result<Self> {
        let base_url = if let Ok(url) = std::env::var("AZURE_AI_BASE_URL") {
            url
        } else if let Ok(resource) = std::env::var("AZURE_AI_RESOURCE") {
            format!("https://{resource}.services.ai.azure.com/anthropic")
        } else {
            return Err(Error::Config(
                "foundry: set AZURE_AI_BASE_URL or AZURE_AI_RESOURCE".into(),
            ));
        };

        let auth = if let Ok(key) = std::env::var("AZURE_AI_API_KEY") {
            FoundryAuth::ApiKey(SecretString::from(key))
        } else {
            let cred = DeveloperToolsCredential::new(None).map_err(|e| {
                Error::auth(format!("foundry: failed to build Entra credential: {e}"))
            })?;
            FoundryAuth::Entra(cred)
        };

        Ok(Self::new(base_url, auth))
    }

    async fn fetch_token(&self) -> Result<String> {
        {
            let cache = self.cached_token.read().await;
            if let Some(t) = cache.as_ref()
                && t.is_fresh()
            {
                return Ok(t.token.expose_secret().to_string());
            }
        }
        let cred = match &self.auth {
            FoundryAuth::Entra(c) => Arc::clone(c),
            FoundryAuth::ApiKey(_) => {
                return Err(Error::auth("foundry: api-key mode does not use tokens"));
            }
        };
        let token = cred
            .get_token(&[SCOPE], None)
            .await
            .map_err(|e| Error::auth(format!("foundry: token fetch failed: {e}")))?;
        let token_str = token.token.secret().to_string();
        *self.cached_token.write().await = Some(CachedToken::new(token_str.clone()));
        Ok(token_str)
    }
}

fn trim_trailing_slash(mut s: String) -> String {
    while s.ends_with('/') {
        s.pop();
    }
    s
}

#[async_trait]
impl ModelTransport for FoundryTransport {
    fn id(&self) -> &'static str {
        "foundry"
    }

    fn supports_codec(&self, codec_id: &str) -> bool {
        // Foundry's Anthropic endpoint speaks the standard Messages API.
        codec_id == "anthropic-messages"
    }

    async fn resolve_endpoint(
        &self,
        shape: &EndpointShape,
        _model: &str,
        _mode: InvocationMode,
    ) -> Result<Endpoint> {
        // Anthropic Messages: path_template = "v1/messages", no verbs.
        let url = format!(
            "{}/{}",
            self.base_url,
            shape.path_template.trim_start_matches('/')
        );
        let mut headers = Vec::with_capacity(shape.required_headers.len() + 1);
        for h in shape.required_headers {
            let value = match h.source {
                HeaderSource::Literal(s) => s.to_string(),
                HeaderSource::ContextValue(key) => {
                    return Err(Error::Config(format!(
                        "foundry transport has no context value for '{key}'"
                    )));
                }
            };
            headers.push((h.name.to_string(), value));
        }
        headers.push(("content-type".to_string(), "application/json".to_string()));
        Ok(Endpoint { url, headers })
    }

    async fn authorize(
        &self,
        req: reqwest::RequestBuilder,
        _body_bytes: &[u8],
    ) -> Result<reqwest::RequestBuilder> {
        match &self.auth {
            FoundryAuth::ApiKey(key) => Ok(req.header("api-key", key.expose_secret())),
            FoundryAuth::Entra(_) => {
                let token = self.fetch_token().await?;
                Ok(req.header("authorization", format!("Bearer {token}")))
            }
        }
    }

    async fn refresh(&self) -> Result<()> {
        *self.cached_token.write().await = None;
        Ok(())
    }
}

/// Constructor helpers exposed for the preset layer and tests.
impl FoundryTransport {
    pub fn with_api_key(base_url: impl Into<String>, key: impl Into<String>) -> Self {
        Self::new(
            base_url,
            FoundryAuth::ApiKey(SecretString::from(key.into())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::codec::{AnthropicMessagesCodec, ModelCodec};

    #[tokio::test]
    async fn anthropic_messages_url_appended_to_base() {
        let t = FoundryTransport::with_api_key(
            "https://my-resource.services.ai.azure.com/anthropic",
            "k",
        );
        let codec = AnthropicMessagesCodec::new();
        let ep = t
            .resolve_endpoint(
                codec.endpoint_shape(),
                "claude-sonnet-4-5",
                InvocationMode::Unary,
            )
            .await
            .unwrap();
        assert_eq!(
            ep.url,
            "https://my-resource.services.ai.azure.com/anthropic/v1/messages"
        );
        assert!(
            ep.headers
                .iter()
                .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01")
        );
    }

    #[tokio::test]
    async fn supports_codec_only_anthropic_messages() {
        let t = FoundryTransport::with_api_key("https://x", "k");
        assert!(t.supports_codec("anthropic-messages"));
        assert!(!t.supports_codec("openai-chat"));
        assert!(!t.supports_codec("gemini-generate"));
        assert!(!t.supports_codec("bedrock-converse"));
    }

    #[tokio::test]
    async fn api_key_authorize_sets_header() {
        let t = FoundryTransport::with_api_key("https://x", "secret");
        let req = reqwest::Client::new().post("https://x/v1/messages");
        let built = t.authorize(req, b"{}").await.unwrap().build().unwrap();
        assert_eq!(built.headers().get("api-key").unwrap(), "secret");
    }

    #[tokio::test]
    async fn trim_trailing_slashes() {
        let t = FoundryTransport::with_api_key("https://x.example.com/anthropic///", "k");
        let codec = AnthropicMessagesCodec::new();
        let ep = t
            .resolve_endpoint(codec.endpoint_shape(), "x", InvocationMode::Unary)
            .await
            .unwrap();
        assert_eq!(ep.url, "https://x.example.com/anthropic/v1/messages");
    }
}
