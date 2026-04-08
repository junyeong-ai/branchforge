//! `VertexTransport` — GCP Vertex AI with publisher routing.
//!
//! Vertex hosts both Anthropic and Google publisher models on the same
//! `{region}-aiplatform.googleapis.com` host. The publisher is derived
//! from the codec id at endpoint resolution time:
//!
//! - `anthropic-messages` → `publishers/anthropic/models/{model}:rawPredict`
//! - `gemini-generate`    → `publishers/google/models/{model}:generateContent`
//!
//! This is the **single** transport that powers both `vertex-anthropic`
//! and the headline `vertex-gemini` preset, paired with the appropriate
//! codec. The codec axis carries the wire format; this transport carries
//! the auth, the host, and the publisher routing.
//!
//! Authentication uses GCP Application Default Credentials via the
//! [`gcp_auth`] crate. The transport injects `x-goog-user-project` (the
//! quota project header) automatically — this is what unblocked the
//! `oy-gemini-enterprise-prd` environment in the live verification.
//!
//! Feature-gated behind `gcp` because of the `gcp_auth` dependency.

#![cfg(feature = "gcp")]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gcp_auth::TokenProvider;
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::RwLock;

use super::{Endpoint, ModelTransport, append_stream_query};
use crate::client::codec::{ApiVersionHint, EndpointShape, HeaderSource, InvocationMode};
use crate::{Error, Result};

const SCOPES: &[&str] = &["https://www.googleapis.com/auth/cloud-platform"];
const TOKEN_TTL: Duration = Duration::from_secs(3300); // ~55 minutes

/// Vertex transport.
///
/// Pairs with [`AnthropicMessagesCodec`](crate::client::codec::AnthropicMessagesCodec)
/// or [`GeminiGenerateCodec`](crate::client::codec::GeminiGenerateCodec).
/// The publisher (`anthropic` or `google`) is derived from
/// `codec.id()` via [`publisher_for_codec`].
pub struct VertexTransport {
    project_id: String,
    location: String,
    quota_project: Option<String>,
    /// `true` if `location == "global"`. Some Anthropic models on Vertex
    /// only resolve through `aiplatform.googleapis.com` (no region prefix).
    use_global_host: bool,
    token_provider: Arc<dyn TokenProvider>,
    cached_token: RwLock<Option<CachedToken>>,
}

impl std::fmt::Debug for VertexTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexTransport")
            .field("project_id", &self.project_id)
            .field("location", &self.location)
            .field("quota_project", &self.quota_project)
            .field("use_global_host", &self.use_global_host)
            .finish_non_exhaustive()
    }
}

struct CachedToken {
    token: SecretString,
    expires_at: std::time::Instant,
}

impl CachedToken {
    fn new(token: String) -> Self {
        Self {
            token: SecretString::from(token),
            expires_at: std::time::Instant::now() + TOKEN_TTL,
        }
    }

    fn is_fresh(&self) -> bool {
        std::time::Instant::now() < self.expires_at
    }
}

impl VertexTransport {
    /// Construct a `VertexTransport`. Resolves ADC credentials immediately
    /// so that misconfiguration surfaces at construction time rather than
    /// on the first request.
    pub async fn new(project_id: impl Into<String>, location: impl Into<String>) -> Result<Self> {
        let token_provider = gcp_auth::provider()
            .await
            .map_err(|e| Error::auth(format!("vertex: failed to acquire ADC: {e}")))?;
        let location = location.into();
        let use_global_host = location == "global";
        Ok(Self {
            project_id: project_id.into(),
            location,
            quota_project: None,
            use_global_host,
            token_provider,
            cached_token: RwLock::new(None),
        })
    }

    /// Construct from environment variables: `GOOGLE_CLOUD_PROJECT`,
    /// `GOOGLE_CLOUD_LOCATION` (or `GOOGLE_CLOUD_REGION` / `CLOUD_ML_REGION`),
    /// `GOOGLE_CLOUD_QUOTA_PROJECT`.
    pub async fn from_env() -> Result<Self> {
        let project = std::env::var("GOOGLE_CLOUD_PROJECT")
            .or_else(|_| std::env::var("GCLOUD_PROJECT"))
            .map_err(|_| {
                Error::Config(
                    "vertex: GOOGLE_CLOUD_PROJECT not set; required to resolve project id".into(),
                )
            })?;
        let location = std::env::var("GOOGLE_CLOUD_LOCATION")
            .or_else(|_| std::env::var("GOOGLE_CLOUD_REGION"))
            .or_else(|_| std::env::var("CLOUD_ML_REGION"))
            .unwrap_or_else(|_| "us-central1".into());
        let mut t = Self::new(project.clone(), location).await?;
        if let Ok(qp) = std::env::var("GOOGLE_CLOUD_QUOTA_PROJECT") {
            t.quota_project = Some(qp);
        } else {
            t.quota_project = Some(project);
        }
        Ok(t)
    }

    /// Override the quota project (`x-goog-user-project` header). Defaults
    /// to the project id, which is the right behaviour for almost every
    /// real-world Vertex setup.
    pub fn with_quota_project(mut self, qp: impl Into<String>) -> Self {
        self.quota_project = Some(qp.into());
        self
    }

    /// Force the `global` location instead of the configured region.
    pub fn with_global_host(mut self) -> Self {
        self.use_global_host = true;
        self
    }

    fn host(&self) -> String {
        if self.use_global_host {
            "aiplatform.googleapis.com".to_string()
        } else {
            format!("{}-aiplatform.googleapis.com", self.location)
        }
    }

    fn url_path(&self, codec_id: &str, model: &str, verb: &str, hint: ApiVersionHint) -> String {
        let publisher = publisher_for_codec(codec_id).unwrap_or("anthropic");
        let api_version = match hint {
            ApiVersionHint::PinnedTo(v) => v,
            ApiVersionHint::Beta => "v1beta1",
            ApiVersionHint::Stable => "v1",
        };
        let location = if self.use_global_host {
            "global"
        } else {
            self.location.as_str()
        };
        if verb.is_empty() {
            format!(
                "{api_version}/projects/{}/locations/{location}/publishers/{publisher}/models/{model}",
                self.project_id
            )
        } else {
            format!(
                "{api_version}/projects/{}/locations/{location}/publishers/{publisher}/models/{model}:{verb}",
                self.project_id
            )
        }
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
        let token = self
            .token_provider
            .token(SCOPES)
            .await
            .map_err(|e| Error::auth(format!("vertex: token fetch failed: {e}")))?;
        let token_str = token.as_str().to_string();
        *self.cached_token.write().await = Some(CachedToken::new(token_str.clone()));
        Ok(token_str)
    }
}

/// Map a codec id to the Vertex `publishers/<name>` URL segment.
///
/// Returns `None` for codecs that cannot be hosted on Vertex (e.g.
/// `openai-chat`); the transport's [`ModelTransport::supports_codec`]
/// uses this to reject incompatible compositions at builder time.
pub fn publisher_for_codec(codec_id: &str) -> Option<&'static str> {
    match codec_id {
        "anthropic-messages" => Some("anthropic"),
        "gemini-generate" => Some("google"),
        _ => None,
    }
}

#[async_trait]
impl ModelTransport for VertexTransport {
    fn id(&self) -> &'static str {
        "vertex"
    }

    fn supports_codec(&self, codec_id: &str) -> bool {
        publisher_for_codec(codec_id).is_some()
    }

    async fn resolve_endpoint(
        &self,
        shape: &EndpointShape,
        model: &str,
        mode: InvocationMode,
    ) -> Result<Endpoint> {
        // The codec id is now declared explicitly on the `EndpointShape`,
        // so the transport routes directly without inspecting the path
        // template. This removes the historical fragile heuristic that
        // would silently misroute any future codec whose path template
        // happened to contain `{model}`.
        let codec_id = shape.codec_id;

        let verb = match (codec_id, mode) {
            // Anthropic-on-Vertex always uses rawPredict / streamRawPredict
            // regardless of the codec's verb_unary (which is empty for the
            // direct Anthropic API).
            ("anthropic-messages", InvocationMode::Stream) => "streamRawPredict",
            ("anthropic-messages", _) => "rawPredict",
            (_, InvocationMode::Stream) => shape.verb_stream,
            (_, _) => shape.verb_unary,
        };

        let path = self.url_path(codec_id, model, verb, shape.api_version_hint);
        let mut url = format!("https://{}/{}", self.host(), path);
        append_stream_query(&mut url, shape, mode);

        // Resolve required headers from the shape.
        let mut headers = Vec::with_capacity(shape.required_headers.len() + 2);
        for h in shape.required_headers {
            let value = match h.source {
                HeaderSource::Literal(s) => s.to_string(),
                HeaderSource::ContextValue(key) => match key {
                    "quota_project" => self.quota_project.clone().unwrap_or_default(),
                    "project_id" => self.project_id.clone(),
                    "location" => self.location.clone(),
                    other => {
                        return Err(Error::Config(format!(
                            "vertex transport has no context value for '{other}'",
                        )));
                    }
                },
            };
            headers.push((h.name.to_string(), value));
        }
        // Always set the quota project header — this is the fix for the
        // user's `oy-gemini-enterprise-prd` 403.
        if let Some(qp) = &self.quota_project {
            headers.push(("x-goog-user-project".to_string(), qp.clone()));
        }
        headers.push(("content-type".to_string(), "application/json".to_string()));

        Ok(Endpoint { url, headers })
    }

    async fn authorize(
        &self,
        req: reqwest::RequestBuilder,
        _body_bytes: &[u8],
    ) -> Result<reqwest::RequestBuilder> {
        let token = self.fetch_token().await?;
        Ok(req.header("authorization", format!("Bearer {token}")))
    }

    async fn refresh(&self) -> Result<()> {
        // Force a re-fetch on the next request.
        *self.cached_token.write().await = None;
        Ok(())
    }

    fn classify_error(
        &self,
        status: u16,
        body: &str,
    ) -> (crate::error::ProviderErrorKind, Option<&'static str>) {
        use crate::error::ProviderErrorKind;
        match status {
            401 | 403 if body.contains("quota") || body.contains("user-project") => (
                ProviderErrorKind::Quota,
                Some(
                    "Set GOOGLE_CLOUD_QUOTA_PROJECT or pass VertexTransport::with_quota_project(...). \
                     If using gcloud creds, run: gcloud auth application-default set-quota-project <project>",
                ),
            ),
            401 | 403 => (
                ProviderErrorKind::Auth,
                Some("Vertex authentication failed. Run: gcloud auth application-default login"),
            ),
            404 if body.contains("Publisher Model") || body.contains("publisher") => (
                ProviderErrorKind::BadRequest,
                Some(
                    "Model not enabled in this project. Enable it in the Vertex AI Model Garden for the publisher.",
                ),
            ),
            429 => (ProviderErrorKind::RateLimit, None),
            500..=599 => (ProviderErrorKind::Server, None),
            400..=499 => (ProviderErrorKind::BadRequest, None),
            _ => (ProviderErrorKind::Server, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::codec::{AnthropicMessagesCodec, GeminiGenerateCodec, ModelCodec};

    #[test]
    fn publisher_routing_table() {
        assert_eq!(publisher_for_codec("anthropic-messages"), Some("anthropic"));
        assert_eq!(publisher_for_codec("gemini-generate"), Some("google"));
        assert_eq!(publisher_for_codec("openai-chat"), None);
        assert_eq!(publisher_for_codec("bedrock-converse"), None);
    }

    /// We can't actually construct a VertexTransport in tests without ADC,
    /// but we can verify the URL builder by constructing one with a fake
    /// token provider via the helper below.
    fn fake_transport(location: &str) -> VertexTransport {
        struct FakeProvider;
        #[async_trait]
        impl TokenProvider for FakeProvider {
            async fn token(
                &self,
                _scopes: &[&str],
            ) -> std::result::Result<Arc<gcp_auth::Token>, gcp_auth::Error> {
                // We never call this in URL-only tests.
                unreachable!("fake provider should not be invoked in URL tests")
            }
            async fn project_id(&self) -> std::result::Result<Arc<str>, gcp_auth::Error> {
                Ok(Arc::from("fake-project"))
            }
        }

        let use_global = location == "global";
        VertexTransport {
            project_id: "oy-gemini-enterprise-prd".into(),
            location: location.to_string(),
            quota_project: Some("oy-gemini-enterprise-prd".into()),
            use_global_host: use_global,
            token_provider: Arc::new(FakeProvider),
            cached_token: RwLock::new(None),
        }
    }

    #[tokio::test]
    async fn vertex_gemini_url_us_central1_streaming() {
        let t = fake_transport("us-central1");
        let codec = GeminiGenerateCodec::new();
        let ep = t
            .resolve_endpoint(
                codec.endpoint_shape(),
                "gemini-2.5-flash",
                InvocationMode::Stream,
            )
            .await
            .unwrap();
        assert_eq!(
            ep.url,
            "https://us-central1-aiplatform.googleapis.com/v1beta1/projects/oy-gemini-enterprise-prd/locations/us-central1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert!(
            ep.headers
                .iter()
                .any(|(k, v)| k == "x-goog-user-project" && v == "oy-gemini-enterprise-prd")
        );
    }

    #[tokio::test]
    async fn vertex_gemini_url_us_central1_unary() {
        let t = fake_transport("us-central1");
        let codec = GeminiGenerateCodec::new();
        let ep = t
            .resolve_endpoint(
                codec.endpoint_shape(),
                "gemini-2.5-flash",
                InvocationMode::Unary,
            )
            .await
            .unwrap();
        assert_eq!(
            ep.url,
            "https://us-central1-aiplatform.googleapis.com/v1beta1/projects/oy-gemini-enterprise-prd/locations/us-central1/publishers/google/models/gemini-2.5-flash:generateContent"
        );
        assert!(!ep.url.contains("alt=sse"));
    }

    #[tokio::test]
    async fn vertex_anthropic_url_us_east5_uses_raw_predict() {
        let t = fake_transport("us-east5");
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
            "https://us-east5-aiplatform.googleapis.com/v1/projects/oy-gemini-enterprise-prd/locations/us-east5/publishers/anthropic/models/claude-sonnet-4-5:rawPredict"
        );
    }

    #[tokio::test]
    async fn vertex_anthropic_streaming_uses_stream_raw_predict() {
        let t = fake_transport("us-east5");
        let codec = AnthropicMessagesCodec::new();
        let ep = t
            .resolve_endpoint(
                codec.endpoint_shape(),
                "claude-sonnet-4-5",
                InvocationMode::Stream,
            )
            .await
            .unwrap();
        assert!(ep.url.ends_with(":streamRawPredict"));
    }

    #[tokio::test]
    async fn vertex_global_host_for_anthropic() {
        let t = fake_transport("global");
        let codec = AnthropicMessagesCodec::new();
        let ep = t
            .resolve_endpoint(
                codec.endpoint_shape(),
                "claude-opus-4-6",
                InvocationMode::Unary,
            )
            .await
            .unwrap();
        assert!(
            ep.url
                .starts_with("https://aiplatform.googleapis.com/v1/projects/")
        );
        assert!(ep.url.contains("/locations/global/"));
    }

    #[tokio::test]
    async fn vertex_quota_project_header_always_present() {
        let t = fake_transport("us-central1");
        let codec = GeminiGenerateCodec::new();
        let ep = t
            .resolve_endpoint(
                codec.endpoint_shape(),
                "gemini-2.5-flash",
                InvocationMode::Unary,
            )
            .await
            .unwrap();
        let qp = ep
            .headers
            .iter()
            .find(|(k, _)| k == "x-goog-user-project")
            .map(|(_, v)| v.as_str());
        assert_eq!(qp, Some("oy-gemini-enterprise-prd"));
    }

    #[tokio::test]
    async fn vertex_supports_codec_filter() {
        let t = fake_transport("us-central1");
        assert!(t.supports_codec("anthropic-messages"));
        assert!(t.supports_codec("gemini-generate"));
        assert!(!t.supports_codec("openai-chat"));
        assert!(!t.supports_codec("bedrock-converse"));
    }
}
