//! Result of a session compaction or projection truncation.

/// Outcome of running compaction against a session.
#[derive(Debug, Clone)]
pub enum CompactResult {
    /// No compaction needed (under threshold).
    NotNeeded,
    /// Full compaction completed.
    Compacted {
        original_count: usize,
        new_count: usize,
        saved_tokens: usize,
        summary: String,
    },
    /// Compaction was skipped (e.g., disabled or another run in flight).
    Skipped { reason: String },
    /// Content blocks were truncated in the projection (micro-compaction).
    /// The graph remains unchanged; truncations are session-local.
    Truncated {
        truncation_count: usize,
        estimated_token_savings: u64,
    },
}
