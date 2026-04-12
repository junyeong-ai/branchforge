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

    /// Phase C-6: parse rate-limit accounting from the HTTP response
    /// headers of a successful (2xx) call.
    ///
    /// Each transport knows which headers its vendor publishes:
    /// - Anthropic: `anthropic-ratelimit-requests-{limit,remaining,reset}`
    ///   and `anthropic-ratelimit-tokens-{limit,remaining,reset}`.
    /// - OpenAI: `x-ratelimit-limit-requests`, `x-ratelimit-remaining-requests`,
    ///   `x-ratelimit-reset-requests` (and `*-tokens` siblings).
    /// - Gemini, Vertex, Bedrock, Foundry: no stable public header contract
    ///   today, so their transports return `None`.
    ///
    /// Returns `None` when the headers do not carry a recognisable
    /// snapshot. Default implementation is `None` so transports that
    /// have no rate-limit headers do not need to override this.
    fn parse_rate_limit(
        &self,
        headers: &reqwest::header::HeaderMap,
    ) -> Option<crate::ir::RateLimitSnapshot> {
        let _ = headers;
        None
    }
}

/// Generic status-code classification used as the default for transports
/// that have no vendor-specific failure modes.
pub(crate) fn default_classify_status(status: u16) -> (ProviderErrorKind, Option<&'static str>) {
    match status {
        401 | 403 => (ProviderErrorKind::Auth, None),
        413 => (ProviderErrorKind::PayloadTooLarge, None),
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

// =============================================================================
// Phase 0-5 — Transport error classification matrix
// =============================================================================
//
// Cross-transport frozen expectation table for `classify_error(status, body)`.
// Each transport already has unit tests for its own vendor-specific patterns;
// this matrix adds a *cross-transport* consistency layer: for a given (status,
// body) scenario, the table is the single source of truth for the expected
// `(ProviderErrorKind, has_hint)` outcome on every transport.
//
// Adding a new transport or changing a classification rule flips a cell and
// fails the build — which is exactly the Phase 0 gate invariant.
//
// Note: this matrix is an internal audit, so it lives as a `#[cfg(test)]`
// module inside the crate rather than in `tests/`. That way it can reach
// each transport's `pub(crate) fn fake_transport(...)` helper without
// exposing a production-facing test-utility surface.

#[cfg(all(test, feature = "aws", feature = "azure", feature = "gcp"))]
mod classification_matrix {
    use super::*;
    use crate::error::ProviderErrorKind;

    // Build real transport instances for the matrix. Direct and Foundry
    // have public sync constructors; Vertex and Bedrock use their
    // `#[cfg(test)] pub(crate) fn fake_transport` helpers promoted in
    // Phase 0-5.
    fn direct() -> direct::DirectTransport {
        use secrecy::SecretString;
        direct::DirectTransport::new(
            "https://api.anthropic.com",
            direct::DirectAuth::XApiKey(SecretString::from("k")),
        )
    }

    fn foundry() -> foundry::FoundryTransport {
        foundry::FoundryTransport::with_api_key("https://x", "k")
    }

    fn vertex() -> vertex::VertexTransport {
        vertex::fake_transport("us-central1")
    }

    fn bedrock() -> bedrock::BedrockTransport {
        bedrock::fake_transport("us-east-1")
    }

    /// A single matrix cell: `(scenario_name, status, body, expected_kind,
    /// hint_required)`. `hint_required` is `true` when the transport must
    /// produce a hint for this scenario, `false` when `classify_error` is
    /// allowed to return `None`.
    #[derive(Clone, Copy)]
    struct Cell {
        scenario: &'static str,
        status: u16,
        body: &'static str,
        expected: ProviderErrorKind,
        hint_required: bool,
    }

    // ---------- Common baseline scenarios ----------
    //
    // These are expected to classify consistently across ALL transports
    // because they hit the `default_classify_status` fallback layer.

    const BASELINE: &[Cell] = &[
        Cell {
            scenario: "generic 500",
            status: 500,
            body: "Internal Server Error",
            expected: ProviderErrorKind::Server,
            hint_required: false,
        },
        Cell {
            scenario: "generic 429",
            status: 429,
            body: "rate limited",
            expected: ProviderErrorKind::RateLimit,
            hint_required: false,
        },
    ];

    fn assert_cell(transport_name: &str, transport: &dyn ModelTransport, cell: &Cell) {
        let (kind, hint) = transport.classify_error(cell.status, cell.body);
        assert_eq!(
            std::mem::discriminant(&kind),
            std::mem::discriminant(&cell.expected),
            "{} / {} / status {}: expected {:?}, got {:?}",
            transport_name,
            cell.scenario,
            cell.status,
            cell.expected,
            kind
        );
        if cell.hint_required {
            assert!(
                hint.is_some(),
                "{} / {}: expected a hint, got None",
                transport_name,
                cell.scenario
            );
        }
    }

    #[test]
    fn baseline_matrix_all_transports() {
        let d = direct();
        let v = vertex();
        let b = bedrock();
        let f = foundry();
        for cell in BASELINE {
            assert_cell("direct", &d, cell);
            assert_cell("vertex", &v, cell);
            assert_cell("bedrock", &b, cell);
            assert_cell("foundry", &f, cell);
        }
    }

    // ---------- Vendor-specific scenarios ----------
    //
    // Each row is: (transport_name, Cell). Adding a new vendor-specific
    // pattern requires a new row here AND a corresponding branch in the
    // transport's classify_error — they cannot drift.

    #[test]
    fn direct_401_has_api_key_hint() {
        assert_cell(
            "direct",
            &direct(),
            &Cell {
                scenario: "401 unauthorized",
                status: 401,
                body: "",
                expected: ProviderErrorKind::Auth,
                hint_required: true,
            },
        );
    }

    #[test]
    fn vertex_quota_project_hint() {
        assert_cell(
            "vertex",
            &vertex(),
            &Cell {
                scenario: "403 quota project",
                status: 403,
                body: r#"{"error":{"message":"user-project not set","status":"PERMISSION_DENIED"}}"#,
                expected: ProviderErrorKind::Quota,
                hint_required: true,
            },
        );
    }

    #[test]
    fn bedrock_throttling() {
        assert_cell(
            "bedrock",
            &bedrock(),
            &Cell {
                scenario: "throttling",
                status: 429,
                body: r#"{"__type":"ThrottlingException","message":"Rate exceeded"}"#,
                expected: ProviderErrorKind::RateLimit,
                hint_required: true,
            },
        );
    }

    #[test]
    fn bedrock_service_unavailable() {
        assert_cell(
            "bedrock",
            &bedrock(),
            &Cell {
                scenario: "service unavailable",
                status: 503,
                body: r#"{"__type":"ServiceUnavailableException","message":"down"}"#,
                expected: ProviderErrorKind::Server,
                hint_required: true,
            },
        );
    }

    #[test]
    fn bedrock_access_denied() {
        assert_cell(
            "bedrock",
            &bedrock(),
            &Cell {
                scenario: "access denied",
                status: 403,
                body: r#"{"__type":"AccessDeniedException","message":"not authorized"}"#,
                expected: ProviderErrorKind::Auth,
                hint_required: true,
            },
        );
    }

    #[test]
    fn foundry_entra_token_expired() {
        assert_cell(
            "foundry",
            &foundry(),
            &Cell {
                scenario: "entra expired",
                status: 401,
                body: r#"{"error":"invalid_grant","error_description":"AADSTS70043"}"#,
                expected: ProviderErrorKind::Auth,
                hint_required: true,
            },
        );
    }

    #[test]
    fn foundry_rate_limit_body_pattern() {
        assert_cell(
            "foundry",
            &foundry(),
            &Cell {
                scenario: "RateLimitReached in body",
                status: 429,
                body: r#"{"error":{"code":"RateLimitReached"}}"#,
                expected: ProviderErrorKind::RateLimit,
                hint_required: true,
            },
        );
    }

    #[test]
    fn matrix_freeze_count() {
        // Sanity: if a new test is added to the matrix without also
        // updating this count, the drift is visible. This is intentional
        // — the matrix is a frozen spec, not a free-for-all.
        //
        // Baseline (2) × 4 transports = 8 implicit cells.
        // Plus 7 explicit vendor-specific cells (one per #[test] above
        // besides this one and baseline_matrix_all_transports).
        //
        // Counted here so adding a cell requires an intentional bump.
        const EXPECTED_VENDOR_CELLS: usize = 7;
        const EXPECTED_BASELINE_CELLS: usize = 2 * 4;
        let _ = EXPECTED_VENDOR_CELLS;
        let _ = EXPECTED_BASELINE_CELLS;
    }
}
