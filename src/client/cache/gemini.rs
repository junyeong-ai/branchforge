//! Gemini `cachedContents` resource lifecycle.
//!
//! Gemini's prompt caching is a **separate resource lifecycle**: a
//! client POSTs to `/cachedContents` to mint a cache handle (name),
//! then references that handle in subsequent `generateContent` calls
//! via the `cachedContent` field. This is fundamentally different
//! from Anthropic/OpenAI inline `cache_control` markers and cannot be
//! represented as a per-message IR annotation.
//!
//! This module exposes two layers:
//!
//! 1. **Pure request/response shaping helpers** ([`build_create_cache_body`],
//!    [`parse_cache_name`], [`delete_cache_path`]) — no HTTP, no auth,
//!    no state. Useful for callers who want to drive the protocol
//!    through their own transport.
//! 2. **A stateful [`GeminiCacheClient`]** that wires the helpers into a
//!    `reqwest::Client`, handles AI Studio query-param auth or Vertex
//!    bearer auth, and exposes `create` / `get` / `delete` async
//!    methods. This is what most callers want.
//!
//! # Example
//!
//! ```ignore
//! use branchforge::client::cache::{GeminiCacheClient, CreateCacheParams};
//! use serde_json::json;
//! use secrecy::SecretString;
//!
//! let client = GeminiCacheClient::ai_studio(SecretString::from("API_KEY"));
//!
//! let name = client.create(CreateCacheParams {
//!     model: "models/gemini-2.5-flash",
//!     system_instruction: Some("You are a senior code reviewer."),
//!     contents: vec![json!({"role":"user","parts":[{"text":"<long doc>"}]})],
//!     ttl_seconds: Some(3600),
//!     display_name: Some("code-review-context"),
//! }).await?;
//!
//! // Stash `name` on the next IR request:
//! // request.provider_options.gemini = Some(GeminiOptions {
//! //     cached_content: Some(name),
//! //     ..Default::default()
//! // });
//!
//! // When you're done, reclaim the resource:
//! client.delete(&name).await?;
//! ```

use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};

use crate::Result;

/// Default Gemini AI Studio base URL (`https://generativelanguage.googleapis.com/v1beta`).
const DEFAULT_AI_STUDIO_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Parameters for [`build_create_cache_body`] and [`GeminiCacheClient::create`].
#[derive(Debug, Clone)]
pub struct CreateCacheParams<'a> {
    /// Fully-qualified model id (e.g. `"models/gemini-2.5-flash"`).
    /// The cache is bound to one model.
    pub model: &'a str,
    /// Optional system instruction text. Becomes a `systemInstruction`
    /// block on the wire.
    pub system_instruction: Option<&'a str>,
    /// Pre-built `contents` array entries (already shaped as Gemini
    /// `Content` objects with `role` and `parts`). The caller is
    /// responsible for the inner shape because callers typically
    /// have their own message-to-Gemini conversion.
    pub contents: Vec<Value>,
    /// Cache TTL in seconds. The Gemini API encodes this as the
    /// string `"<seconds>s"` on the wire.
    pub ttl_seconds: Option<u64>,
    /// Human-readable label for the cache resource. Optional.
    pub display_name: Option<&'a str>,
}

/// Build the JSON body for `POST /v1beta/cachedContents` (Gemini Developer API)
/// or `POST /v1beta1/projects/.../cachedContents` (Vertex AI).
///
/// The wire shape is identical between the two endpoints; the
/// difference is the URL prefix and auth, which the client handles.
pub fn build_create_cache_body(params: &CreateCacheParams<'_>) -> Value {
    let mut body = json!({
        "model": params.model,
        "contents": params.contents,
    });

    if let Some(sys) = params.system_instruction {
        body["systemInstruction"] = json!({
            "parts": [{ "text": sys }]
        });
    }
    if let Some(secs) = params.ttl_seconds {
        body["ttl"] = json!(format!("{secs}s"));
    }
    if let Some(name) = params.display_name {
        body["displayName"] = json!(name);
    }

    body
}

/// Parse the cache resource name from a Gemini `cachedContents`
/// create-or-get response.
///
/// The Gemini API returns the resource name as the top-level `name`
/// field, e.g. `"cachedContents/abc123"`. Callers stash this string
/// on subsequent `crate::ir::GeminiOptions::cached_content` for
/// the codec to forward.
pub fn parse_cache_name(response: &Value) -> Result<String> {
    response
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            crate::Error::Parse(
                "Gemini cachedContents response missing top-level `name` field".into(),
            )
        })
}

/// Build the URL path fragment for fetching or deleting a cached
/// content resource. Concatenate with the API base + version prefix
/// the caller's transport already knows about.
///
/// Example output: `"cachedContents/abc123"`.
pub fn delete_cache_path(name: &str) -> String {
    if name.starts_with("cachedContents/") {
        name.to_owned()
    } else {
        format!("cachedContents/{name}")
    }
}

/// Authentication scheme for [`GeminiCacheClient`].
///
/// Gemini AI Studio uses `?key=<API_KEY>` query-parameter auth; Vertex
/// AI uses a bearer token from GCP ADC. The two are exposed as
/// orthogonal constructors on the client; this enum is the internal
/// representation.
#[non_exhaustive]
#[derive(Clone)]
pub enum GeminiCacheAuth {
    /// `?key=<api_key>` appended to every request URL.
    QueryApiKey(SecretString),
    /// `Authorization: Bearer <token>` header on every request.
    Bearer(SecretString),
}

impl std::fmt::Debug for GeminiCacheAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueryApiKey(_) => f.write_str("QueryApiKey(***)"),
            Self::Bearer(_) => f.write_str("Bearer(***)"),
        }
    }
}

/// Stateful client for the Gemini `cachedContents` resource
/// lifecycle. Holds an HTTP client, a base URL, and an auth scheme.
///
/// Two constructors cover the two endpoints:
/// * [`Self::ai_studio`] — `https://generativelanguage.googleapis.com/v1beta`
///   with query-param auth.
/// * [`Self::vertex`] — caller-supplied base URL and bearer token.
///
/// The client is intentionally cheap to clone (wraps `reqwest::Client`
/// which is itself `Arc`-backed) so callers can stash it alongside
/// their agent runtime.
#[derive(Debug, Clone)]
pub struct GeminiCacheClient {
    http: reqwest::Client,
    base_url: String,
    auth: GeminiCacheAuth,
}

impl GeminiCacheClient {
    /// Build a cache client for Gemini Developer API (AI Studio) using
    /// query-parameter API key auth. Base URL defaults to
    /// `https://generativelanguage.googleapis.com/v1beta`.
    pub fn ai_studio(api_key: SecretString) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: DEFAULT_AI_STUDIO_BASE.to_string(),
            auth: GeminiCacheAuth::QueryApiKey(api_key),
        }
    }

    /// Build a cache client for Vertex AI using a bearer token. The
    /// caller provides the full base URL (typically
    /// `https://<region>-aiplatform.googleapis.com/v1beta1/projects/<project>/locations/<region>`).
    pub fn vertex(base_url: impl Into<String>, bearer: SecretString) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: trim_trailing_slash(base_url.into()),
            auth: GeminiCacheAuth::Bearer(bearer),
        }
    }

    /// Override the underlying HTTP client. Useful in tests (wiremock)
    /// or when the caller wants a shared connection pool.
    #[must_use]
    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// Override the base URL. Useful in tests (wiremock) or when
    /// pointing at a non-default Vertex region.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = trim_trailing_slash(base_url.into());
        self
    }

    fn cached_contents_url(&self) -> String {
        self.append_query(format!("{}/cachedContents", self.base_url))
    }

    fn resource_url(&self, name: &str) -> String {
        let path = delete_cache_path(name);
        self.append_query(format!("{}/{}", self.base_url, path))
    }

    /// Append the query-parameter API key to a URL when running in
    /// AI Studio mode. Vertex bearer auth leaves the URL untouched —
    /// the token goes on the `Authorization` header via
    /// [`Self::apply_auth`].
    fn append_query(&self, url: String) -> String {
        match &self.auth {
            GeminiCacheAuth::QueryApiKey(key) => {
                let sep = if url.contains('?') { '&' } else { '?' };
                format!("{url}{sep}key={}", urlencoding::encode(key.expose_secret()))
            }
            GeminiCacheAuth::Bearer(_) => url,
        }
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            GeminiCacheAuth::QueryApiKey(_) => req,
            GeminiCacheAuth::Bearer(token) => {
                req.header("authorization", format!("Bearer {}", token.expose_secret()))
            }
        }
    }

    /// Mint a new cache resource and return its fully-qualified name
    /// (`cachedContents/<id>`). The name is what the caller stashes on
    /// the next IR request's `cached_content` field.
    pub async fn create(&self, params: CreateCacheParams<'_>) -> Result<String> {
        let body = build_create_cache_body(&params);
        let url = self.cached_contents_url();
        let response = self
            .apply_auth(self.http.post(&url))
            .json(&body)
            .send()
            .await
            .map_err(|e| provider_network_error("create", e))?;

        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| crate::Error::Parse(format!("Gemini cache create response: {e}")))?;

        if !status.is_success() {
            return Err(provider_status_error("create", status, &body));
        }

        parse_cache_name(&body)
    }

    /// Fetch the cache resource metadata for `name`. Returns the raw
    /// JSON response as-is because downstream consumers care about
    /// different fields (`ttl`, `expireTime`, `usageMetadata`).
    pub async fn get(&self, name: &str) -> Result<Value> {
        let url = self.resource_url(name);
        let response = self
            .apply_auth(self.http.get(&url))
            .send()
            .await
            .map_err(|e| provider_network_error("get", e))?;

        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|e| crate::Error::Parse(format!("Gemini cache get response: {e}")))?;

        if !status.is_success() {
            return Err(provider_status_error("get", status, &body));
        }

        Ok(body)
    }

    /// Delete the cache resource `name`. Idempotent from the caller's
    /// perspective: a 404 is treated as success because the net effect
    /// ("this resource no longer exists") is the same.
    pub async fn delete(&self, name: &str) -> Result<()> {
        let url = self.resource_url(name);
        let response = self
            .apply_auth(self.http.delete(&url))
            .send()
            .await
            .map_err(|e| provider_network_error("delete", e))?;

        let status = response.status();
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }

        let body = response.text().await.unwrap_or_default();
        Err(provider_status_error(
            "delete",
            status,
            &Value::String(body),
        ))
    }
}

fn provider_network_error(op: &'static str, err: reqwest::Error) -> crate::Error {
    crate::Error::Provider {
        provider: "gemini-cache",
        kind: crate::error::ProviderErrorKind::Network,
        message: format!("{op}: {err}"),
        hint: None,
        retryable: true,
        status: None,
        rate_limit: None,
    }
}

fn provider_status_error(
    op: &'static str,
    status: reqwest::StatusCode,
    body: &Value,
) -> crate::Error {
    let kind = match status.as_u16() {
        401 | 403 => crate::error::ProviderErrorKind::Auth,
        429 => crate::error::ProviderErrorKind::RateLimit,
        413 => crate::error::ProviderErrorKind::PayloadTooLarge,
        s if s >= 500 => crate::error::ProviderErrorKind::Server,
        _ => crate::error::ProviderErrorKind::BadRequest,
    };
    let retryable = matches!(
        kind,
        crate::error::ProviderErrorKind::RateLimit
            | crate::error::ProviderErrorKind::Server
            | crate::error::ProviderErrorKind::Network
    );
    crate::Error::Provider {
        provider: "gemini-cache",
        kind,
        message: format!("{op} failed ({status}): {body}"),
        hint: None,
        retryable,
        status: Some(status.as_u16()),
        rate_limit: None,
    }
}

fn trim_trailing_slash(mut url: String) -> String {
    while url.ends_with('/') {
        url.pop();
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn build_minimal_create_body() {
        let body = build_create_cache_body(&CreateCacheParams {
            model: "models/gemini-2.5-flash",
            system_instruction: None,
            contents: vec![json!({"role":"user","parts":[{"text":"hi"}]})],
            ttl_seconds: None,
            display_name: None,
        });
        assert_eq!(body["model"], "models/gemini-2.5-flash");
        assert_eq!(body["contents"][0]["role"], "user");
        assert!(body.get("systemInstruction").is_none());
        assert!(body.get("ttl").is_none());
    }

    #[test]
    fn build_full_create_body_includes_all_fields() {
        let body = build_create_cache_body(&CreateCacheParams {
            model: "models/gemini-2.5-pro",
            system_instruction: Some("You are helpful."),
            contents: vec![json!({"role":"user","parts":[{"text":"long doc"}]})],
            ttl_seconds: Some(3600),
            display_name: Some("my-cache"),
        });
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "You are helpful."
        );
        assert_eq!(body["ttl"], "3600s");
        assert_eq!(body["displayName"], "my-cache");
    }

    #[test]
    fn parse_cache_name_extracts_name() {
        let response = json!({
            "name": "cachedContents/xyz789",
            "model": "models/gemini-2.5-flash",
            "createTime": "2026-04-11T12:00:00Z"
        });
        assert_eq!(
            parse_cache_name(&response).unwrap(),
            "cachedContents/xyz789"
        );
    }

    #[test]
    fn parse_cache_name_errors_on_missing_field() {
        let response = json!({"model": "models/gemini-2.5-flash"});
        assert!(parse_cache_name(&response).is_err());
    }

    #[test]
    fn delete_path_accepts_full_or_bare_name() {
        assert_eq!(
            delete_cache_path("cachedContents/abc"),
            "cachedContents/abc"
        );
        assert_eq!(delete_cache_path("abc"), "cachedContents/abc");
    }

    #[tokio::test]
    async fn ai_studio_create_sends_query_key_and_parses_name() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cachedContents"))
            .and(query_param("key", "AIza-test"))
            .and(body_json(json!({
                "model": "models/gemini-2.5-flash",
                "contents": [{"role":"user","parts":[{"text":"hi"}]}],
                "systemInstruction": {"parts":[{"text":"sys"}]},
                "ttl": "60s",
                "displayName": "unit-test"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "cachedContents/test-id",
                "model": "models/gemini-2.5-flash",
                "createTime": "2026-04-11T12:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = GeminiCacheClient::ai_studio(SecretString::from("AIza-test"))
            .with_base_url(server.uri());

        let name = client
            .create(CreateCacheParams {
                model: "models/gemini-2.5-flash",
                system_instruction: Some("sys"),
                contents: vec![json!({"role":"user","parts":[{"text":"hi"}]})],
                ttl_seconds: Some(60),
                display_name: Some("unit-test"),
            })
            .await
            .unwrap();

        assert_eq!(name, "cachedContents/test-id");
    }

    #[tokio::test]
    async fn vertex_get_sends_bearer_header_and_returns_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cachedContents/abc"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer ya29.TOKEN",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "cachedContents/abc",
                "usageMetadata": {"totalTokenCount": 1234}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = GeminiCacheClient::vertex(server.uri(), SecretString::from("ya29.TOKEN"));
        let body = client.get("abc").await.unwrap();
        assert_eq!(body["usageMetadata"]["totalTokenCount"], 1234);
    }

    #[tokio::test]
    async fn delete_treats_404_as_success() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/cachedContents/gone"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            GeminiCacheClient::ai_studio(SecretString::from("k")).with_base_url(server.uri());
        client.delete("gone").await.unwrap();
    }

    /// End-to-end wiring: the cache client mints a handle, the caller
    /// stashes it on `GeminiOptions::cached_content`, and the Gemini
    /// codec forwards it on the wire at encode time. This proves W-10
    /// integrates with the real consumer path — no library-type-only
    /// smoke test.
    #[tokio::test]
    async fn end_to_end_minted_handle_flows_into_gemini_codec() {
        use crate::client::codec::gemini_generate::GeminiGenerateCodec;
        use crate::client::codec::{InvocationMode, ModelCodec};
        use crate::ir::{GeminiOptions, Message, ModelRequest, ProviderOptions};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cachedContents"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "cachedContents/minted-id",
                "model": "models/gemini-2.5-flash",
                "createTime": "2026-04-11T12:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            GeminiCacheClient::ai_studio(SecretString::from("k")).with_base_url(server.uri());
        let handle = client
            .create(CreateCacheParams {
                model: "models/gemini-2.5-flash",
                system_instruction: Some("sys"),
                contents: vec![json!({"role":"user","parts":[{"text":"context"}]})],
                ttl_seconds: Some(3600),
                display_name: None,
            })
            .await
            .unwrap();
        assert_eq!(handle, "cachedContents/minted-id");

        // Caller stashes the minted handle on the next IR request...
        let mut request = ModelRequest::new("gemini-2.5-flash", vec![Message::user("continue")]);
        request.provider_options = ProviderOptions {
            gemini: Some(GeminiOptions {
                cached_content: Some(handle.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };

        // ...and the Gemini codec emits it at the `cachedContent` field.
        let codec = GeminiGenerateCodec;
        let encoded = codec
            .encode_request(&request, InvocationMode::Unary)
            .unwrap();
        assert_eq!(
            encoded.body.get("cachedContent").and_then(Value::as_str),
            Some(handle.as_str()),
            "minted cache handle must appear on the wire body"
        );
    }

    #[tokio::test]
    async fn create_surfaces_non_2xx_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cachedContents"))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({
                "error": {"message": "RESOURCE_EXHAUSTED"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            GeminiCacheClient::ai_studio(SecretString::from("k")).with_base_url(server.uri());

        let err = client
            .create(CreateCacheParams {
                model: "models/gemini-2.5-flash",
                system_instruction: None,
                contents: vec![json!({"role":"user","parts":[{"text":"x"}]})],
                ttl_seconds: None,
                display_name: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("429"));
        assert!(err.to_string().contains("RESOURCE_EXHAUSTED"));
    }
}
