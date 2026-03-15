//! Observability module for Branchforge SDK.
//!
//! Provides structured tracing, metrics collection, and telemetry integration.
//!
//! ## Features
//!
//! - **Built-in metrics**: Counter, Gauge, Histogram for local tracking
//! - **Structured spans**: Tracing integration for request/tool execution
//! - **OpenTelemetry** (optional): Export to OTLP-compatible backends
//!
//! ## OpenTelemetry Integration
//!
//! Enable the `otel` feature to export traces and metrics:
//!
//! ```toml
//! branchforge = { version = "0.2", features = ["otel"] }
//! ```
//!
//! ```rust,ignore
//! use branchforge::observability::{OtelConfig, OtelRuntime};
//!
//! let config = OtelConfig::new("my-agent")
//!     .with_endpoint("http://localhost:4317");
//!
//! let runtime = OtelRuntime::init(&config)?;
//! // ... use the agent ...
//! runtime.shutdown(); // Flush before exit
//! ```

mod metrics;
#[cfg(feature = "otel")]
mod otel;
mod spans;

pub use metrics::{Counter, Gauge, Histogram, MetricsConfig, MetricsRegistry, MetricsSummary};
#[cfg(feature = "otel")]
pub use otel::{
    OtelConfig, OtelError, OtelRuntime, SERVICE_NAME_DEFAULT, init_tracing_subscriber, semantic,
};
pub use spans::{ApiCallSpan, SpanContext, TracingConfig, TracingLevel};

use std::sync::Arc;

/// Observability configuration combining tracing and metrics.
#[derive(Clone, Default)]
pub struct ObservabilityConfig {
    pub tracing: TracingConfig,
    pub metrics: MetricsConfig,
    #[cfg(feature = "otel")]
    pub otel: Option<OtelConfig>,
}

impl ObservabilityConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tracing(mut self, config: TracingConfig) -> Self {
        self.tracing = config;
        self
    }

    pub fn metrics(mut self, config: MetricsConfig) -> Self {
        self.metrics = config;
        self
    }

    pub fn service_name(mut self, name: impl Into<String>) -> Self {
        self.tracing.service_name = Some(name.into());
        self
    }

    #[cfg(feature = "otel")]
    pub fn otel(mut self, config: OtelConfig) -> Self {
        self.otel = Some(config);
        self
    }

    pub fn build_registry(&self) -> Arc<MetricsRegistry> {
        #[cfg(feature = "otel")]
        if let Some(ref otel_config) = self.otel {
            return Arc::new(MetricsRegistry::otel(&self.metrics, otel_config));
        }

        Arc::new(MetricsRegistry::new(&self.metrics))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observability_config() {
        let config = ObservabilityConfig::new()
            .service_name("test-agent")
            .metrics(MetricsConfig::default());

        assert_eq!(config.tracing.service_name, Some("test-agent".to_string()));
    }

    #[cfg(feature = "otel")]
    #[test]
    fn test_observability_with_otel() {
        let config = ObservabilityConfig::new()
            .service_name("test-agent")
            .otel(OtelConfig::new("test-agent"));

        assert!(config.otel.is_some());
    }
}
