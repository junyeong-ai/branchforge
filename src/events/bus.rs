//! Non-blocking event bus for observability.
//!
//! Unlike [`HookManager`](crate::hooks::HookManager) which is fail-closed and blocking
//! (security-critical hooks that can reject operations), [`EventBus`] is fire-and-forget:
//!
//! - Events are dispatched asynchronously via `tokio::spawn`
//! - Subscriber failures are silently ignored
//! - No event can block or cancel execution
//!
//! This makes `EventBus` suitable for metrics, logging, and other observability concerns
//! that should never interfere with the agent's operation.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::broadcast;

/// Event kinds for the observability bus.
///
/// Subscribers can filter on these to receive only relevant events.
/// Use [`Custom`](EventKind::Custom) for application-specific event types.
#[derive(Clone, Copy, Debug, Eq)]
pub enum EventKind {
    /// A request was sent to the provider.
    RequestSent,
    /// A response was received from the provider.
    ResponseReceived,
    /// A tool was executed.
    ToolExecuted,
    /// Tokens were consumed (per-turn usage).
    TokensConsumed,
    /// A stream chunk was received.
    StreamChunk,
    /// An error occurred.
    Error,
    /// Session state changed (generic).
    SessionChanged,
    /// Budget threshold reached.
    BudgetAlert,
    /// Session was compacted — carries the summary text for indexing.
    SessionCompacted,
    /// A branch was forked — carries ancestor context for indexing.
    BranchForked,
    /// A checkpoint was created.
    CheckpointCreated,
    /// Custom event for extensibility.
    Custom(&'static str),
}

impl PartialEq for EventKind {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::RequestSent, Self::RequestSent)
            | (Self::ResponseReceived, Self::ResponseReceived)
            | (Self::ToolExecuted, Self::ToolExecuted)
            | (Self::TokensConsumed, Self::TokensConsumed)
            | (Self::StreamChunk, Self::StreamChunk)
            | (Self::Error, Self::Error)
            | (Self::SessionChanged, Self::SessionChanged)
            | (Self::BudgetAlert, Self::BudgetAlert)
            | (Self::SessionCompacted, Self::SessionCompacted)
            | (Self::BranchForked, Self::BranchForked)
            | (Self::CheckpointCreated, Self::CheckpointCreated) => true,
            (Self::Custom(a), Self::Custom(b)) => a == b,
            _ => false,
        }
    }
}

impl Hash for EventKind {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Use a discriminant byte so different variants never collide.
        match self {
            Self::RequestSent => state.write_u8(0),
            Self::ResponseReceived => state.write_u8(1),
            Self::ToolExecuted => state.write_u8(2),
            Self::TokensConsumed => state.write_u8(3),
            Self::StreamChunk => state.write_u8(4),
            Self::Error => state.write_u8(5),
            Self::SessionChanged => state.write_u8(6),
            Self::BudgetAlert => state.write_u8(7),
            Self::SessionCompacted => state.write_u8(8),
            Self::BranchForked => state.write_u8(9),
            Self::CheckpointCreated => state.write_u8(10),
            Self::Custom(s) => {
                state.write_u8(11);
                s.hash(state);
            }
        }
    }
}

/// Event payload delivered to subscribers.
#[derive(Clone, Debug)]
pub struct Event {
    /// The kind of event.
    pub kind: EventKind,
    /// When the event occurred.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Arbitrary JSON payload.
    pub data: serde_json::Value,
    /// Optional session identifier for correlation.
    pub session_id: Option<String>,
}

impl Event {
    /// Create a new event with the given kind and data.
    pub fn new(kind: EventKind, data: serde_json::Value) -> Self {
        Self {
            kind,
            timestamp: chrono::Utc::now(),
            data,
            session_id: None,
        }
    }

    /// Attach a session identifier for correlation.
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// Subscriber callback type.
///
/// Callbacks are invoked via `tokio::spawn`, so they must be `Send + Sync + 'static`.
/// Panics inside a callback are caught by the tokio task and do not propagate.
pub type SubscriberFn = Arc<dyn Fn(Event) + Send + Sync>;

/// Non-blocking event bus for observability.
///
/// Unlike [`HookManager`](crate::hooks::HookManager) (fail-closed, blocking), `EventBus`
/// is fire-and-forget:
/// - Events are dispatched asynchronously
/// - Subscriber failures are silently ignored
/// - No event can block or cancel execution
pub struct EventBus {
    subscribers: DashMap<EventKind, Vec<SubscriberFn>>,
    broadcast: broadcast::Sender<Event>,
}

impl EventBus {
    /// Create a new `EventBus` with the given broadcast channel capacity.
    ///
    /// The capacity determines how many un-consumed events can be buffered
    /// in the broadcast channel before older events are dropped for slow
    /// receivers (lagged).
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self {
            subscribers: DashMap::new(),
            broadcast: tx,
        }
    }

    /// Subscribe to a specific event kind with a callback.
    ///
    /// The callback will be spawned as a tokio task each time a matching
    /// event is emitted, so it must not block.
    pub fn subscribe(&self, kind: EventKind, callback: SubscriberFn) {
        self.subscribers.entry(kind).or_default().push(callback);
    }

    /// Subscribe to all events via a broadcast channel.
    ///
    /// Returns a receiver that will get a clone of every emitted event.
    /// If the receiver falls behind by more than `capacity` events it will
    /// experience lag (missed events).
    pub fn subscribe_all(&self) -> broadcast::Receiver<Event> {
        self.broadcast.subscribe()
    }

    /// Emit an event (non-blocking, fire-and-forget).
    ///
    /// 1. Sends to the broadcast channel (ignored if no receivers).
    /// 2. Looks up per-kind subscribers and spawns each callback in a
    ///    tokio task so the caller never blocks.
    pub fn emit(&self, event: Event) {
        // Broadcast to all-event subscribers. Ignore errors (no active
        // receivers is not an error condition for fire-and-forget).
        let _ = self.broadcast.send(event.clone());

        // Dispatch to per-kind subscribers.
        if let Some(subs) = self.subscribers.get(&event.kind) {
            for callback in subs.value().iter() {
                let cb = Arc::clone(callback);
                let ev = event.clone();
                tokio::spawn(async move {
                    // Subscriber failures are silently ignored.
                    // std::panic::catch_unwind is not needed here because
                    // tokio::spawn already catches panics within the task.
                    cb(ev);
                });
            }
        }
    }

    /// Convenience: emit with just a kind and data.
    pub fn emit_simple(&self, kind: EventKind, data: serde_json::Value) {
        self.emit(Event::new(kind, data));
    }

    /// Remove all subscribers for a specific event kind.
    pub fn clear_subscribers(&self, kind: EventKind) {
        self.subscribers.remove(&kind);
    }

    /// Get the count of subscribers registered for a specific event kind.
    ///
    /// Useful for debugging and testing.
    pub fn subscriber_count(&self, kind: EventKind) -> usize {
        self.subscribers
            .get(&kind)
            .map(|subs| subs.value().len())
            .unwrap_or(0)
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("subscriber_kinds", &self.subscribers.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn event_kind_hash_eq() {
        // Built-in variants
        assert_eq!(EventKind::RequestSent, EventKind::RequestSent);
        assert_ne!(EventKind::RequestSent, EventKind::ResponseReceived);

        // Custom variants
        assert_eq!(EventKind::Custom("foo"), EventKind::Custom("foo"));
        assert_ne!(EventKind::Custom("foo"), EventKind::Custom("bar"));
        assert_ne!(EventKind::Custom("foo"), EventKind::RequestSent);

        // Verify they work as DashMap keys
        let map: DashMap<EventKind, u32> = DashMap::new();
        map.insert(EventKind::RequestSent, 1);
        map.insert(EventKind::Custom("my_event"), 2);
        assert_eq!(*map.get(&EventKind::RequestSent).unwrap(), 1);
        assert_eq!(*map.get(&EventKind::Custom("my_event")).unwrap(), 2);
    }

    #[test]
    fn event_construction() {
        let event = Event::new(EventKind::ToolExecuted, serde_json::json!({"tool": "bash"}));
        assert_eq!(event.kind, EventKind::ToolExecuted);
        assert!(event.session_id.is_none());

        let event = event.with_session("sess-123");
        assert_eq!(event.session_id.as_deref(), Some("sess-123"));
    }

    #[tokio::test]
    async fn emit_to_broadcast_receiver() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe_all();

        bus.emit_simple(
            EventKind::RequestSent,
            serde_json::json!({"url": "/v1/messages"}),
        );

        let event = rx.recv().await.unwrap();
        assert_eq!(event.kind, EventKind::RequestSent);
    }

    #[tokio::test]
    async fn emit_to_per_kind_subscriber() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bus = EventBus::default();

        let c = Arc::clone(&counter);
        bus.subscribe(
            EventKind::ToolExecuted,
            Arc::new(move |_event| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );

        bus.emit_simple(EventKind::ToolExecuted, serde_json::json!({}));
        // Give the spawned task a moment to run.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unrelated_kind_not_dispatched() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bus = EventBus::default();

        let c = Arc::clone(&counter);
        bus.subscribe(
            EventKind::Error,
            Arc::new(move |_event| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );

        // Emit a different kind.
        bus.emit_simple(EventKind::RequestSent, serde_json::json!({}));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn subscriber_count_and_clear() {
        let bus = EventBus::default();

        assert_eq!(bus.subscriber_count(EventKind::Error), 0);

        bus.subscribe(EventKind::Error, Arc::new(|_| {}));
        bus.subscribe(EventKind::Error, Arc::new(|_| {}));
        assert_eq!(bus.subscriber_count(EventKind::Error), 2);

        bus.clear_subscribers(EventKind::Error);
        assert_eq!(bus.subscriber_count(EventKind::Error), 0);
    }

    #[test]
    fn debug_impl() {
        let bus = EventBus::default();
        let debug = format!("{:?}", bus);
        assert!(debug.contains("EventBus"));
        assert!(debug.contains("subscriber_kinds"));
    }

    #[tokio::test]
    async fn subscriber_panic_does_not_propagate() {
        let bus = EventBus::default();

        bus.subscribe(
            EventKind::Error,
            Arc::new(|_| {
                panic!("intentional test panic");
            }),
        );

        // This must not panic the caller.
        bus.emit_simple(EventKind::Error, serde_json::json!({}));

        // Give the spawned task a moment to run (and panic).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn no_receivers_does_not_error() {
        // No broadcast subscribers, no per-kind subscribers.
        let bus = EventBus::default();
        // Must not panic or error.
        bus.emit_simple(EventKind::StreamChunk, serde_json::json!({"chunk": 1}));
    }
}
