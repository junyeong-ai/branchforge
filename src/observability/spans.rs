//! Structured span definitions for tracing.

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
pub struct ApiCallSpan {
    span: Span,
    start: Instant,
}

impl ApiCallSpan {
    pub fn new(model: &str) -> Self {
        let span = span!(
            Level::INFO,
            "api.call",
            model = model,
            otel.name = "api.call",
            input_tokens = field::Empty,
            output_tokens = field::Empty,
            latency_ms = field::Empty,
            cache_read_tokens = field::Empty,
            cache_creation_tokens = field::Empty,
        );
        Self {
            span,
            start: Instant::now(),
        }
    }

    pub fn record_usage(&self, input_tokens: u32, output_tokens: u32) {
        self.span.record("input_tokens", input_tokens);
        self.span.record("output_tokens", output_tokens);
    }

    pub fn record_cache(&self, read_tokens: u32, creation_tokens: u32) {
        self.span.record("cache_read_tokens", read_tokens);
        self.span.record("cache_creation_tokens", creation_tokens);
    }

    pub fn finish(self) {
        let latency_ms = self.start.elapsed().as_millis() as u64;
        self.span.record("latency_ms", latency_ms);
    }

    pub fn span(&self) -> &Span {
        &self.span
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_context() {
        let span_context = SpanContext::new("test-session");
        assert_eq!(span_context.next_request_id(), 0);
        assert_eq!(span_context.next_request_id(), 1);
    }

    #[test]
    fn test_api_call_span() {
        let span = ApiCallSpan::new("claude-sonnet-4-5");
        span.record_usage(100, 50);
        span.record_cache(20, 10);
        span.finish();
    }
}
