//! Non-blocking event bus for observability.
//!
//! This module provides a fire-and-forget event bus for metrics, logging, and
//! other observability concerns. It complements [`HookRegistry`](crate::hooks::HookRegistry)
//! which handles fail-closed, security-critical hooks.

mod bus;
mod typed;

pub use bus::{Event, EventBus, EventKind, SubscriberFn, SubscriptionHandle, SubscriptionId};
pub use typed::{
    BranchForkedPayload, BudgetAlertPayload, CacheBreakObservedPayload, CheckpointCreatedPayload,
    EventPayload, RateLimitApproachingPayload, RateLimitObservedPayload, SessionChangedPayload,
    SessionCompactedPayload, StreamChunkKind, StreamChunkPayload, TokensConsumedPayload,
    ToolExecutedPayload, ToolProgressPayload,
};
