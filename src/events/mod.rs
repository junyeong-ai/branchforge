//! Non-blocking event bus for observability.
//!
//! This module provides a fire-and-forget event bus for metrics, logging, and
//! other observability concerns. It complements [`HookManager`](crate::hooks::HookManager)
//! which handles fail-closed, security-critical hooks.

mod bus;

pub use bus::{Event, EventBus, EventKind, SubscriberFn, SubscriptionHandle, SubscriptionId};
