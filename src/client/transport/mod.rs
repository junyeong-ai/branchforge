//! `ModelTransport` — endpoint resolution and authentication.
//!
//! A [`ModelTransport`] owns the *how* of getting bytes to and from a
//! provider host: URL construction, auth headers, credential refresh, and
//! any wire-level signing (AWS SigV4, GCP ADC, Azure Entra). It is
//! deliberately ignorant of the request body shape — that is the
//! [`crate::client::codec::ModelCodec`]'s job. The bridge between the two
//! is [`crate::client::codec::EndpointShape`], which transports consume to
//! produce a concrete [`Endpoint`].
//!
//! This split is what makes Vertex-Gemini fall out for free:
//! `VertexTransport` consumes any codec's `EndpointShape` and routes to
//! `publishers/anthropic` or `publishers/google` based on the codec id,
//! without baking either into the URL.
//!
//! See plan §1.

#[cfg(feature = "aws")]
pub mod bedrock;
pub mod bedrock_stream;
pub mod direct;
#[cfg(feature = "azure")]
pub mod foundry;
#[cfg(feature = "gcp")]
pub mod vertex;

use async_trait::async_trait;
use std::collections::HashMap;

use crate::Result;
use crate::client::codec::{EndpointShape, InvocationMode};
use crate::error::ProviderErrorKind;

#[cfg(feature = "aws")]
pub use bedrock::BedrockTransport;
pub use direct::{DirectAuth, DirectTransport};
#[cfg(feature = "azure")]
pub use foundry::FoundryTransport;
#[cfg(feature = "gcp")]
pub use vertex::{VertexTransport, publisher_for_codec};

/// Endpoint resolution and authentication for one provider host.
#[async_trait]
pub trait ModelTransport: Send + Sync + std::fmt::Debug {
    /// Stable identifier for this transport, e.g. `"direct"`, `"bedrock"`,
    /// `"vertex"`, `"foundry"`. Used by codec/transport composition checks
    /// in `Client::builder()`.
    fn id(&self) -> &'static str;

    /// `true` if this transport can carry the named codec. Used by the
    /// `Client` builder to validate compositions at construction time.
    fn supports_codec(&self, codec_id: &str) -> bool;

    /// Resolve a concrete [`Endpoint`] for the given codec endpoint shape,
    /// model id, and invocation mode. The transport substitutes its own
    /// context (project, region, etc.) into the codec's path template.
    async fn resolve_endpoint(
        &self,
        shape: &EndpointShape,
        model: &str,
        mode: InvocationMode,
    ) -> Result<Endpoint>;

    /// Apply authentication headers to a `reqwest::RequestBuilder`.
    ///
    /// `body_bytes` is the already-serialised request payload. Most
    /// transports ignore it; AWS SigV4 uses it to compute the request
    /// signature. The caller is expected to attach the body separately
    /// via `req.body(body_bytes)` *after* this call returns, so the body
    /// participates in the signed headers but is not double-set.
    async fn authorize(
        &self,
        req: reqwest::RequestBuilder,
        body_bytes: &[u8],
    ) -> Result<reqwest::RequestBuilder>;

    /// Refresh underlying credentials if they support refresh. Default is
    /// no-op for transports with non-refreshing credentials.
    async fn refresh(&self) -> Result<()> {
        Ok(())
    }

    /// Classify a non-2xx HTTP response from this transport into a
    /// [`ProviderErrorKind`] plus an optional human-readable hint.
    ///
    /// Each transport owns its own error patterns: GCP quota project
    /// failures (`x-goog-user-project`), Vertex Publisher Model 404s,
    /// Bedrock throttling, Foundry Entra refresh hints, and so on. The
    /// default implementation handles only generic HTTP status codes;
    /// transports with vendor-specific failure modes override this.
    ///
    /// This method exists so that adding a new transport never requires
    /// editing a central `match` block on the transport id — adhering to
    /// the open/closed principle.
    fn classify_error(&self, status: u16, body: &str) -> (ProviderErrorKind, Option<&'static str>) {
        let _ = body;
        default_classify_status(status)
    }
}

/// Generic status-code classification used as the default for transports
/// that have no vendor-specific failure modes.
pub(crate) fn default_classify_status(status: u16) -> (ProviderErrorKind, Option<&'static str>) {
    match status {
        401 | 403 => (ProviderErrorKind::Auth, None),
        429 => (ProviderErrorKind::RateLimit, None),
        500..=599 => (ProviderErrorKind::Server, None),
        400..=499 => (ProviderErrorKind::BadRequest, None),
        _ => (ProviderErrorKind::Server, None),
    }
}

/// A fully-resolved endpoint: URL plus any headers the transport wants the
/// client to set before [`ModelTransport::authorize`] is invoked.
///
/// Auth headers are applied separately by `authorize()` so that retries can
/// pick up freshly-refreshed credentials without re-resolving the endpoint.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Fully-qualified URL to POST to.
    pub url: String,
    /// Static headers (codec-required + transport-injected). Authorization
    /// headers are NOT included here; they come from `authorize()`.
    pub headers: Vec<(String, String)>,
}

impl Endpoint {
    /// Construct an `Endpoint` with a URL and no headers.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            headers: Vec::new(),
        }
    }

    /// Builder helper to add a header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// Substitute `{model}`, `{verb}`, and any `{key}` from `context` into a
/// URL path template. Used by every transport that consumes
/// [`EndpointShape::path_template`].
pub(crate) fn render_path(
    template: &str,
    model: &str,
    verb: &str,
    context: &HashMap<&'static str, String>,
) -> String {
    let mut out = template.to_string();
    out = out.replace("{model}", model);
    out = out.replace("{verb}", verb);
    for (k, v) in context {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Append the streaming query parameters from an [`EndpointShape`] to a
/// URL. No-op for unary mode or when the shape declares no stream query.
pub(crate) fn append_stream_query(url: &mut String, shape: &EndpointShape, mode: InvocationMode) {
    if !matches!(mode, InvocationMode::Stream) || shape.stream_query.is_empty() {
        return;
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    url.push(separator);
    let parts: Vec<String> = shape
        .stream_query
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    url.push_str(&parts.join("&"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_path_substitutes_model_verb_and_context() {
        let mut ctx = HashMap::new();
        ctx.insert("project", "p1".to_string());
        ctx.insert("location", "us-central1".to_string());
        let url = render_path(
            "v1/projects/{project}/locations/{location}/publishers/anthropic/models/{model}:{verb}",
            "claude-sonnet-4-5",
            "rawPredict",
            &ctx,
        );
        assert_eq!(
            url,
            "v1/projects/p1/locations/us-central1/publishers/anthropic/models/claude-sonnet-4-5:rawPredict"
        );
    }

    #[test]
    fn render_path_with_no_placeholders_passes_through() {
        let url = render_path("v1/messages", "ignored", "ignored", &HashMap::new());
        assert_eq!(url, "v1/messages");
    }

    #[test]
    fn append_stream_query_unary_is_noop() {
        let shape = EndpointShape {
            codec_id: "test-codec",
            path_template: "x",
            verb_unary: "",
            verb_stream: "",
            stream_query: &[("alt", "sse")],
            required_headers: &[],
            api_version_hint: crate::client::codec::ApiVersionHint::Stable,
        };
        let mut url = "https://x.example/v1".to_string();
        append_stream_query(&mut url, &shape, InvocationMode::Unary);
        assert_eq!(url, "https://x.example/v1");
    }

    #[test]
    fn append_stream_query_stream_appends() {
        let shape = EndpointShape {
            codec_id: "test-codec",
            path_template: "x",
            verb_unary: "",
            verb_stream: "",
            stream_query: &[("alt", "sse")],
            required_headers: &[],
            api_version_hint: crate::client::codec::ApiVersionHint::Stable,
        };
        let mut url = "https://x.example/v1".to_string();
        append_stream_query(&mut url, &shape, InvocationMode::Stream);
        assert_eq!(url, "https://x.example/v1?alt=sse");
    }

    #[test]
    fn append_stream_query_with_existing_query() {
        let shape = EndpointShape {
            codec_id: "test-codec",
            path_template: "x",
            verb_unary: "",
            verb_stream: "",
            stream_query: &[("alt", "sse"), ("foo", "bar")],
            required_headers: &[],
            api_version_hint: crate::client::codec::ApiVersionHint::Stable,
        };
        let mut url = "https://x.example/v1?baz=qux".to_string();
        append_stream_query(&mut url, &shape, InvocationMode::Stream);
        assert_eq!(url, "https://x.example/v1?baz=qux&alt=sse&foo=bar");
    }

    #[test]
    fn endpoint_builder() {
        let e = Endpoint::new("https://x.example/v1/messages")
            .with_header("anthropic-version", "2023-06-01");
        assert_eq!(e.url, "https://x.example/v1/messages");
        assert_eq!(e.headers.len(), 1);
        assert_eq!(e.headers[0].0, "anthropic-version");
    }
}
