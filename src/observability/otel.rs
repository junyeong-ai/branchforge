//! OpenTelemetry integration for tracing and metrics export.
//!
//! This module provides OpenTelemetry SDK initialization and configuration
//! for exporting traces and metrics to OTLP-compatible backends.

use std::time::Duration;

use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{MetricExporter, Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{RandomIdGenerator, Sampler, SdkTracerProvider};
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_VERSION};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Default service name for OpenTelemetry instrumentation.
pub const SERVICE_NAME_DEFAULT: &str = "branchforge";

/// OpenTelemetry configuration for the agent.
#[derive(Debug, Clone)]
pub struct OtelConfig {
    pub service_name: String,
    pub service_version: Option<String>,
    pub otlp_endpoint: String,
    pub traces_enabled: bool,
    pub metrics_enabled: bool,
    pub metrics_export_interval: Duration,
    pub sample_ratio: f64,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            service_name: SERVICE_NAME_DEFAULT.to_string(),
            service_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            otlp_endpoint: "http://localhost:4317".to_string(),
            traces_enabled: true,
            metrics_enabled: true,
            metrics_export_interval: Duration::from_secs(60),
            sample_ratio: 1.0,
        }
    }
}

impl OtelConfig {
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            ..Default::default()
        }
    }

    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.otlp_endpoint = endpoint.into();
        self
    }

    pub fn service_version(mut self, version: impl Into<String>) -> Self {
        self.service_version = Some(version.into());
        self
    }

    pub fn traces(mut self, enabled: bool) -> Self {
        self.traces_enabled = enabled;
        self
    }

    pub fn metrics(mut self, enabled: bool) -> Self {
        self.metrics_enabled = enabled;
        self
    }

    pub fn metrics_interval(mut self, interval: Duration) -> Self {
        self.metrics_export_interval = interval;
        self
    }

    pub fn sample_ratio(mut self, ratio: f64) -> Self {
        self.sample_ratio = ratio.clamp(0.0, 1.0);
        self
    }

    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
            config.otlp_endpoint = endpoint;
        }

        if let Ok(name) = std::env::var("OTEL_SERVICE_NAME") {
            config.service_name = name;
        }

        if let Ok(ratio) = std::env::var("OTEL_TRACES_SAMPLER_ARG")
            && let Ok(r) = ratio.parse::<f64>()
        {
            config.sample_ratio = r.clamp(0.0, 1.0);
        }

        config
    }

    fn build_resource(&self) -> Resource {
        let mut attributes = vec![KeyValue::new(SERVICE_NAME, self.service_name.clone())];

        if let Some(ref version) = self.service_version {
            attributes.push(KeyValue::new(SERVICE_VERSION, version.clone()));
        }

        Resource::builder().with_attributes(attributes).build()
    }
}

/// OpenTelemetry runtime handle.
///
/// Holds references to the tracer and meter providers.
/// Call `shutdown()` before application exit to flush pending data.
pub struct OtelRuntime {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl OtelRuntime {
    /// Initialize OpenTelemetry with the given configuration.
    pub fn init(config: &OtelConfig) -> Result<Self, OtelError> {
        global::set_text_map_propagator(TraceContextPropagator::new());

        let resource = config.build_resource();

        let tracer_provider = if config.traces_enabled {
            Some(Self::init_tracer(config, resource.clone())?)
        } else {
            None
        };

        let meter_provider = if config.metrics_enabled {
            Some(Self::init_metrics(config, resource)?)
        } else {
            None
        };

        Ok(Self {
            tracer_provider,
            meter_provider,
        })
    }

    fn init_tracer(
        config: &OtelConfig,
        resource: Resource,
    ) -> Result<SdkTracerProvider, OtelError> {
        let exporter = SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{}/v1/traces", config.otlp_endpoint))
            .build()
            .map_err(|e| OtelError::Init(format!("Failed to create span exporter: {}", e)))?;

        let sampler = if config.sample_ratio >= 1.0 {
            Sampler::AlwaysOn
        } else if config.sample_ratio <= 0.0 {
            Sampler::AlwaysOff
        } else {
            Sampler::TraceIdRatioBased(config.sample_ratio)
        };

        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_sampler(sampler)
            .with_id_generator(RandomIdGenerator::default())
            .with_resource(resource)
            .build();

        global::set_tracer_provider(provider.clone());

        Ok(provider)
    }

    fn init_metrics(
        config: &OtelConfig,
        resource: Resource,
    ) -> Result<SdkMeterProvider, OtelError> {
        let exporter = MetricExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{}/v1/metrics", config.otlp_endpoint))
            .build()
            .map_err(|e| OtelError::Init(format!("Failed to create metric exporter: {}", e)))?;

        let reader = PeriodicReader::builder(exporter)
            .with_interval(config.metrics_export_interval)
            .build();

        let provider = SdkMeterProvider::builder()
            .with_reader(reader)
            .with_resource(resource)
            .build();

        global::set_meter_provider(provider.clone());

        Ok(provider)
    }

    /// Get the global meter for recording metrics.
    pub fn meter(&self, name: &'static str) -> opentelemetry::metrics::Meter {
        global::meter(name)
    }

    /// Shutdown the OpenTelemetry runtime, flushing any pending data.
    pub fn shutdown(self) {
        if let Some(provider) = self.tracer_provider
            && let Err(e) = provider.shutdown()
        {
            tracing::warn!("Failed to shutdown tracer provider: {:?}", e);
        }

        if let Some(provider) = self.meter_provider
            && let Err(e) = provider.shutdown()
        {
            tracing::warn!("Failed to shutdown meter provider: {:?}", e);
        }
    }
}

/// Initialize tracing subscriber with OpenTelemetry layer.
///
/// This sets up the global tracing subscriber with:
/// - Console output (if enabled)
/// - OpenTelemetry trace export
pub fn init_tracing_subscriber(config: &OtelConfig, with_console: bool) -> Result<(), OtelError> {
    let resource = config.build_resource();

    let exporter = SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(format!("{}/v1/traces", config.otlp_endpoint))
        .build()
        .map_err(|e| OtelError::Init(format!("Failed to create span exporter: {}", e)))?;

    let sampler = if config.sample_ratio >= 1.0 {
        Sampler::AlwaysOn
    } else if config.sample_ratio <= 0.0 {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(config.sample_ratio)
    };

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(sampler)
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource)
        .build();

    global::set_tracer_provider(provider);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if with_console {
        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_thread_ids(false)
            .with_file(false);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .with(OpenTelemetryLayer::new(global::tracer(
                SERVICE_NAME_DEFAULT,
            )))
            .try_init()
            .map_err(|e| OtelError::Init(format!("Failed to init subscriber: {}", e)))?;
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(OpenTelemetryLayer::new(global::tracer(
                SERVICE_NAME_DEFAULT,
            )))
            .try_init()
            .map_err(|e| OtelError::Init(format!("Failed to init subscriber: {}", e)))?;
    }

    Ok(())
}

/// Errors that can occur during OpenTelemetry initialization.
#[derive(Debug, thiserror::Error)]
pub enum OtelError {
    #[error("OpenTelemetry initialization failed: {0}")]
    Init(String),
}

/// Semantic conventions for agent-specific attributes.
pub mod semantic {
    pub const AGENT_SESSION_ID: &str = "agent.session.id";
    pub const AGENT_MODEL: &str = "agent.model";
    pub const AGENT_REQUEST_ID: &str = "agent.request.id";
    pub const AGENT_TOOL_NAME: &str = "agent.tool.name";
    pub const AGENT_TOOL_USE_ID: &str = "agent.tool.use_id";
    pub const AGENT_INPUT_TOKENS: &str = "agent.tokens.input";
    pub const AGENT_OUTPUT_TOKENS: &str = "agent.tokens.output";
    pub const AGENT_CACHE_READ_TOKENS: &str = "agent.tokens.cache_read";
    pub const AGENT_CACHE_CREATION_TOKENS: &str = "agent.tokens.cache_creation";
    pub const AGENT_COST_USD: &str = "agent.cost.usd";
}

/// OpenTelemetry metrics bridge for the built-in MetricsRegistry.
pub struct OtelMetricsBridge {
    requests_total: opentelemetry::metrics::Counter<u64>,
    requests_success: opentelemetry::metrics::Counter<u64>,
    requests_error: opentelemetry::metrics::Counter<u64>,
    tokens_input: opentelemetry::metrics::Counter<u64>,
    tokens_output: opentelemetry::metrics::Counter<u64>,
    cache_read_tokens: opentelemetry::metrics::Counter<u64>,
    cache_creation_tokens: opentelemetry::metrics::Counter<u64>,
    tool_calls_total: opentelemetry::metrics::Counter<u64>,
    tool_errors: opentelemetry::metrics::Counter<u64>,
    active_sessions: opentelemetry::metrics::UpDownCounter<i64>,
    request_latency: opentelemetry::metrics::Histogram<f64>,
    cost_total: opentelemetry::metrics::Counter<f64>,
}

impl OtelMetricsBridge {
    pub fn new(meter: &opentelemetry::metrics::Meter) -> Self {
        Self {
            requests_total: meter
                .u64_counter("agent.requests.total")
                .with_description("Total number of API requests")
                .build(),
            requests_success: meter
                .u64_counter("agent.requests.success")
                .with_description("Number of successful API requests")
                .build(),
            requests_error: meter
                .u64_counter("agent.requests.error")
                .with_description("Number of failed API requests")
                .build(),
            tokens_input: meter
                .u64_counter("agent.tokens.input")
                .with_description("Total input tokens consumed")
                .build(),
            tokens_output: meter
                .u64_counter("agent.tokens.output")
                .with_description("Total output tokens generated")
                .build(),
            cache_read_tokens: meter
                .u64_counter("agent.tokens.cache_read")
                .with_description("Total cache read tokens")
                .build(),
            cache_creation_tokens: meter
                .u64_counter("agent.tokens.cache_creation")
                .with_description("Total cache creation tokens")
                .build(),
            tool_calls_total: meter
                .u64_counter("agent.tool_calls.total")
                .with_description("Total number of tool calls")
                .build(),
            tool_errors: meter
                .u64_counter("agent.tool_calls.error")
                .with_description("Number of failed tool calls")
                .build(),
            active_sessions: meter
                .i64_up_down_counter("agent.sessions.active")
                .with_description("Number of active sessions")
                .build(),
            request_latency: meter
                .f64_histogram("agent.request.latency")
                .with_description("Request latency in milliseconds")
                .with_unit("ms")
                .build(),
            cost_total: meter
                .f64_counter("agent.cost.total")
                .with_description("Total cost in USD")
                .with_unit("USD")
                .build(),
        }
    }

    pub fn record_request_start(&self) {
        self.requests_total.add(1, &[]);
        self.active_sessions.add(1, &[]);
    }

    pub fn record_request_end(&self, success: bool, latency_ms: f64) {
        self.active_sessions.add(-1, &[]);
        self.request_latency.record(latency_ms, &[]);
        if success {
            self.requests_success.add(1, &[]);
        } else {
            self.requests_error.add(1, &[]);
        }
    }

    pub fn record_tokens(&self, input: u64, output: u64) {
        self.tokens_input.add(input, &[]);
        self.tokens_output.add(output, &[]);
    }

    pub fn record_cache(&self, read: u64, creation: u64) {
        self.cache_read_tokens.add(read, &[]);
        self.cache_creation_tokens.add(creation, &[]);
    }

    pub fn record_tool_call(&self, success: bool) {
        self.tool_calls_total.add(1, &[]);
        if !success {
            self.tool_errors.add(1, &[]);
        }
    }

    pub fn record_cost(&self, cost_usd: Decimal) {
        // Convert Decimal to f64 at OpenTelemetry boundary
        let cost_f64 = cost_usd.to_f64().unwrap_or(0.0);
        self.cost_total.add(cost_f64, &[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_otel_config_default() {
        let config = OtelConfig::default();
        assert_eq!(config.service_name, "branchforge");
        assert!(config.traces_enabled);
        assert!(config.metrics_enabled);
        assert_eq!(config.sample_ratio, 1.0);
    }

    #[test]
    fn test_otel_config_builder() {
        let config = OtelConfig::new("my-agent")
            .endpoint("http://otel-collector:4317")
            .sample_ratio(0.5)
            .metrics_interval(Duration::from_secs(30));

        assert_eq!(config.service_name, "my-agent");
        assert_eq!(config.otlp_endpoint, "http://otel-collector:4317");
        assert_eq!(config.sample_ratio, 0.5);
        assert_eq!(config.metrics_export_interval, Duration::from_secs(30));
    }

    #[test]
    fn test_sample_ratio_clamping() {
        let config = OtelConfig::default().sample_ratio(1.5);
        assert_eq!(config.sample_ratio, 1.0);

        let config = OtelConfig::default().sample_ratio(-0.5);
        assert_eq!(config.sample_ratio, 0.0);
    }
}
