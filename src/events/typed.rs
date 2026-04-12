//! Typed payloads for [`EventBus`].
//!
//! The base [`EventBus`] delivers [`Event`]s with an untyped
//! `data: serde_json::Value` payload — each consumer must decode the
//! fields they care about by hand, and every emitter hand-rolls a
//! `serde_json::json!(…)` literal. This is fine for external
//! integrations that want raw access but it is a source of drift
//! and copy-paste bugs inside the runtime.
//!
//! `EventPayload` is a thin Serde-backed trait that pins a typed
//! struct to a specific [`EventKind`]. Emitters build the struct and
//! call [`EventBus::emit_typed`]; subscribers register a typed
//! callback via [`EventBus::subscribe_typed`] that receives the
//! decoded struct directly. The bus's internal dispatch is unchanged
//! — typed events still flow through `Event.data` as JSON — so the
//! typed and untyped APIs coexist cleanly.
//!
//! # Example
//!
//! ```rust
//! use branchforge::events::{EventBus, TokensConsumedPayload};
//! use std::sync::Arc;
//! # fn example() {
//! let bus = EventBus::default();
//! let total = Arc::new(std::sync::Mutex::new(0u64));
//! let total_for_cb = Arc::clone(&total);
//! bus.subscribe_typed(move |data: TokensConsumedPayload| {
//!     *total_for_cb.lock().unwrap() += data.input_tokens + data.output_tokens;
//! });
//! bus.emit_typed(TokensConsumedPayload {
//!     input_tokens: 100,
//!     output_tokens: 50,
//!     model: "claude-sonnet-4-5".into(),
//! });
//! # }
//! ```

#![allow(missing_docs)]

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::bus::EventKind;
use crate::tools::ProgressStatus;

/// Typed payload for a specific [`EventKind`].
///
/// Implementors are Serde-backed structs or enums that serialize /
/// deserialize through the bus's JSON `Event.data` field. The
/// associated `KIND` pins the payload to exactly one event kind so
/// typed dispatch is a single map lookup.
pub trait EventPayload:
    Clone + std::fmt::Debug + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    /// The event kind this payload is dispatched under. Used by
    /// [`super::EventBus::subscribe_typed`] to pick the right
    /// subscriber slot and by [`super::EventBus::emit_typed`] to
    /// stamp the outgoing event.
    const KIND: EventKind;
}

/// `EventKind::TokensConsumed` payload: one API call's token usage.
///
/// Emitted after every successful model call by the agent runtime.
/// Subscribers use it to feed real-time usage dashboards and OTel
/// counters. Token totals are `u64` because long-running sessions
/// on 1M-context models can saturate `u32`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokensConsumedPayload {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
}

impl EventPayload for TokensConsumedPayload {
    const KIND: EventKind = EventKind::TokensConsumed;
}

/// `EventKind::ToolExecuted` payload: one completed tool call.
///
/// Emitted after every tool invocation completes (success or error).
/// `duration_ms` is wall-clock time from dispatch to result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolExecutedPayload {
    pub tool_name: String,
    pub duration_ms: u64,
    pub is_error: bool,
}

impl EventPayload for ToolExecutedPayload {
    const KIND: EventKind = EventKind::ToolExecuted;
}

/// `EventKind::ToolProgress` payload: a sub-step inside a running tool.
///
/// Long-running tools (Bash, multi-file Edit) emit these to surface
/// intermediate state to UIs before the final result arrives. The
/// `status` discriminates started / completed / failed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolProgressPayload {
    pub tool_id: String,
    pub tool_name: String,
    pub step: String,
    pub status: ProgressStatus,
}

impl EventPayload for ToolProgressPayload {
    const KIND: EventKind = EventKind::ToolProgress;
}

/// `EventKind::BudgetAlert` payload: budget utilization crossed the
/// configured alert threshold, or the budget is exhausted.
///
/// All monetary fields are `Decimal` — the `rust_decimal` crate's
/// `serde-with-str` feature serializes them as lossless string
/// literals so the JSON round-trip preserves precision. `utilization`
/// is a 0.0–1.0 ratio (not a percentage string), keeping the
/// numerics easy to graph without client-side parsing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BudgetAlertPayload {
    pub used_usd: Decimal,
    pub limit_usd: Decimal,
    pub remaining_usd: Decimal,
    /// Ratio of `used_usd / limit_usd` clamped to `[0.0, 1.0]`.
    /// `1.0` means budget exhausted or exceeded.
    pub utilization: f64,
}

impl EventPayload for BudgetAlertPayload {
    const KIND: EventKind = EventKind::BudgetAlert;
}

/// `EventKind::BranchForked` payload: a new branch was created from
/// an existing one. Emitted by [`crate::graph::SessionGraph::fork_branch`]
/// so indexers and UIs can react to topology changes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BranchForkedPayload {
    pub branch_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<String>,
}

impl EventPayload for BranchForkedPayload {
    const KIND: EventKind = EventKind::BranchForked;
}

/// `EventKind::CheckpointCreated` payload: a graph checkpoint was
/// recorded. Emitted by the graph's checkpoint machinery so UIs
/// can update their "save points" panel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointCreatedPayload {
    pub checkpoint_id: String,
    pub branch_id: String,
    pub label: String,
}

impl EventPayload for CheckpointCreatedPayload {
    const KIND: EventKind = EventKind::CheckpointCreated;
}

/// Discriminator for the three kinds of streaming chunks the agent
/// runtime re-broadcasts: text deltas, thinking deltas, and
/// materialised tool calls. Each variant carries the shape that
/// was previously stuffed into an untyped `{chunk_type: "…", …}`
/// JSON blob.
#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "chunk_type", rename_all = "snake_case")]
pub enum StreamChunkKind {
    /// Text delta of `length` characters.
    Text { length: usize },
    /// Reasoning / thinking delta of `length` characters.
    Thinking { length: usize },
    /// A materialised tool call surfaced through the stream.
    ToolUse { tool_name: String },
}

/// `EventKind::StreamChunk` payload: one wire-level streaming
/// event re-broadcast for observability. The inner enum carries
/// the discriminator, replacing the old stringly-typed
/// `chunk_type` field.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StreamChunkPayload {
    pub chunk: StreamChunkKind,
}

impl EventPayload for StreamChunkPayload {
    const KIND: EventKind = EventKind::StreamChunk;
}

/// `EventKind::SessionCompacted` payload: a session was compacted
/// to save tokens. Carries the saved-token delta and the summary
/// text for downstream indexing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionCompactedPayload {
    pub session_id: String,
    pub saved_tokens: u64,
    pub summary: String,
}

impl EventPayload for SessionCompactedPayload {
    const KIND: EventKind = EventKind::SessionCompacted;
}

/// Phase C-6: `EventKind::RateLimitObserved` payload — a point-in-time
/// snapshot of the provider's rate-limit accounting from the most
/// recent successful response. Fires on every observation, regardless
/// of remaining budget, so dashboards can plot usage curves.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateLimitObservedPayload {
    pub snapshot: crate::ir::RateLimitSnapshot,
}

impl EventPayload for RateLimitObservedPayload {
    const KIND: EventKind = EventKind::RateLimitObserved;
}

/// Phase C-6: `EventKind::RateLimitApproaching` payload — a subset of
/// [`RateLimitObservedPayload`] fired only when any axis is at or
/// below 10% of its window. Lets operators page on the approaching
/// threshold without subscribing to every observation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateLimitApproachingPayload {
    pub snapshot: crate::ir::RateLimitSnapshot,
}

impl EventPayload for RateLimitApproachingPayload {
    const KIND: EventKind = EventKind::RateLimitApproaching;
}

/// Phase D E-1: `EventKind::CacheBreakObserved` payload — fired
/// when the prompt-cache classifier detects that a cache break
/// occurred between two consecutive requests on the same session.
/// Carries the classified root cause (see
/// [`crate::observability::CacheBreakCause`]) so operators can
/// answer "why did the cache break?" without parsing logs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheBreakObservedPayload {
    /// Low-cardinality category string — matches
    /// `CacheBreakCause::category()`.
    pub category: String,
    /// Human-readable summary; free-form, NOT a metrics label.
    pub summary: String,
    /// Model id that reported the break.
    pub model: String,
}

impl EventPayload for CacheBreakObservedPayload {
    const KIND: EventKind = EventKind::CacheBreakObserved;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn tokens_consumed_serde_round_trip() {
        let original = TokensConsumedPayload {
            input_tokens: 12_345,
            output_tokens: 6_789,
            model: "claude-sonnet-4-5".into(),
        };
        let json = serde_json::to_value(&original).unwrap();
        let back: TokensConsumedPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.input_tokens, 12_345);
        assert_eq!(back.output_tokens, 6_789);
        assert_eq!(back.model, "claude-sonnet-4-5");
    }

    #[test]
    fn tool_executed_serde_round_trip() {
        let original = ToolExecutedPayload {
            tool_name: "Bash".into(),
            duration_ms: 1234,
            is_error: false,
        };
        let json = serde_json::to_value(&original).unwrap();
        let back: ToolExecutedPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.tool_name, "Bash");
        assert_eq!(back.duration_ms, 1234);
        assert!(!back.is_error);
    }

    #[test]
    fn tool_progress_serde_round_trip() {
        let original = ToolProgressPayload {
            tool_id: "tool-42".into(),
            tool_name: "Bash".into(),
            step: "spawn".into(),
            status: ProgressStatus::Started,
        };
        let json = serde_json::to_value(&original).unwrap();
        let back: ToolProgressPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.step, "spawn");
        assert_eq!(back.status, ProgressStatus::Started);
    }

    #[test]
    fn budget_alert_serde_round_trip() {
        let original = BudgetAlertPayload {
            used_usd: dec!(7.50),
            limit_usd: dec!(10.00),
            remaining_usd: dec!(2.50),
            utilization: 0.75,
        };
        let json = serde_json::to_value(&original).unwrap();
        let back: BudgetAlertPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.used_usd, dec!(7.50));
        assert_eq!(back.limit_usd, dec!(10.00));
        assert_eq!(back.remaining_usd, dec!(2.50));
        assert!((back.utilization - 0.75).abs() < 1e-9);
    }

    /// Each payload binds to exactly one `EventKind`.
    #[test]
    fn payload_kind_bindings_are_stable() {
        assert_eq!(TokensConsumedPayload::KIND, EventKind::TokensConsumed);
        assert_eq!(ToolExecutedPayload::KIND, EventKind::ToolExecuted);
        assert_eq!(ToolProgressPayload::KIND, EventKind::ToolProgress);
        assert_eq!(BudgetAlertPayload::KIND, EventKind::BudgetAlert);
        assert_eq!(BranchForkedPayload::KIND, EventKind::BranchForked);
        assert_eq!(CheckpointCreatedPayload::KIND, EventKind::CheckpointCreated);
        assert_eq!(StreamChunkPayload::KIND, EventKind::StreamChunk);
        assert_eq!(SessionCompactedPayload::KIND, EventKind::SessionCompacted);
    }

    #[test]
    fn branch_forked_serde_round_trip() {
        let original = BranchForkedPayload {
            branch_id: "b-1".into(),
            name: "experiment".into(),
            forked_from: Some("b-0".into()),
        };
        let json = serde_json::to_value(&original).unwrap();
        let back: BranchForkedPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.branch_id, "b-1");
        assert_eq!(back.name, "experiment");
        assert_eq!(back.forked_from.as_deref(), Some("b-0"));
    }

    #[test]
    fn stream_chunk_kind_round_trip() {
        for kind in [
            StreamChunkKind::Text { length: 42 },
            StreamChunkKind::Thinking { length: 100 },
            StreamChunkKind::ToolUse {
                tool_name: "Bash".into(),
            },
        ] {
            let payload = StreamChunkPayload {
                chunk: kind.clone(),
            };
            let json = serde_json::to_value(&payload).unwrap();
            let back: StreamChunkPayload = serde_json::from_value(json).unwrap();
            match (back.chunk, kind) {
                (StreamChunkKind::Text { length: a }, StreamChunkKind::Text { length: b }) => {
                    assert_eq!(a, b);
                }
                (
                    StreamChunkKind::Thinking { length: a },
                    StreamChunkKind::Thinking { length: b },
                ) => assert_eq!(a, b),
                (
                    StreamChunkKind::ToolUse { tool_name: a },
                    StreamChunkKind::ToolUse { tool_name: b },
                ) => assert_eq!(a, b),
                other => panic!("mismatched chunk kind: {other:?}"),
            }
        }
    }

    #[test]
    fn session_compacted_serde_round_trip() {
        let original = SessionCompactedPayload {
            session_id: "sess-1".into(),
            saved_tokens: 5_000,
            summary: "Compacted 42 messages.".into(),
        };
        let json = serde_json::to_value(&original).unwrap();
        let back: SessionCompactedPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back.session_id, "sess-1");
        assert_eq!(back.saved_tokens, 5_000);
        assert_eq!(back.summary, "Compacted 42 messages.");
    }
}
