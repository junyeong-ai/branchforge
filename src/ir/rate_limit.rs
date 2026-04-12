//! Provider-neutral rate-limit snapshot.
//!
//! Every commercial LLM provider publishes soft rate-limit accounting on
//! successful-response HTTP headers (Anthropic `anthropic-ratelimit-*`,
//! OpenAI `x-ratelimit-*`, Gemini `x-goog-quota-*`). The agent runtime
//! uses this to:
//!
//! 1. Emit a `RateLimitApproachingPayload` event when remaining budget
//!    drops below a threshold, so observability dashboards can warn
//!    before a 429 actually fires.
//! 2. Let recovery recipes make data-driven backoff decisions instead
//!    of falling back to arbitrary exponential retry windows.
//!
//! The snapshot is provider-neutral: every field is `Option` because
//! different providers report different subsets, and `reset` is an
//! absolute `DateTime<Utc>` so comparison across requests is safe
//! regardless of clock skew between the agent process and the provider.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Point-in-time view of the caller's remaining request / token budget
/// for a single model family, as reported by the provider on the most
/// recent response. `None` fields mean the provider did not publish
/// that axis; callers must treat absence as "unknown", not zero.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitSnapshot {
    /// Provider-reported maximum requests allowed in the current window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_limit: Option<u64>,

    /// Requests remaining in the current window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_remaining: Option<u64>,

    /// Absolute time at which the request window resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_reset: Option<DateTime<Utc>>,

    /// Provider-reported maximum tokens allowed in the current window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_limit: Option<u64>,

    /// Tokens remaining in the current window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_remaining: Option<u64>,

    /// Absolute time at which the token window resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_reset: Option<DateTime<Utc>>,
}

/// Default "approaching" threshold — a snapshot whose remaining
/// budget on any axis has dropped to ≤10% of its window. Used by
/// the agent runtime as the default firing point for
/// [`crate::events::RateLimitApproachingPayload`]. Exposed as a
/// public constant so downstream callers that want a different
/// threshold have a stable reference to compare against.
pub const APPROACHING_THRESHOLD: f64 = 0.10;

impl RateLimitSnapshot {
    /// Ratio `remaining / limit` for the requests axis. Returns `None`
    /// when either field is missing or `limit == 0`.
    pub fn requests_ratio(&self) -> Option<f64> {
        let limit = self.requests_limit?;
        if limit == 0 {
            return None;
        }
        let remaining = self.requests_remaining?;
        Some(remaining as f64 / limit as f64)
    }

    /// Ratio `remaining / limit` for the tokens axis.
    pub fn tokens_ratio(&self) -> Option<f64> {
        let limit = self.tokens_limit?;
        if limit == 0 {
            return None;
        }
        let remaining = self.tokens_remaining?;
        Some(remaining as f64 / limit as f64)
    }

    /// `true` when any axis reports at most `threshold` fraction of its
    /// budget remaining. Default threshold is `0.10` (10%) — callers
    /// wire this into `RateLimitApproachingPayload` emission.
    pub fn is_approaching_limit(&self, threshold: f64) -> bool {
        let approaching = |r: Option<f64>| matches!(r, Some(v) if v <= threshold);
        approaching(self.requests_ratio()) || approaching(self.tokens_ratio())
    }

    /// Shortest non-negative duration (seconds) until any window resets.
    /// Used by recovery recipes to bound their retry wait.
    pub fn seconds_until_reset(&self, now: DateTime<Utc>) -> Option<u64> {
        let candidates = [self.requests_reset, self.tokens_reset];
        candidates
            .into_iter()
            .flatten()
            .map(|ts| (ts - now).num_seconds().max(0) as u64)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn ratios_compute_when_both_fields_present() {
        let snap = RateLimitSnapshot {
            requests_limit: Some(1000),
            requests_remaining: Some(150),
            tokens_limit: Some(500_000),
            tokens_remaining: Some(50_000),
            ..Default::default()
        };
        assert!((snap.requests_ratio().unwrap() - 0.15).abs() < 1e-9);
        assert!((snap.tokens_ratio().unwrap() - 0.10).abs() < 1e-9);
    }

    #[test]
    fn ratio_missing_limit_returns_none() {
        let snap = RateLimitSnapshot {
            requests_remaining: Some(50),
            ..Default::default()
        };
        assert!(snap.requests_ratio().is_none());
    }

    #[test]
    fn approaching_limit_fires_on_any_axis() {
        let snap = RateLimitSnapshot {
            requests_limit: Some(1000),
            requests_remaining: Some(900), // 90% remaining
            tokens_limit: Some(100_000),
            tokens_remaining: Some(5_000), // 5% remaining
            ..Default::default()
        };
        assert!(snap.is_approaching_limit(0.10));
        assert!(!snap.is_approaching_limit(0.01));
    }

    #[test]
    fn seconds_until_reset_picks_soonest() {
        let now = Utc::now();
        let snap = RateLimitSnapshot {
            requests_reset: Some(now + Duration::seconds(60)),
            tokens_reset: Some(now + Duration::seconds(30)),
            ..Default::default()
        };
        assert_eq!(snap.seconds_until_reset(now), Some(30));
    }

    #[test]
    fn seconds_until_reset_clamps_negative_to_zero() {
        let now = Utc::now();
        let snap = RateLimitSnapshot {
            requests_reset: Some(now - Duration::seconds(10)),
            ..Default::default()
        };
        assert_eq!(snap.seconds_until_reset(now), Some(0));
    }
}
