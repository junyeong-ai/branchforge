//! Result of a session compaction or projection truncation.

use crate::decision::DecisionReason;
use crate::ir::TokenCount;

/// Typed reason a compaction run was skipped. Replaces the free-form
/// string that used to be stamped into [`CompactResult::Skipped`] so
/// the observability layer can tag span attributes with a
/// cardinality-bounded category.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactSkipReason {
    /// Circuit breaker is open after repeated compaction failures;
    /// the chain will retry once the breaker half-opens.
    CircuitBreakerOpen,
    /// Disabled in configuration — the caller opted out of compaction
    /// entirely for this session.
    Disabled,
    /// Another compaction run is in flight on the same session.
    AlreadyRunning,
    /// Configured strategies ran but none was applicable to the
    /// current state (below threshold, no mutable blocks, etc.).
    NoStrategyApplicable,
    /// Caller passed `llm: None` but every registered strategy
    /// required an LLM.
    NoLlmAvailable,
    /// Escape hatch for new skip paths pending proper categorisation.
    /// Prefer adding a dedicated variant over reaching for this.
    Other(&'static str),
}

impl std::fmt::Display for CompactSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CircuitBreakerOpen => write!(f, "circuit breaker open"),
            Self::Disabled => write!(f, "compaction disabled"),
            Self::AlreadyRunning => write!(f, "another compaction run in flight"),
            Self::NoStrategyApplicable => write!(f, "no compaction strategy applicable"),
            Self::NoLlmAvailable => write!(f, "no LLM available for strategies"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl DecisionReason for CompactSkipReason {
    fn category(&self) -> &'static str {
        match self {
            Self::CircuitBreakerOpen => "circuit_breaker_open",
            Self::Disabled => "disabled",
            Self::AlreadyRunning => "already_running",
            Self::NoStrategyApplicable => "no_strategy_applicable",
            Self::NoLlmAvailable => "no_llm_available",
            Self::Other(_) => "other",
        }
    }

    fn summary(&self) -> String {
        self.to_string()
    }
}

/// Outcome of running compaction against a session.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum CompactResult {
    /// No compaction needed (under threshold).
    NotNeeded,
    /// Full compaction completed.
    Compacted {
        original_count: usize,
        new_count: usize,
        saved_tokens: TokenCount,
        summary: String,
    },
    /// Compaction was skipped. The reason is a typed
    /// [`CompactSkipReason`] so downstream observability can bucket
    /// skips without parsing strings.
    Skipped { reason: CompactSkipReason },
    /// Content blocks were truncated in the projection (micro-compaction).
    /// The graph remains unchanged; truncations are session-local.
    Truncated {
        truncation_count: usize,
        estimated_token_savings: TokenCount,
    },
}
