//! Provider client stack: codecs, transports, presets, and decorators.
//!
//! The public API surface is intentionally minimal:
//!
//! - [`Preset`] / [`preset::from_env`] — opinionated `(codec, transport)`
//!   compositions, the canonical entry point for applications.
//! - [`ProviderClient`] — composition of one [`codec::ModelCodec`] and one
//!   [`transport::ModelTransport`].
//! - [`LlmCall`] + decorators ([`RetryingClient`], [`FallingBackClient`],
//!   [`CircuitBrokenClient`]) — composable behaviour wrappers around any
//!   `LlmCall` implementation.
//!
//! There is no monolithic `Client` type. The agent runtime holds an
//! `Arc<dyn LlmCall>` and the public `query`/`stream` helpers in `lib.rs`
//! resolve a `Preset` from environment variables on demand.

pub mod codec;
pub mod fallback;
pub mod llm_call;
pub mod preset;
pub mod provider_client;
pub mod resilience;
pub mod schema;
pub mod transport;

use std::time::Duration;

pub use fallback::{FallbackConfig, FallbackTrigger};
pub use llm_call::{CircuitBrokenClient, FallingBackClient, LlmCall, RetryingClient};
pub use preset::Preset;
pub use provider_client::ProviderClient;
pub use resilience::{CircuitBreaker, CircuitConfig, CircuitState, Resilience, ResilienceConfig};
pub use schema::{strict_schema, transform_for_strict};

/// Default HTTP timeout shared by all transports unless overridden.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Policy for retrying transient errors with exponential backoff.
///
/// Wrapped around a [`LlmCall`] via [`RetryingClient`]. Retries the *same*
/// model first; switching to a different model is the
/// [`FallingBackClient`]'s job.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Default::default()
        }
    }

    pub fn delay_for(&self, attempt: u32, server_retry_after: Option<Duration>) -> Duration {
        if let Some(server_delay) = server_retry_after {
            return server_delay;
        }
        let exp =
            self.base_delay.as_millis() as f64 * 2.0f64.powi(attempt.saturating_sub(1) as i32);
        let clamped = exp.min(self.max_delay.as_millis() as f64);
        let jitter = clamped * 0.15 * (2.0 * rand::random::<f64>() - 1.0);
        Duration::from_millis((clamped + jitter).max(0.0) as u64)
    }
}
