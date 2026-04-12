//! Provider client stack: codecs, transports, profiles, and decorators.
//!
//! The public API surface is intentionally minimal:
//!
//! - [`ProfileRegistry`] / [`preset::from_env`] — open-set
//!   registry of named `(codec, transport, credential)` recipes.
//!   Ships canonical builtins **and** accepts user-registered
//!   profiles at runtime.
//! - [`ProviderClient`] — composition of one [`codec::ModelCodec`] and one
//!   [`transport::ModelTransport`].
//! - [`LlmCall`] + decorators ([`RetryingClient`], [`FallingBackClient`],
//!   [`CircuitBrokenClient`]) — composable behaviour wrappers around any
//!   `LlmCall` implementation.
//!
//! There is no monolithic `Client` type. The agent runtime holds an
//! `Arc<dyn LlmCall>` and the public `query`/`stream` helpers in `lib.rs`
//! resolve a profile from environment variables on demand.

#![allow(missing_docs)]

pub mod cache;
pub mod codec;
pub mod fallback;
pub mod llm_call;
pub mod mock;
pub mod preset;
pub mod provider_client;
pub mod resilience;
pub mod schema;
pub mod transport;

use std::time::Duration;

pub use fallback::{FallbackConfig, FallbackTrigger};
pub use llm_call::{CircuitBrokenClient, FallingBackClient, LlmCall, RetryingClient};
pub use mock::{MockLlmCall, MockResponse};
pub use provider_client::validate_composition;
// Phase H-1: `EnvLookup` / `SystemEnv` now live in `crate::common::env`;
// re-exported here so existing consumers of `branchforge::client::EnvLookup`
// keep working without an import path change.
pub use crate::common::env::{EnvLookup, SystemEnv};
pub use preset::{
    CredentialHint, ProfileBuildContext, ProfileRegistry, ProviderProfile, TransportBuilder,
};
pub use provider_client::ProviderClient;
pub use resilience::{CircuitBreaker, CircuitConfig, CircuitState, Resilience, ResilienceConfig};
pub use schema::{
    MinItemsPolicy, ObjectClosure, PreparedSchema, RequiredHandling, SchemaPolicy, prepare_schema,
    prepare_tool_schema, schema_for, warn_dropped_metadata,
};

/// Default HTTP timeout shared by all transports unless overridden.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Configurable exponential-backoff strategy.
///
/// Used by [`RetryPolicy`] to compute per-attempt delays.
#[derive(Debug, Clone)]
pub struct BackoffStrategy {
    /// Delay for the first retry attempt.
    pub initial_delay: Duration,
    /// Upper bound on the computed delay (before jitter).
    pub max_delay: Duration,
    /// Factor applied to the delay on each successive attempt.
    pub multiplier: f64,
    /// When `true`, +-15 % jitter is added to the delay.
    pub jitter: bool,
}

impl Default for BackoffStrategy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            multiplier: 2.0,
            jitter: true,
        }
    }
}

impl BackoffStrategy {
    /// Compute the delay for the given attempt number (1-indexed).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let exp = self.initial_delay.as_millis() as f64
            * self.multiplier.powi(attempt.saturating_sub(1) as i32);
        let clamped = exp.min(self.max_delay.as_millis() as f64);
        let with_jitter = if self.jitter {
            let j = clamped * 0.15 * (2.0 * rand::random::<f64>() - 1.0);
            (clamped + j).max(0.0)
        } else {
            clamped
        };
        Duration::from_millis(with_jitter as u64)
    }
}

/// Policy for retrying transient errors with exponential backoff.
///
/// Wrapped around a [`LlmCall`] via [`RetryingClient`]. Retries the *same*
/// model first; switching to a different model is the
/// [`FallingBackClient`]'s job.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub backoff: BackoffStrategy,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            backoff: BackoffStrategy {
                initial_delay: Duration::from_secs(1),
                max_delay: Duration::from_secs(30),
                ..BackoffStrategy::default()
            },
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

    /// Convenience: legacy fields.
    pub fn base_delay(&self) -> Duration {
        self.backoff.initial_delay
    }

    pub fn max_delay(&self) -> Duration {
        self.backoff.max_delay
    }

    pub fn delay_for(&self, attempt: u32, server_retry_after: Option<Duration>) -> Duration {
        if let Some(server_delay) = server_retry_after {
            return server_delay;
        }
        self.backoff.delay_for_attempt(attempt)
    }
}
