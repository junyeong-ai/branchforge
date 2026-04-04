//! Resilience layer for Claude API client.
//!
//! Provides circuit breaker pattern for provider fault isolation.

pub use crate::common::circuit::{CircuitBreaker, CircuitConfig, CircuitState};

use std::sync::Arc;
use std::time::Duration;

/// Configuration for client resilience behavior.
///
/// Controls circuit breaker settings and request timeout.
#[derive(Clone)]
pub struct ResilienceConfig {
    pub circuit: Option<CircuitConfig>,
    pub timeout: Duration,
}

impl Default for ResilienceConfig {
    fn default() -> Self {
        Self {
            circuit: Some(CircuitConfig::default()),
            timeout: Duration::from_secs(120),
        }
    }
}

impl ResilienceConfig {
    /// No circuit breaker, just timeout.
    pub fn timeout_only(timeout: Duration) -> Self {
        Self {
            circuit: None,
            timeout,
        }
    }

    /// Higher failure threshold and longer recovery window.
    pub fn lenient() -> Self {
        Self {
            circuit: Some(CircuitConfig {
                failure_threshold: 10,
                recovery_timeout: Duration::from_secs(60),
                success_threshold: 5,
            }),
            timeout: Duration::from_secs(300),
        }
    }
}

pub struct Resilience {
    config: ResilienceConfig,
    circuit: Option<Arc<CircuitBreaker>>,
}

impl Resilience {
    pub fn new(config: ResilienceConfig) -> Self {
        let circuit = config
            .circuit
            .as_ref()
            .map(|c| Arc::new(CircuitBreaker::new(c.clone())));
        Self { config, circuit }
    }

    pub fn config(&self) -> &ResilienceConfig {
        &self.config
    }

    pub fn circuit(&self) -> Option<&Arc<CircuitBreaker>> {
        self.circuit.as_ref()
    }
}

impl Default for Resilience {
    fn default() -> Self {
        Self::new(ResilienceConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ResilienceConfig::default();
        assert!(config.circuit.is_some());
        assert_eq!(config.timeout, Duration::from_secs(120));
    }

    #[test]
    fn test_timeout_only() {
        let config = ResilienceConfig::timeout_only(Duration::from_secs(60));
        assert!(config.circuit.is_none());
    }

    #[test]
    fn test_lenient_config() {
        let config = ResilienceConfig::lenient();
        assert!(config.circuit.is_some());
        assert_eq!(config.circuit.unwrap().failure_threshold, 10);
    }
}
