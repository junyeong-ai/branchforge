//! Metrics collection and export.
//!
//! Provides built-in atomic metrics for local tracking, with optional
//! OpenTelemetry export when the `otel` feature is enabled.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use rust_decimal::Decimal;

use crate::budget::COST_SCALE_FACTOR;

#[cfg(feature = "otel")]
use super::otel::{OtelConfig, OtelMetricsBridge, SERVICE_NAME_DEFAULT};
#[cfg(feature = "otel")]
use opentelemetry::global;

/// Metrics configuration.
#[derive(Clone, Default)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub export_interval: Option<Duration>,
}

impl MetricsConfig {
    pub fn new() -> Self {
        Self {
            enabled: true,
            export_interval: Some(Duration::from_secs(60)),
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            export_interval: None,
        }
    }
}

/// Thread-safe atomic counter.
#[derive(Default)]
pub struct Counter {
    value: AtomicU64,
}

impl Counter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// Thread-safe atomic gauge.
#[derive(Default)]
pub struct Gauge {
    value: AtomicI64,
}

impl Gauge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, value: i64) {
        self.value.store(value, Ordering::Relaxed);
    }

    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec(&self) {
        self.value.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn get(&self) -> i64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// Simple histogram using fixed buckets.
pub struct Histogram {
    buckets: Vec<AtomicU64>,
    bucket_bounds: Vec<f64>,
    sum: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    pub fn new(bucket_bounds: Vec<f64>) -> Self {
        let buckets = (0..=bucket_bounds.len())
            .map(|_| AtomicU64::new(0))
            .collect();
        Self {
            buckets,
            bucket_bounds,
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    pub fn default_latency() -> Self {
        Self::new(vec![
            10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0,
        ])
    }

    pub fn observe(&self, value: f64) {
        let bucket_idx = self
            .bucket_bounds
            .iter()
            .position(|&bound| value <= bound)
            .unwrap_or(self.bucket_bounds.len());

        self.buckets[bucket_idx].fetch_add(1, Ordering::Relaxed);
        self.sum
            .fetch_add((value * 1000.0) as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    pub fn sum(&self) -> u64 {
        self.sum.load(Ordering::Relaxed)
    }

    /// Returns the sum as a floating-point value in the original unit (ms).
    ///
    /// The internal sum is stored scaled by 1000x to preserve sub-integer
    /// precision. This method converts back to the original scale.
    pub fn sum_ms(&self) -> f64 {
        self.sum.load(Ordering::Relaxed) as f64 / 1000.0
    }

    /// Estimate the value at the given percentile in [0.0, 1.0] using
    /// bucket upper bounds. Returns `None` if the histogram is empty.
    ///
    /// The estimate is the upper bound of the bucket that contains the
    /// `ceil(p * count)`-th observation. This is the standard "upper bound"
    /// estimator used by Prometheus-style histograms — it never under-
    /// estimates a true percentile and is precise within a bucket width.
    pub fn percentile(&self, p: f64) -> Option<f64> {
        debug_assert!(
            (0.0..=1.0).contains(&p),
            "percentile must be in [0.0, 1.0], got {p}"
        );
        let total = self.count();
        if total == 0 {
            return None;
        }
        let target = ((total as f64) * p).ceil() as u64;
        let target = target.max(1);
        let mut cumulative = 0u64;
        for (i, bucket) in self.buckets.iter().enumerate() {
            cumulative += bucket.load(Ordering::Relaxed);
            if cumulative >= target {
                return self
                    .bucket_bounds
                    .get(i)
                    .copied()
                    .or_else(|| self.bucket_bounds.last().copied());
            }
        }
        self.bucket_bounds.last().copied()
    }

    /// 50th-percentile (median) estimate.
    pub fn p50(&self) -> Option<f64> {
        self.percentile(0.50)
    }

    /// 95th-percentile estimate.
    pub fn p95(&self) -> Option<f64> {
        self.percentile(0.95)
    }

    /// 99th-percentile estimate.
    pub fn p99(&self) -> Option<f64> {
        self.percentile(0.99)
    }

    /// Mean of all observations, in the original unit (ms).
    pub fn mean(&self) -> Option<f64> {
        let count = self.count();
        if count == 0 {
            None
        } else {
            Some(self.sum_ms() / count as f64)
        }
    }
}

/// Agent-specific metrics registry.
///
/// Tracks metrics locally with atomic counters, and optionally exports
/// to OpenTelemetry when the `otel` feature is enabled.
pub struct MetricsRegistry {
    pub requests_total: Counter,
    pub requests_success: Counter,
    pub requests_error: Counter,
    pub tokens_input: Counter,
    pub tokens_output: Counter,
    pub cache_read_tokens: Counter,
    pub cache_creation_tokens: Counter,
    pub tool_calls_total: Counter,
    pub tool_errors: Counter,
    pub active_sessions: Gauge,
    pub request_latency_ms: Histogram,
    pub cost_total_micros: Counter,
    #[cfg(feature = "otel")]
    otel_bridge: Option<OtelMetricsBridge>,
}

impl MetricsRegistry {
    pub fn new(_config: &MetricsConfig) -> Self {
        Self {
            requests_total: Counter::new(),
            requests_success: Counter::new(),
            requests_error: Counter::new(),
            tokens_input: Counter::new(),
            tokens_output: Counter::new(),
            cache_read_tokens: Counter::new(),
            cache_creation_tokens: Counter::new(),
            tool_calls_total: Counter::new(),
            tool_errors: Counter::new(),
            active_sessions: Gauge::new(),
            request_latency_ms: Histogram::default_latency(),
            cost_total_micros: Counter::new(),
            #[cfg(feature = "otel")]
            otel_bridge: None,
        }
    }

    #[cfg(feature = "otel")]
    pub fn otel(_config: &MetricsConfig, otel_config: &OtelConfig) -> Self {
        let meter = global::meter(SERVICE_NAME_DEFAULT);
        let bridge = OtelMetricsBridge::new(&meter);
        let _ = &otel_config.service_name; // Used for configuration, meter uses static name

        Self {
            requests_total: Counter::new(),
            requests_success: Counter::new(),
            requests_error: Counter::new(),
            tokens_input: Counter::new(),
            tokens_output: Counter::new(),
            cache_read_tokens: Counter::new(),
            cache_creation_tokens: Counter::new(),
            tool_calls_total: Counter::new(),
            tool_errors: Counter::new(),
            active_sessions: Gauge::new(),
            request_latency_ms: Histogram::default_latency(),
            cost_total_micros: Counter::new(),
            otel_bridge: Some(bridge),
        }
    }

    pub fn record_request_start(&self) {
        self.requests_total.inc();
        self.active_sessions.inc();

        #[cfg(feature = "otel")]
        if let Some(ref bridge) = self.otel_bridge {
            bridge.record_request_start();
        }
    }

    pub fn record_request_end(&self, success: bool, latency_ms: f64) {
        self.active_sessions.dec();
        self.request_latency_ms.observe(latency_ms);
        if success {
            self.requests_success.inc();
        } else {
            self.requests_error.inc();
        }

        #[cfg(feature = "otel")]
        if let Some(ref bridge) = self.otel_bridge {
            bridge.record_request_end(success, latency_ms);
        }
    }

    pub fn record_tokens(&self, input: u64, output: u64) {
        self.tokens_input.add(input);
        self.tokens_output.add(output);

        #[cfg(feature = "otel")]
        if let Some(ref bridge) = self.otel_bridge {
            bridge.record_tokens(input, output);
        }
    }

    pub fn record_cache(&self, read: u64, creation: u64) {
        self.cache_read_tokens.add(read);
        self.cache_creation_tokens.add(creation);

        #[cfg(feature = "otel")]
        if let Some(ref bridge) = self.otel_bridge {
            bridge.record_cache(read, creation);
        }
    }

    pub fn record_tool_call(&self, success: bool) {
        self.tool_calls_total.inc();
        if !success {
            self.tool_errors.inc();
        }

        #[cfg(feature = "otel")]
        if let Some(ref bridge) = self.otel_bridge {
            bridge.record_tool_call(success);
        }
    }

    pub fn record_cost(&self, cost_usd: Decimal) {
        let scaled = cost_usd * COST_SCALE_FACTOR;
        let micros = scaled
            .try_into()
            .unwrap_or_else(|_| scaled.to_string().parse::<u64>().unwrap_or(0));
        self.cost_total_micros.add(micros);

        #[cfg(feature = "otel")]
        if let Some(ref bridge) = self.otel_bridge {
            bridge.record_cost(cost_usd);
        }
    }

    pub fn total_cost_usd(&self) -> Decimal {
        Decimal::from(self.cost_total_micros.get()) / COST_SCALE_FACTOR
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new(&MetricsConfig::default())
    }
}

/// High-level metrics summary for an agent session.
///
/// This is a snapshot summary for export/display purposes, derived from
/// the MetricsRegistry. For detailed per-agent metrics with tool stats
/// and call records, see `crate::agent::AgentMetrics`.
#[derive(Debug, Clone, Default)]
pub struct MetricsSummary {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub total_tool_calls: u64,
    pub failed_tool_calls: u64,
    pub total_cost_usd: Decimal,
    /// Mean request latency in milliseconds (`None` if no observations).
    pub avg_latency_ms: Option<f64>,
    /// 50th-percentile request latency in milliseconds.
    pub p50_latency_ms: Option<f64>,
    /// 95th-percentile request latency in milliseconds.
    pub p95_latency_ms: Option<f64>,
    /// 99th-percentile request latency in milliseconds.
    pub p99_latency_ms: Option<f64>,
}

impl MetricsSummary {
    pub fn from_registry(registry: &MetricsRegistry) -> Self {
        let hist = &registry.request_latency_ms;
        Self {
            total_requests: registry.requests_total.get(),
            successful_requests: registry.requests_success.get(),
            failed_requests: registry.requests_error.get(),
            total_input_tokens: registry.tokens_input.get(),
            total_output_tokens: registry.tokens_output.get(),
            cache_read_tokens: registry.cache_read_tokens.get(),
            cache_creation_tokens: registry.cache_creation_tokens.get(),
            total_tool_calls: registry.tool_calls_total.get(),
            failed_tool_calls: registry.tool_errors.get(),
            total_cost_usd: registry.total_cost_usd(),
            avg_latency_ms: hist.mean(),
            p50_latency_ms: hist.p50(),
            p95_latency_ms: hist.p95(),
            p99_latency_ms: hist.p99(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_counter() {
        let counter = Counter::new();
        assert_eq!(counter.get(), 0);
        counter.inc();
        assert_eq!(counter.get(), 1);
        counter.add(5);
        assert_eq!(counter.get(), 6);
    }

    #[test]
    fn test_gauge() {
        let gauge = Gauge::new();
        gauge.set(10);
        assert_eq!(gauge.get(), 10);
        gauge.inc();
        assert_eq!(gauge.get(), 11);
        gauge.dec();
        assert_eq!(gauge.get(), 10);
        // Verify no underflow panic with negative values
        gauge.set(0);
        gauge.dec();
        assert_eq!(gauge.get(), -1);
    }

    #[test]
    fn test_histogram() {
        let hist = Histogram::new(vec![10.0, 50.0, 100.0]);
        hist.observe(5.0);
        hist.observe(25.0);
        hist.observe(75.0);
        hist.observe(150.0);
        assert_eq!(hist.count(), 4);
    }

    #[test]
    fn test_histogram_percentile_empty() {
        let hist = Histogram::new(vec![10.0, 50.0]);
        assert_eq!(hist.percentile(0.5), None);
        assert_eq!(hist.p50(), None);
        assert_eq!(hist.mean(), None);
    }

    #[test]
    fn test_histogram_percentile_single_value() {
        let hist = Histogram::new(vec![10.0, 50.0, 100.0]);
        hist.observe(25.0);
        // Single observation in bucket [10, 50] → p50/p95/p99 all return 50.
        assert_eq!(hist.p50(), Some(50.0));
        assert_eq!(hist.p95(), Some(50.0));
        assert_eq!(hist.p99(), Some(50.0));
        assert_eq!(hist.mean(), Some(25.0));
    }

    #[test]
    fn test_histogram_percentile_distribution() {
        let hist = Histogram::new(vec![10.0, 50.0, 100.0, 500.0]);
        // 100 observations evenly spread: 25 each in (0..=10), (10..=50),
        // (50..=100), (100..=500).
        for _ in 0..25 {
            hist.observe(5.0);
        }
        for _ in 0..25 {
            hist.observe(30.0);
        }
        for _ in 0..25 {
            hist.observe(75.0);
        }
        for _ in 0..25 {
            hist.observe(300.0);
        }
        assert_eq!(hist.count(), 100);
        // p50: 50th observation lands in bucket [10, 50] → upper bound 50
        assert_eq!(hist.p50(), Some(50.0));
        // p95: 95th observation lands in bucket [100, 500] → upper bound 500
        assert_eq!(hist.p95(), Some(500.0));
        // p99: same bucket
        assert_eq!(hist.p99(), Some(500.0));
    }

    #[test]
    fn test_metrics_registry() {
        let registry = MetricsRegistry::default();
        registry.record_request_start();
        registry.record_tokens(100, 50);
        registry.record_tool_call(true);
        registry.record_cost(dec!(0.001));
        registry.record_request_end(true, 250.0);

        let metrics = MetricsSummary::from_registry(&registry);
        assert_eq!(metrics.total_requests, 1);
        assert_eq!(metrics.total_input_tokens, 100);
        assert_eq!(metrics.total_output_tokens, 50);
        assert_eq!(metrics.total_tool_calls, 1);
    }
}
