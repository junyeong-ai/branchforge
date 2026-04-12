//! Circuit breaker implementation.
//!
//! Generic circuit breaker pattern for protecting against cascading failures.
//! Used by the HTTP client resilience layer and compaction pipeline.

use std::sync::RwLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Clone, Debug)]
pub struct CircuitConfig {
    pub failure_threshold: u32,
    pub recovery_timeout: Duration,
    pub success_threshold: u32,
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            recovery_timeout: Duration::from_secs(30),
            success_threshold: 3,
        }
    }
}

pub struct CircuitBreaker {
    config: CircuitConfig,
    state: RwLock<CircuitState>,
    failure_count: AtomicU32,
    success_count: AtomicU32,
    last_failure_time: AtomicU64,
    half_open_requests: AtomicU32,
}

impl CircuitBreaker {
    pub fn new(config: CircuitConfig) -> Self {
        Self {
            config,
            state: RwLock::new(CircuitState::Closed),
            failure_count: AtomicU32::new(0),
            success_count: AtomicU32::new(0),
            last_failure_time: AtomicU64::new(0),
            half_open_requests: AtomicU32::new(0),
        }
    }

    pub fn state(&self) -> CircuitState {
        *self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn allow_request(&self) -> bool {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());

        match *state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let last_failure_ms = self.last_failure_time.load(Ordering::Relaxed);
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let elapsed = Duration::from_millis(now_ms.saturating_sub(last_failure_ms));

                if elapsed >= self.config.recovery_timeout {
                    drop(state);
                    self.transition_to_half_open();
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => loop {
                let current = self.half_open_requests.load(Ordering::Acquire);
                if current >= self.config.success_threshold {
                    return false;
                }
                if self
                    .half_open_requests
                    .compare_exchange_weak(
                        current,
                        current + 1,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return true;
                }
            },
        }
    }

    pub fn record_success(&self) {
        let state = *self.state.read().unwrap_or_else(|e| e.into_inner());

        match state {
            CircuitState::Closed => {
                self.failure_count.store(0, Ordering::Relaxed);
            }
            CircuitState::HalfOpen => {
                let successes = self.success_count.fetch_add(1, Ordering::Relaxed) + 1;
                if successes >= self.config.success_threshold {
                    self.transition_to_closed();
                }
            }
            CircuitState::Open => {}
        }
    }

    pub fn record_failure(&self) {
        let state = *self.state.read().unwrap_or_else(|e| e.into_inner());

        match state {
            CircuitState::Closed => {
                let failures = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
                if failures >= self.config.failure_threshold {
                    self.transition_to_open();
                }
            }
            CircuitState::HalfOpen => {
                self.transition_to_open();
            }
            CircuitState::Open => {}
        }
    }

    fn transition_to_open(&self) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        *state = CircuitState::Open;
        self.last_failure_time.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            Ordering::Relaxed,
        );
        self.success_count.store(0, Ordering::Relaxed);
        self.half_open_requests.store(0, Ordering::Relaxed);
        tracing::warn!("Circuit breaker opened");
    }

    fn transition_to_half_open(&self) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if *state == CircuitState::Open {
            *state = CircuitState::HalfOpen;
            self.half_open_requests.store(0, Ordering::Relaxed);
            self.success_count.store(0, Ordering::Relaxed);
            tracing::info!("Circuit breaker half-open");
        }
    }

    fn transition_to_closed(&self) {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        *state = CircuitState::Closed;
        self.failure_count.store(0, Ordering::Relaxed);
        self.success_count.store(0, Ordering::Relaxed);
        self.half_open_requests.store(0, Ordering::Relaxed);
        tracing::info!("Circuit breaker closed");
    }

    pub fn reset(&self) {
        self.transition_to_closed();
    }

    pub fn failure_count(&self) -> u32 {
        self.failure_count.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_starts_closed() {
        let cb = CircuitBreaker::new(CircuitConfig::default());
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow_request());
    }

    #[test]
    fn test_circuit_opens_after_failures() {
        let config = CircuitConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_success_resets_failure_count() {
        let config = CircuitConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.failure_count(), 2);

        cb.record_success();
        assert_eq!(cb.failure_count(), 0);
    }

    #[test]
    fn test_reset() {
        let config = CircuitConfig {
            failure_threshold: 2,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        cb.reset();
        assert_eq!(cb.state(), CircuitState::Closed);
    }
}
