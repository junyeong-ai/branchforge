//! Prompt cache break detection.
//!
//! Monitors cache hit rates across consecutive API calls and detects
//! sudden drops that indicate prompt cache invalidation. Emits events
//! via the [`EventBus`](crate::events::EventBus) when breaks are detected.

use std::collections::VecDeque;
use std::sync::Arc;

use tracing::warn;

use crate::events::{EventBus, EventKind};
use crate::types::TokenUsage;

/// Default number of recent samples to track.
const DEFAULT_WINDOW_SIZE: usize = 5;

/// Default drop threshold (fraction) to consider a cache break.
/// If hit rate drops by more than this from the rolling average, it's a break.
const DEFAULT_DROP_THRESHOLD: f64 = 0.3;

/// Monitors prompt cache hit rates and detects breaks.
///
/// A "cache break" occurs when the cache hit rate drops significantly
/// between consecutive API calls, indicating that the cached prompt
/// prefix was invalidated (e.g., by system prompt changes, tool
/// definition reordering, or message modifications).
///
/// # Usage
///
/// ```rust
/// use branchforge::tokens::CacheBreakDetector;
///
/// let detector = CacheBreakDetector::new();
/// // After each API call:
/// // detector.record(&usage);
/// // if detector.is_break_detected() { ... }
/// ```
pub struct CacheBreakDetector {
    /// Rolling window of recent cache hit rates.
    samples: VecDeque<f64>,
    /// Maximum samples to keep.
    window_size: usize,
    /// Minimum drop from rolling average to trigger detection.
    drop_threshold: f64,
    /// Whether a break was detected on the last `record()` call.
    break_detected: bool,
    /// Total number of breaks detected.
    total_breaks: u32,
    /// Optional event bus for emitting CacheBreak events.
    event_bus: Option<Arc<EventBus>>,
}

impl CacheBreakDetector {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(DEFAULT_WINDOW_SIZE + 1),
            window_size: DEFAULT_WINDOW_SIZE,
            drop_threshold: DEFAULT_DROP_THRESHOLD,
            break_detected: false,
            total_breaks: 0,
            event_bus: None,
        }
    }

    pub fn window_size(mut self, size: usize) -> Self {
        self.window_size = size.max(2);
        self
    }

    pub fn drop_threshold(mut self, threshold: f64) -> Self {
        self.drop_threshold = threshold.clamp(0.05, 0.9);
        self
    }

    pub fn event_bus(mut self, bus: Arc<EventBus>) -> Self {
        self.event_bus = Some(bus);
        self
    }

    /// Record a new usage sample and check for cache break.
    ///
    /// Returns `true` if a cache break was detected.
    pub fn record(&mut self, usage: &TokenUsage) -> bool {
        let hit_rate = cache_hit_rate(usage);
        self.break_detected = false;

        if self.samples.len() >= 2 {
            let avg = self.rolling_average();
            let drop = avg - hit_rate;

            if drop > self.drop_threshold && avg > 0.1 {
                self.break_detected = true;
                self.total_breaks += 1;

                warn!(
                    hit_rate = format!("{:.1}%", hit_rate * 100.0),
                    average = format!("{:.1}%", avg * 100.0),
                    drop = format!("{:.1}%", drop * 100.0),
                    "Prompt cache break detected"
                );

                if let Some(ref bus) = self.event_bus {
                    bus.emit_simple(
                        EventKind::Custom("cache_break"),
                        serde_json::json!({
                            "hit_rate": hit_rate,
                            "average": avg,
                            "drop": drop,
                            "total_breaks": self.total_breaks,
                        }),
                    );
                }
            }
        }

        self.samples.push_back(hit_rate);
        if self.samples.len() > self.window_size {
            self.samples.pop_front();
        }

        self.break_detected
    }

    /// Whether the last `record()` call detected a cache break.
    pub fn is_break_detected(&self) -> bool {
        self.break_detected
    }

    /// Total number of cache breaks detected.
    pub fn total_breaks(&self) -> u32 {
        self.total_breaks
    }

    /// Current rolling average cache hit rate.
    pub fn rolling_average(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.samples.iter().sum::<f64>() / self.samples.len() as f64
    }

    /// Current cache hit rate (last sample).
    pub fn current_hit_rate(&self) -> f64 {
        self.samples.back().copied().unwrap_or(0.0)
    }

    /// Reset the detector state.
    pub fn reset(&mut self) {
        self.samples.clear();
        self.break_detected = false;
    }
}

impl Default for CacheBreakDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Calculate cache hit rate from token usage.
///
/// Returns a value between 0.0 (no cache hits) and 1.0 (all from cache).
fn cache_hit_rate(usage: &TokenUsage) -> f64 {
    let total =
        usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens;
    if total == 0 {
        return 0.0;
    }
    usage.cache_read_input_tokens as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, cache_read: u64, cache_create: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: 100,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_create,
            ..Default::default()
        }
    }

    #[test]
    fn cache_hit_rate_calculation() {
        assert!((cache_hit_rate(&usage(100, 0, 0))).abs() < f64::EPSILON);
        assert!((cache_hit_rate(&usage(0, 100, 0)) - 1.0).abs() < f64::EPSILON);
        assert!((cache_hit_rate(&usage(50, 50, 0)) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn no_break_on_stable_cache() {
        let mut detector = CacheBreakDetector::new().window_size(3);

        // Build up history with high cache rate
        assert!(!detector.record(&usage(10, 90, 0)));
        assert!(!detector.record(&usage(10, 90, 0)));
        assert!(!detector.record(&usage(10, 90, 0)));

        // Still high
        assert!(!detector.record(&usage(10, 85, 5)));
        assert!(!detector.is_break_detected());
    }

    #[test]
    fn detects_break_on_sudden_drop() {
        let mut detector = CacheBreakDetector::new().window_size(3).drop_threshold(0.3);

        // Build high cache history
        detector.record(&usage(10, 90, 0));
        detector.record(&usage(10, 90, 0));
        detector.record(&usage(10, 90, 0));

        // Sudden drop to 0% cache
        let broke = detector.record(&usage(100, 0, 0));
        assert!(broke);
        assert!(detector.is_break_detected());
        assert_eq!(detector.total_breaks(), 1);
    }

    #[test]
    fn reset_clears_state() {
        let mut detector = CacheBreakDetector::new();
        detector.record(&usage(10, 90, 0));
        detector.record(&usage(10, 90, 0));

        detector.reset();
        assert!(!detector.is_break_detected());
        assert_eq!(detector.rolling_average(), 0.0);
    }

    #[test]
    fn zero_usage_no_panic() {
        let mut detector = CacheBreakDetector::new();
        assert!(!detector.record(&usage(0, 0, 0)));
    }
}
