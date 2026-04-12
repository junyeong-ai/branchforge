//! Structured span definitions for tracing.
//!
//! Attribute naming follows the OpenTelemetry GenAI semantic conventions
//! where applicable: `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`,
//! `gen_ai.usage.reasoning_tokens`, and the non-standard but widely used
//! `gen_ai.usage.cost_usd`. Cache-specific attributes
//! (`gen_ai.usage.cache_read_tokens`, `gen_ai.usage.cache_creation_tokens`)
//! track Anthropic-style prompt caching.
//!
//! Cost is stored as a string (`Decimal::to_string`) rather than an `f64`
//! because downstream billing / reconciliation pipelines require exact
//! decimal fidelity; converting through `f64` would leak rounding error.

use rust_decimal::Decimal;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tracing::{Level, Span, field, span};

/// Tracing configuration.
#[derive(Clone, Default)]
pub struct TracingConfig {
    pub service_name: Option<String>,
    pub enabled: bool,
    pub level: TracingLevel,
}

#[non_exhaustive]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum TracingLevel {
    #[default]
    Info,
    Debug,
    Trace,
}

impl TracingConfig {
    pub fn new() -> Self {
        Self {
            enabled: true,
            ..Default::default()
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

/// Context for creating structured spans.
pub struct SpanContext {
    session_id: String,
    request_id: AtomicU64,
}

impl SpanContext {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            request_id: AtomicU64::new(0),
        }
    }

    pub fn next_request_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn agent_execute_span(&self, model: &str) -> Span {
        let request_id = self.next_request_id();
        span!(
            Level::INFO,
            "agent.execute",
            session_id = %self.session_id,
            request_id = request_id,
            model = model,
            otel.name = "agent.execute",
        )
    }

    pub fn api_call_span(&self, model: &str) -> ApiCallSpan {
        ApiCallSpan::new(model)
    }

    pub fn tool_execute_span(&self, tool_name: &str, tool_call_id: &str) -> Span {
        span!(
            Level::INFO,
            "tool.execute",
            tool_name = tool_name,
            tool_call_id = tool_call_id,
            session_id = %self.session_id,
            otel.name = format!("tool.{}", tool_name),
            is_error = field::Empty,
            duration_ms = field::Empty,
        )
    }
}

/// Helper for tracking API call metrics within a span.
///
/// Recorded attributes follow OpenTelemetry GenAI semantic conventions:
///
/// | Attribute                              | Method                                          |
/// |----------------------------------------|-------------------------------------------------|
/// | `gen_ai.request.model`                 | set at construction                             |
/// | `gen_ai.usage.input_tokens`            | [`record_usage`][Self::record_usage]            |
/// | `gen_ai.usage.output_tokens`           | [`record_usage`][Self::record_usage]            |
/// | `gen_ai.usage.reasoning_tokens`        | [`record_reasoning_tokens`][Self::record_reasoning_tokens] |
/// | `gen_ai.usage.cache_read_tokens`       | [`record_cache`][Self::record_cache]            |
/// | `gen_ai.usage.cache_creation_tokens`   | [`record_cache`][Self::record_cache]            |
/// | `gen_ai.usage.cost_usd`                | [`record_cost`][Self::record_cost]              |
/// | `gen_ai.latency_ms`                    | [`finish`][Self::finish]                        |
pub struct ApiCallSpan {
    span: Span,
    start: Instant,
}

impl ApiCallSpan {
    pub fn new(model: &str) -> Self {
        Self::with_system(model, "")
    }

    /// Create an API call span with an explicit `gen_ai.system` label
    /// (i.e. the codec id: `anthropic-messages`, `openai-chat`,
    /// `openai-responses`, `gemini-generate`, `bedrock-converse`).
    ///
    /// Use this variant from `ProviderClient::send` so the span carries
    /// the provider identity alongside the model name — downstream OTel
    /// dashboards can then group by provider without scraping the model
    /// string.
    pub fn with_system(model: &str, system: &str) -> Self {
        let span = span!(
            Level::INFO,
            "api.call",
            "gen_ai.request.model" = model,
            "gen_ai.system" = system,
            otel.name = "api.call",
            "gen_ai.usage.input_tokens" = field::Empty,
            "gen_ai.usage.output_tokens" = field::Empty,
            "gen_ai.usage.reasoning_tokens" = field::Empty,
            "gen_ai.usage.cache_read_tokens" = field::Empty,
            "gen_ai.usage.cache_creation_tokens" = field::Empty,
            "gen_ai.usage.cost_usd" = field::Empty,
            "gen_ai.latency_ms" = field::Empty,
            "error.category" = field::Empty,
            "otel.status_code" = field::Empty,
        );
        Self {
            span,
            start: Instant::now(),
        }
    }

    /// Record the canonical input/output token counts from the provider
    /// response. Safe to call at most once per span; subsequent calls
    /// overwrite the previously-recorded values (tracing only keeps the
    /// latest `record` per field).
    pub fn record_usage(&self, input_tokens: u64, output_tokens: u64) {
        self.span.record("gen_ai.usage.input_tokens", input_tokens);
        self.span
            .record("gen_ai.usage.output_tokens", output_tokens);
    }

    /// Record reasoning token count (o-series, Gemini thinking, etc.).
    ///
    /// Only providers that expose an explicit reasoning-tokens count
    /// call this — Anthropic extended thinking, for example, does not
    /// return a separate reasoning-tokens field and so this attribute
    /// is omitted entirely for Anthropic requests.
    pub fn record_reasoning_tokens(&self, reasoning_tokens: u64) {
        self.span
            .record("gen_ai.usage.reasoning_tokens", reasoning_tokens);
    }

    pub fn record_cache(&self, read_tokens: u64, creation_tokens: u64) {
        self.span
            .record("gen_ai.usage.cache_read_tokens", read_tokens);
        self.span
            .record("gen_ai.usage.cache_creation_tokens", creation_tokens);
    }

    /// Record the request cost in USD.
    ///
    /// Stored as the `Decimal`'s canonical string form so downstream
    /// billing pipelines can parse it back without f64 round-trip
    /// error. Callers compute the cost from the model's rate card plus
    /// the usage figures returned by the provider.
    pub fn record_cost(&self, cost_usd: Decimal) {
        self.span
            .record("gen_ai.usage.cost_usd", cost_usd.to_string().as_str());
    }

    /// Mark the span as failed and record a coarse failure
    /// classification. Sets both the span-level `otel.status_code` to
    /// `"ERROR"` and an `error.category` attribute matching the stable
    /// vocabulary exported by [`crate::FailureCategory::as_str`].
    ///
    /// Call this before [`finish`][Self::finish] when the underlying
    /// HTTP request or codec pipeline returned an `Err`. Downstream
    /// observability (Honeycomb, Grafana, OTel collectors) can then
    /// group failures by category without inspecting the error
    /// message.
    pub fn record_error(&self, category: crate::FailureCategory) {
        self.span.record("error.category", category.as_str());
        self.span.record("otel.status_code", "ERROR");
    }

    pub fn finish(self) {
        let latency_ms = self.start.elapsed().as_millis() as u64;
        self.span.record("gen_ai.latency_ms", latency_ms);
    }

    pub fn span(&self) -> &Span {
        &self.span
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_span_context() {
        let span_context = SpanContext::new("test-session");
        assert_eq!(span_context.next_request_id(), 0);
        assert_eq!(span_context.next_request_id(), 1);
    }

    #[test]
    fn test_api_call_span_records_usage_cache_and_finish() {
        let span = ApiCallSpan::new("claude-sonnet-4-5");
        span.record_usage(100, 50);
        span.record_cache(20, 10);
        span.finish();
    }

    #[test]
    fn test_api_call_span_records_reasoning_tokens() {
        // For providers that expose reasoning tokens separately
        // (OpenAI o-series, Gemini thinking config). Smoke-test only —
        // the tracing subscriber does not capture recorded fields in
        // unit tests; we verify the API compiles and does not panic.
        let span = ApiCallSpan::new("gemini-2.5-flash");
        span.record_usage(200, 80);
        span.record_reasoning_tokens(150);
        span.finish();
    }

    #[test]
    fn test_api_call_span_records_cost() {
        // Cost is a rust_decimal::Decimal so we preserve the exact
        // string form across the OTel attribute boundary.
        let span = ApiCallSpan::new("claude-sonnet-4-5");
        span.record_usage(1000, 500);
        span.record_cost(dec!(0.00345));
        span.finish();
    }

    #[test]
    fn test_api_call_span_all_attributes_together() {
        let span = ApiCallSpan::new("claude-opus-4-6");
        span.record_usage(2048, 512);
        span.record_cache(128, 64);
        span.record_reasoning_tokens(256);
        span.record_cost(dec!(0.0789));
        span.finish();
    }
}
