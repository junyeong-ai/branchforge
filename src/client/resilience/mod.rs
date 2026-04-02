//! Resilience layer for Claude API client.
//!
//! Provides retry with exponential backoff and circuit breaker pattern.

mod backoff;

pub use crate::common::circuit::{CircuitBreaker, CircuitConfig, CircuitState};
pub use backoff::ExponentialBackoff;

use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct ResilienceConfig {
    pub retry: RetryConfig,
    pub circuit: Option<CircuitConfig>,
    pub timeout: Duration,
}

#[derive(Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub backoff: ExponentialBackoff,
    pub retry_on_rate_limit: bool,
    pub retry_on_server_error: bool,
    pub retry_on_network_error: bool,
}

impl Default for ResilienceConfig {
    fn default() -> Self {
        Self {
            retry: RetryConfig::default(),
            circuit: Some(CircuitConfig::default()),
            timeout: Duration::from_secs(120),
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            backoff: ExponentialBackoff::default(),
            retry_on_rate_limit: true,
            retry_on_server_error: true,
            retry_on_network_error: true,
        }
    }
}

impl ResilienceConfig {
    pub fn no_retry() -> Self {
        Self {
            retry: RetryConfig {
                max_retries: 0,
                ..Default::default()
            },
            circuit: None,
            timeout: Duration::from_secs(120),
        }
    }

    pub fn aggressive() -> Self {
        Self {
            retry: RetryConfig {
                max_retries: 5,
                backoff: ExponentialBackoff::new(
                    Duration::from_millis(50),
                    Duration::from_secs(10),
                    2.0,
                ),
                ..Default::default()
            },
            circuit: Some(CircuitConfig {
                failure_threshold: 10,
                recovery_timeout: Duration::from_secs(60),
                success_threshold: 5,
            }),
            timeout: Duration::from_secs(300),
        }
    }

    pub fn conservative() -> Self {
        Self {
            retry: RetryConfig {
                max_retries: 2,
                backoff: ExponentialBackoff::new(
                    Duration::from_millis(500),
                    Duration::from_secs(30),
                    2.0,
                ),
                ..Default::default()
            },
            circuit: Some(CircuitConfig::default()),
            timeout: Duration::from_secs(60),
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

    pub async fn execute<F, T, E>(&self, mut operation: F) -> Result<T, E>
    where
        F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, E>> + Send>>,
        E: Into<crate::Error> + From<crate::Error> + Clone,
    {
        if let Some(ref cb) = self.circuit
            && !cb.allow_request()
        {
            return Err(E::from(crate::Error::CircuitOpen));
        }

        let mut attempts = 0;
        loop {
            let result = tokio::time::timeout(self.config.timeout, operation()).await;

            match result {
                Ok(Ok(value)) => {
                    if let Some(ref cb) = self.circuit {
                        cb.record_success();
                    }
                    return Ok(value);
                }
                Ok(Err(e)) => {
                    let error: crate::Error = e.clone().into();

                    if let Some(ref cb) = self.circuit {
                        cb.record_failure();
                    }

                    attempts += 1;
                    if attempts > self.config.retry.max_retries {
                        return Err(e);
                    }

                    if !self.should_retry(&error) {
                        return Err(e);
                    }

                    let delay = self.config.retry.backoff.delay_for(attempts);

                    if let Some(retry_after) = error.retry_after() {
                        tokio::time::sleep(retry_after.max(delay)).await;
                    } else {
                        tokio::time::sleep(delay).await;
                    }
                }
                Err(_timeout) => {
                    if let Some(ref cb) = self.circuit {
                        cb.record_failure();
                    }

                    attempts += 1;
                    if attempts > self.config.retry.max_retries {
                        return Err(E::from(crate::Error::Timeout(self.config.timeout)));
                    }

                    let delay = self.config.retry.backoff.delay_for(attempts);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    fn should_retry(&self, error: &crate::Error) -> bool {
        match error {
            crate::Error::RateLimit { .. } => self.config.retry.retry_on_rate_limit,
            crate::Error::Network(_) => self.config.retry.retry_on_network_error,
            crate::Error::Api {
                status: Some(529), ..
            } => self.config.retry.retry_on_server_error,
            crate::Error::Api {
                status: Some(500..=599),
                ..
            } => self.config.retry.retry_on_server_error,
            _ => false,
        }
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
        assert_eq!(config.retry.max_retries, 3);
        assert!(config.circuit.is_some());
    }

    #[test]
    fn test_no_retry_config() {
        let config = ResilienceConfig::no_retry();
        assert_eq!(config.retry.max_retries, 0);
        assert!(config.circuit.is_none());
    }

    #[test]
    fn test_aggressive_config() {
        let config = ResilienceConfig::aggressive();
        assert_eq!(config.retry.max_retries, 5);
        assert!(config.circuit.is_some());
    }
}
