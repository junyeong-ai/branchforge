//! `DirectTransport` — direct HTTPS to a provider's public API endpoint
//! using API key or bearer token authentication.
//!
//! Used by `anthropic`, `openai`, `openai-chat`, and `gemini` presets, plus
//! any OpenAI-compatible third party (`base_url` override).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::RwLock;

use super::{Endpoint, ModelTransport, append_stream_query, render_path};
use crate::Result;
use crate::client::codec::{EndpointShape, HeaderSource, InvocationMode};

/// Authentication scheme for [`DirectTransport`].
#[non_exhaustive]
#[derive(Clone)]
pub enum DirectAuth {
    /// `x-api-key: <key>`. Used by Anthropic Direct.
    XApiKey(SecretString),
    /// `Authorization: Bearer <token>`. Used by OpenAI, Gemini OAuth.
    Bearer(SecretString),
    /// Gemini-style `?key=<key>` URL query parameter. Stored on the transport
    /// because it must be appended to the URL during endpoint resolution
    /// rather than as a header.
    QueryParam {
        param: &'static str,
        value: SecretString,
    },
    /// No authentication. First-class case for local providers
    /// (Ollama, llama.cpp, vLLM, custom self-hosted gateways) where
    /// the server does not require credentials. The transport
    /// short-circuits `authorize` and adds no auth header or query
    /// parameter.
    None,
}

impl std::fmt::Debug for DirectAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::XApiKey(_) => f.debug_tuple("XApiKey").field(&"[redacted]").finish(),
            Self::Bearer(_) => f.debug_tuple("Bearer").field(&"[redacted]").finish(),
            Self::QueryParam { param, .. } => f
                .debug_struct("QueryParam")
                .field("param", param)
                .field("value", &"[redacted]")
                .finish(),
            Self::None => f.write_str("None"),
        }
    }
}

/// Direct HTTPS transport.
///
/// Constructs URLs as `{base_url}/{rendered path}[?query]`. Adds the
/// codec's `required_headers` (resolving `ContextValue` placeholders from
/// the transport's `Self::context` map) and applies the configured
/// [`DirectAuth`] in [`Self::authorize`].
///
/// # Credential refresh
///
/// When `credential_provider` is set, [`ModelTransport::refresh`] asks
/// the provider for a fresh credential and rewrites the cached
/// [`DirectAuth`]. This is what makes Bearer-token presets (Anthropic
/// CLI OAuth, custom OAuth2 deployments) recover from `401 Unauthorized`
/// without forcing the caller to rebuild the transport.
pub struct DirectTransport {
    base_url: String,
    auth: RwLock<DirectAuth>,
    /// Substitutions for `HeaderSource::ContextValue(key)` placeholders in
    /// codec endpoint shapes (e.g. OpenAI `OPENAI_ORG_ID`).
    context: HashMap<&'static str, String>,
    /// Codec ids this transport explicitly supports. If empty, every codec
    /// is accepted (the common case for direct HTTPS).
    allowed_codecs: Arc<[&'static str]>,
    /// Optional credential provider used by [`ModelTransport::refresh`] to
    /// pull a fresh credential after a `401`. Only meaningful when the
    /// underlying provider supports `refresh()` — for static API keys this
    /// stays `None`.
    credential_provider: Option<Arc<dyn crate::auth::CredentialProvider>>,
    /// Static headers added to every request. Used by Claude Code OAuth
    /// to inject `user-agent`, `x-app`, `anthropic-beta`, and the
    /// `anthropic-dangerous-direct-browser-access` flag.
    extra_headers: HashMap<String, String>,
    /// Static URL query parameters appended to every request. Used by
    /// Claude Code OAuth (`?beta=true`).
    extra_url_params: HashMap<String, String>,
}

impl std::fmt::Debug for DirectTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectTransport")
            .field("base_url", &self.base_url)
            .field("allowed_codecs", &self.allowed_codecs)
            .finish()
    }
}

impl DirectTransport {
    /// Construct a `DirectTransport` for `base_url` with the given auth.
    ///
    /// `base_url` should not have a trailing slash. The codec's
    /// [`EndpointShape::path_template`] is appended directly.
    pub fn new(base_url: impl Into<String>, auth: DirectAuth) -> Self {
        Self {
            base_url: trim_trailing_slash(base_url.into()),
            auth: RwLock::new(auth),
            context: HashMap::new(),
            allowed_codecs: Arc::from([] as [&'static str; 0]),
            credential_provider: None,
            extra_headers: HashMap::new(),
            extra_url_params: HashMap::new(),
        }
    }

    /// Restrict this transport to a specific set of codecs. Used by the
    /// preset layer to enforce e.g. "Direct(api.anthropic.com) only carries
    /// the Anthropic Messages codec".
    pub fn with_allowed_codecs(mut self, codecs: &[&'static str]) -> Self {
        self.allowed_codecs = Arc::from(codecs);
        self
    }

    /// Set a context value used to substitute `{key}` placeholders in
    /// codec path templates and `HeaderSource::ContextValue(key)` headers.
    pub fn with_context(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.context.insert(key, value.into());
        self
    }

    /// Attach a credential provider so [`ModelTransport::refresh`] can
    /// fetch a fresh credential after a `401 Unauthorized`. The provider
    /// must implement [`crate::auth::CredentialProvider::supports_refresh`]
    /// and return `true`; otherwise refresh will fail with an auth error.
    pub fn with_credential_provider(
        mut self,
        provider: Arc<dyn crate::auth::CredentialProvider>,
    ) -> Self {
        self.credential_provider = Some(provider);
        self
    }

    /// Attach static headers added to every request. Existing context
    /// headers and the `Authorization` header are merged on top — the
    /// `extra_headers` map is best used for vendor-specific OAuth flags
    /// like `user-agent`, `x-app`, and `anthropic-beta` (Claude Code).
    pub fn with_extra_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.extra_headers = headers;
        self
    }

    /// Attach static URL query parameters appended to every request URL.
    /// Used by Claude Code OAuth (`?beta=true`); the QueryParam auth path
    /// is unaffected (it appends its own `?key=` separately).
    pub fn with_extra_url_params(mut self, params: HashMap<String, String>) -> Self {
        self.extra_url_params = params;
        self
    }

    /// Replace the auth scheme. Used by credential refresh.
    pub async fn set_auth(&self, auth: DirectAuth) {
        *self.auth.write().await = auth;
    }

    /// Internal helper: convert a freshly-resolved [`crate::auth::Credential`]
    /// into the equivalent [`DirectAuth`] variant. Bearer tokens and API
    /// keys are mapped one-to-one; query-param auth is unchanged because
    /// we cannot infer which query parameter the new credential should
    /// use without more context.
    fn credential_to_auth(
        current: &DirectAuth,
        credential: crate::auth::Credential,
    ) -> Option<DirectAuth> {
        use crate::auth::Credential as Cred;
        match (current, credential) {
            (DirectAuth::XApiKey(_), Cred::ApiKey(secret)) => Some(DirectAuth::XApiKey(secret)),
            (DirectAuth::Bearer(_), Cred::OAuth(oauth)) => {
                Some(DirectAuth::Bearer(oauth.access_token))
            }
            (DirectAuth::Bearer(_), Cred::ApiKey(secret)) => Some(DirectAuth::Bearer(secret)),
            // Query-param auth retains the original parameter name; the
            // refreshed credential just rewrites the secret value.
            (DirectAuth::QueryParam { param, .. }, Cred::ApiKey(secret)) => {
                Some(DirectAuth::QueryParam {
                    param,
                    value: secret,
                })
            }
            _ => None,
        }
    }
}

fn trim_trailing_slash(mut s: String) -> String {
    while s.ends_with('/') {
        s.pop();
    }
    s
}

/// Phase C-6: decode a [`crate::ir::RateLimitSnapshot`] from the
/// response headers of a `DirectTransport` call.
///
/// Reads both Anthropic and OpenAI header conventions in one pass
/// — the direct transport is shared across all three presets, and
/// discriminating by preset would require plumbing the codec id
/// into the call. Cheaper to try both and take whatever the server
/// published.
///
/// Returns `None` when no recognisable rate-limit header is present,
/// so the caller can tell "no snapshot" apart from "all zeros".
fn parse_direct_rate_limit(
    headers: &reqwest::header::HeaderMap,
) -> Option<crate::ir::RateLimitSnapshot> {
    use chrono::{DateTime, Utc};

    fn get_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
        headers.get(name)?.to_str().ok()?.parse().ok()
    }

    fn get_reset(headers: &reqwest::header::HeaderMap, name: &str) -> Option<DateTime<Utc>> {
        let raw = headers.get(name)?.to_str().ok()?;
        // Anthropic: ISO 8601 absolute timestamp.
        if let Ok(ts) = DateTime::parse_from_rfc3339(raw) {
            return Some(ts.with_timezone(&Utc));
        }
        // OpenAI: duration string like "1s" / "6m0s" / "1h3m2s".
        // Parse naively by scanning digits + unit letters.
        let mut total = 0i64;
        let mut buf = String::new();
        for ch in raw.chars() {
            if ch.is_ascii_digit() || ch == '.' {
                buf.push(ch);
            } else {
                let n: f64 = buf.parse().ok()?;
                total += match ch {
                    'h' => (n * 3600.0) as i64,
                    'm' => (n * 60.0) as i64,
                    's' => n as i64,
                    'd' => (n * 86400.0) as i64,
                    _ => return None,
                };
                buf.clear();
            }
        }
        if total == 0 && buf.is_empty() {
            return None;
        }
        // Trailing bare number (seconds) is tolerated.
        if let Ok(n) = buf.parse::<i64>() {
            total += n;
        }
        Some(Utc::now() + chrono::Duration::seconds(total))
    }

    let mut snap = crate::ir::RateLimitSnapshot::default();
    let mut any = false;

    // Anthropic headers (take precedence when present).
    if let Some(v) = get_u64(headers, "anthropic-ratelimit-requests-limit") {
        snap.requests_limit = Some(v);
        any = true;
    }
    if let Some(v) = get_u64(headers, "anthropic-ratelimit-requests-remaining") {
        snap.requests_remaining = Some(v);
        any = true;
    }
    if let Some(v) = get_reset(headers, "anthropic-ratelimit-requests-reset") {
        snap.requests_reset = Some(v);
        any = true;
    }
    if let Some(v) = get_u64(headers, "anthropic-ratelimit-tokens-limit") {
        snap.tokens_limit = Some(v);
        any = true;
    }
    if let Some(v) = get_u64(headers, "anthropic-ratelimit-tokens-remaining") {
        snap.tokens_remaining = Some(v);
        any = true;
    }
    if let Some(v) = get_reset(headers, "anthropic-ratelimit-tokens-reset") {
        snap.tokens_reset = Some(v);
        any = true;
    }

    // OpenAI headers (merged into whatever Anthropic left unset).
    if snap.requests_limit.is_none()
        && let Some(v) = get_u64(headers, "x-ratelimit-limit-requests")
    {
        snap.requests_limit = Some(v);
        any = true;
    }
    if snap.requests_remaining.is_none()
        && let Some(v) = get_u64(headers, "x-ratelimit-remaining-requests")
    {
        snap.requests_remaining = Some(v);
        any = true;
    }
    if snap.requests_reset.is_none()
        && let Some(v) = get_reset(headers, "x-ratelimit-reset-requests")
    {
        snap.requests_reset = Some(v);
        any = true;
    }
    if snap.tokens_limit.is_none()
        && let Some(v) = get_u64(headers, "x-ratelimit-limit-tokens")
    {
        snap.tokens_limit = Some(v);
        any = true;
    }
    if snap.tokens_remaining.is_none()
        && let Some(v) = get_u64(headers, "x-ratelimit-remaining-tokens")
    {
        snap.tokens_remaining = Some(v);
        any = true;
    }
    if snap.tokens_reset.is_none()
        && let Some(v) = get_reset(headers, "x-ratelimit-reset-tokens")
    {
        snap.tokens_reset = Some(v);
        any = true;
    }

    any.then_some(snap)
}

#[async_trait]
impl ModelTransport for DirectTransport {
    fn id(&self) -> &'static str {
        "direct"
    }

    fn supports_codec(&self, codec_id: &str) -> bool {
        if self.allowed_codecs.is_empty() {
            true
        } else {
            self.allowed_codecs.contains(&codec_id)
        }
    }

    async fn resolve_endpoint(
        &self,
        shape: &EndpointShape,
        model: &str,
        mode: InvocationMode,
    ) -> Result<Endpoint> {
        let verb = match mode {
            InvocationMode::Stream => shape.verb_stream,
            _ => shape.verb_unary,
        };
        let path = render_path(shape.path_template, model, verb, &self.context);
        let mut url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        append_stream_query(&mut url, shape, mode);

        // Append static `extra_url_params` (Claude Code OAuth uses
        // `?beta=true` to enable the OAuth-aware code path on the
        // Anthropic API). Sorted for stable ordering across calls.
        if !self.extra_url_params.is_empty() {
            let mut entries: Vec<(&String, &String)> = self.extra_url_params.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (k, v) in entries {
                let separator = if url.contains('?') { '&' } else { '?' };
                url.push(separator);
                url.push_str(k);
                url.push('=');
                url.push_str(v);
            }
        }

        // Append `?key=` query for QueryParam auth (Gemini direct).
        {
            let auth = self.auth.read().await;
            if let DirectAuth::QueryParam { param, value } = &*auth {
                let separator = if url.contains('?') { '&' } else { '?' };
                url.push(separator);
                url.push_str(param);
                url.push('=');
                url.push_str(value.expose_secret());
            }
        }

        // Resolve required headers.
        let mut headers =
            Vec::with_capacity(shape.required_headers.len() + self.extra_headers.len() + 1);
        for h in shape.required_headers {
            let value = match h.source {
                HeaderSource::Literal(s) => s.to_string(),
                HeaderSource::ContextValue(key) => self
                    .context
                    .get(key)
                    .cloned()
                    .ok_or_else(|| {
                        crate::Error::Config(format!(
                            "DirectTransport missing context value '{key}' required by codec header '{}'",
                            h.name
                        ))
                    })?,
            };
            headers.push((h.name.to_string(), value));
        }
        for (k, v) in &self.extra_headers {
            headers.push((k.clone(), v.clone()));
        }
        headers.push(("content-type".to_string(), "application/json".to_string()));

        Ok(Endpoint { url, headers })
    }

    async fn authorize(
        &self,
        req: reqwest::RequestBuilder,
        _body_bytes: &[u8],
    ) -> Result<reqwest::RequestBuilder> {
        let auth = self.auth.read().await;
        let req = match &*auth {
            DirectAuth::XApiKey(key) => req.header("x-api-key", key.expose_secret()),
            DirectAuth::Bearer(token) => {
                req.header("authorization", format!("Bearer {}", token.expose_secret()))
            }
            // Query param auth was already appended to the URL during
            // endpoint resolution; nothing to do here.
            DirectAuth::QueryParam { .. } => req,
            // No-auth providers (Ollama, llama.cpp, custom local
            // gateways) get no header injection. The request hits
            // the wire as-is.
            DirectAuth::None => req,
        };
        Ok(req)
    }

    fn classify_error(
        &self,
        status: u16,
        _body: &str,
    ) -> (crate::error::ProviderErrorKind, Option<&'static str>) {
        use crate::error::ProviderErrorKind;
        match status {
            401 => (
                ProviderErrorKind::Auth,
                Some("API key missing or invalid. Check the configured credential."),
            ),
            _ => super::default_classify_status(status),
        }
    }

    fn parse_rate_limit(
        &self,
        headers: &reqwest::header::HeaderMap,
    ) -> Option<crate::ir::RateLimitSnapshot> {
        parse_direct_rate_limit(headers)
    }

    async fn refresh(&self) -> Result<()> {
        // Static API-key transports have nothing to refresh.
        let Some(provider) = &self.credential_provider else {
            return Ok(());
        };
        if !provider.supports_refresh() {
            return Err(crate::Error::auth(format!(
                "DirectTransport credential provider '{}' does not support refresh",
                provider.name()
            )));
        }
        let fresh = provider.refresh().await?;
        let current = self.auth.read().await.clone();
        let next = Self::credential_to_auth(&current, fresh).ok_or_else(|| {
            crate::Error::auth(
                "DirectTransport could not map the refreshed credential to its current auth scheme"
                    .to_string(),
            )
        })?;
        self.set_auth(next).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::codec::{ApiVersionHint, EndpointShape, HeaderSpec};

    fn anthropic_shape() -> EndpointShape {
        EndpointShape {
            codec_id: "anthropic-messages",
            path_template: "v1/messages",
            verb_unary: "",
            verb_stream: "",
            stream_query: &[],
            required_headers: &[HeaderSpec {
                name: "anthropic-version",
                source: HeaderSource::Literal("2023-06-01"),
            }],
            api_version_hint: ApiVersionHint::Stable,
        }
    }

    fn gemini_shape() -> EndpointShape {
        EndpointShape {
            codec_id: "gemini-generate",
            path_template: "v1beta/models/{model}:{verb}",
            verb_unary: "generateContent",
            verb_stream: "streamGenerateContent",
            stream_query: &[("alt", "sse")],
            required_headers: &[],
            api_version_hint: ApiVersionHint::Stable,
        }
    }

    #[tokio::test]
    async fn anthropic_direct_unary() {
        let t = DirectTransport::new(
            "https://api.anthropic.com",
            DirectAuth::XApiKey(SecretString::from("sk-test")),
        );
        let ep = t
            .resolve_endpoint(
                &anthropic_shape(),
                "claude-sonnet-4-5",
                InvocationMode::Unary,
            )
            .await
            .unwrap();
        assert_eq!(ep.url, "https://api.anthropic.com/v1/messages");
        assert!(
            ep.headers
                .iter()
                .any(|(k, v)| k == "anthropic-version" && v == "2023-06-01")
        );
        assert!(ep.headers.iter().any(|(k, _)| k == "content-type"));
    }

    #[tokio::test]
    async fn anthropic_direct_stream_no_verb_no_query() {
        let t = DirectTransport::new(
            "https://api.anthropic.com",
            DirectAuth::XApiKey(SecretString::from("k")),
        );
        let ep = t
            .resolve_endpoint(&anthropic_shape(), "x", InvocationMode::Stream)
            .await
            .unwrap();
        assert_eq!(ep.url, "https://api.anthropic.com/v1/messages");
    }

    #[tokio::test]
    async fn gemini_direct_streaming_url_has_alt_sse() {
        let t = DirectTransport::new(
            "https://generativelanguage.googleapis.com",
            DirectAuth::QueryParam {
                param: "key",
                value: SecretString::from("secret"),
            },
        );
        let ep = t
            .resolve_endpoint(&gemini_shape(), "gemini-2.5-flash", InvocationMode::Stream)
            .await
            .unwrap();
        assert!(ep
            .url
            .starts_with("https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent"));
        assert!(ep.url.contains("alt=sse"));
        assert!(ep.url.contains("key=secret"));
    }

    #[tokio::test]
    async fn gemini_direct_unary_uses_generate_content() {
        let t = DirectTransport::new(
            "https://generativelanguage.googleapis.com",
            DirectAuth::QueryParam {
                param: "key",
                value: SecretString::from("k"),
            },
        );
        let ep = t
            .resolve_endpoint(&gemini_shape(), "gemini-2.5-flash", InvocationMode::Unary)
            .await
            .unwrap();
        assert!(ep.url.contains(":generateContent"));
        assert!(!ep.url.contains("alt=sse"));
    }

    #[tokio::test]
    async fn authorize_sets_x_api_key_header() {
        let t = DirectTransport::new(
            "https://api.anthropic.com",
            DirectAuth::XApiKey(SecretString::from("sk-x")),
        );
        let client = reqwest::Client::new();
        let req = client.post("https://x");
        let req = t.authorize(req, b"").await.unwrap();
        let built = req.build().unwrap();
        assert_eq!(built.headers().get("x-api-key").unwrap(), "sk-x");
    }

    #[tokio::test]
    async fn authorize_sets_bearer_header() {
        let t = DirectTransport::new(
            "https://api.openai.com",
            DirectAuth::Bearer(SecretString::from("tok")),
        );
        let req = reqwest::Client::new().post("https://x");
        let built = t.authorize(req, b"").await.unwrap().build().unwrap();
        assert_eq!(built.headers().get("authorization").unwrap(), "Bearer tok");
    }

    #[tokio::test]
    async fn allowed_codecs_filter() {
        let t = DirectTransport::new(
            "https://api.anthropic.com",
            DirectAuth::XApiKey(SecretString::from("k")),
        )
        .with_allowed_codecs(&["anthropic-messages"]);
        assert!(t.supports_codec("anthropic-messages"));
        assert!(!t.supports_codec("openai-chat"));
    }

    #[tokio::test]
    async fn empty_allowed_codecs_accepts_anything() {
        let t = DirectTransport::new("https://x", DirectAuth::XApiKey(SecretString::from("k")));
        assert!(t.supports_codec("anything"));
    }

    #[tokio::test]
    async fn missing_context_value_for_required_header_errors() {
        const SHAPE: EndpointShape = EndpointShape {
            codec_id: "test-codec",
            path_template: "v1/x",
            verb_unary: "",
            verb_stream: "",
            stream_query: &[],
            required_headers: &[HeaderSpec {
                name: "x-org",
                source: HeaderSource::ContextValue("org_id"),
            }],
            api_version_hint: ApiVersionHint::Stable,
        };
        let t = DirectTransport::new(
            "https://x.example",
            DirectAuth::Bearer(SecretString::from("k")),
        );
        let result = t
            .resolve_endpoint(&SHAPE, "model", InvocationMode::Unary)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn context_value_substituted_into_header() {
        const SHAPE: EndpointShape = EndpointShape {
            codec_id: "test-codec",
            path_template: "v1/x",
            verb_unary: "",
            verb_stream: "",
            stream_query: &[],
            required_headers: &[HeaderSpec {
                name: "openai-organization",
                source: HeaderSource::ContextValue("org_id"),
            }],
            api_version_hint: ApiVersionHint::Stable,
        };
        let t = DirectTransport::new(
            "https://api.openai.com",
            DirectAuth::Bearer(SecretString::from("k")),
        )
        .with_context("org_id", "org-abc");
        let ep = t
            .resolve_endpoint(&SHAPE, "model", InvocationMode::Unary)
            .await
            .unwrap();
        assert!(
            ep.headers
                .iter()
                .any(|(k, v)| k == "openai-organization" && v == "org-abc")
        );
    }

    #[tokio::test]
    async fn trim_trailing_slash_on_base_url() {
        let t = DirectTransport::new(
            "https://api.anthropic.com///",
            DirectAuth::XApiKey(SecretString::from("k")),
        );
        let ep = t
            .resolve_endpoint(&anthropic_shape(), "x", InvocationMode::Unary)
            .await
            .unwrap();
        assert_eq!(ep.url, "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn parse_rate_limit_reads_anthropic_headers() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        h.insert(
            "anthropic-ratelimit-requests-limit",
            HeaderValue::from_static("1000"),
        );
        h.insert(
            "anthropic-ratelimit-requests-remaining",
            HeaderValue::from_static("950"),
        );
        h.insert(
            "anthropic-ratelimit-requests-reset",
            HeaderValue::from_static("2026-04-11T19:30:00Z"),
        );
        h.insert(
            "anthropic-ratelimit-tokens-limit",
            HeaderValue::from_static("400000"),
        );
        h.insert(
            "anthropic-ratelimit-tokens-remaining",
            HeaderValue::from_static("250000"),
        );
        let snap = super::parse_direct_rate_limit(&h).expect("snapshot present");
        assert_eq!(snap.requests_limit, Some(1000));
        assert_eq!(snap.requests_remaining, Some(950));
        assert_eq!(snap.tokens_limit, Some(400_000));
        assert_eq!(snap.tokens_remaining, Some(250_000));
        assert!(snap.requests_reset.is_some());
    }

    #[test]
    fn parse_rate_limit_reads_openai_headers() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        h.insert(
            "x-ratelimit-limit-requests",
            HeaderValue::from_static("5000"),
        );
        h.insert(
            "x-ratelimit-remaining-requests",
            HeaderValue::from_static("4999"),
        );
        h.insert(
            "x-ratelimit-limit-tokens",
            HeaderValue::from_static("200000"),
        );
        h.insert(
            "x-ratelimit-remaining-tokens",
            HeaderValue::from_static("199900"),
        );
        let snap = super::parse_direct_rate_limit(&h).expect("snapshot present");
        assert_eq!(snap.requests_limit, Some(5000));
        assert_eq!(snap.requests_remaining, Some(4999));
        assert_eq!(snap.tokens_remaining, Some(199_900));
    }

    #[test]
    fn parse_rate_limit_returns_none_when_no_headers_match() {
        use reqwest::header::HeaderMap;
        assert!(super::parse_direct_rate_limit(&HeaderMap::new()).is_none());
    }

    #[test]
    fn classify_error_401_gives_api_key_hint() {
        let t = DirectTransport::new(
            "https://api.anthropic.com",
            DirectAuth::XApiKey(SecretString::from("k")),
        );
        let (kind, hint) = t.classify_error(401, "invalid api key");
        assert!(matches!(kind, crate::error::ProviderErrorKind::Auth));
        assert!(hint.is_some());
    }

    #[test]
    fn classify_error_429_falls_through_to_default() {
        let t = DirectTransport::new(
            "https://api.anthropic.com",
            DirectAuth::XApiKey(SecretString::from("k")),
        );
        let (kind, _) = t.classify_error(429, "rate limited");
        assert!(matches!(kind, crate::error::ProviderErrorKind::RateLimit));
    }
}
