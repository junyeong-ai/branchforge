//! Non-blocking event bus for observability.
//!
//! Unlike [`HookRegistry`](crate::hooks::HookRegistry) which is fail-closed and blocking
//! (security-critical hooks that can reject operations), [`EventBus`] is fire-and-forget:
//!
//! - Each subscriber owns a single drainer task that consumes events from a
//!   bounded mpsc channel — never one task per event, so high-frequency
//!   emissions cannot trigger task explosion.
//! - When a subscriber's channel is full the bus applies a configurable
//!   [`OverflowPolicy`] (drop silently or drop with a tracing warning) so
//!   operators can see when subscribers fall behind.
//! - [`EventBus::emit`] returns [`EmitStats`] reporting how many subscribers
//!   accepted vs. dropped each event, which makes lag visible to callers.
//! - No event can block or cancel execution.
//!
//! This makes `EventBus` suitable for metrics, logging, and other observability concerns
//! that should never interfere with the agent's operation.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// Unique identifier for a subscription registered with [`EventBus`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

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
    /// A tool emitted a sub-step progress event.
    ToolProgress,
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
            | (Self::ToolProgress, Self::ToolProgress)
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
            Self::ToolProgress => state.write_u8(3),
            Self::TokensConsumed => state.write_u8(4),
            Self::StreamChunk => state.write_u8(5),
            Self::Error => state.write_u8(6),
            Self::SessionChanged => state.write_u8(7),
            Self::BudgetAlert => state.write_u8(8),
            Self::SessionCompacted => state.write_u8(9),
            Self::BranchForked => state.write_u8(10),
            Self::CheckpointCreated => state.write_u8(11),
            Self::Custom(s) => {
                state.write_u8(12);
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
/// Callbacks run on a per-subscriber drainer task, so they must be
/// `Send + Sync + 'static`. Panics inside a callback are caught by the
/// drainer (tokio::spawn already catches panics) and do not propagate.
pub type SubscriberFn = Arc<dyn Fn(Event) + Send + Sync>;

/// Default mpsc buffer size for new subscribers.
///
/// 256 is large enough to absorb short bursts of high-frequency events
/// (e.g. `StreamChunk`) without blocking emitters, but small enough that a
/// stuck subscriber surfaces as a `dropped` count quickly instead of
/// silently consuming memory.
pub const DEFAULT_SUBSCRIBER_BUFFER: usize = 256;

/// What the bus does when a subscriber's channel is full at emit time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Silently drop the event for that subscriber (preserves the historic
    /// fire-and-forget contract). This is the default.
    #[default]
    Drop,
    /// Drop the event and emit a `tracing::warn!` so operators can detect
    /// lagging subscribers in production.
    WarnAndDrop,
}

/// Result of a single [`EventBus::emit`] call.
///
/// `delivered` is the number of subscribers whose mpsc channel accepted
/// the event; `dropped` is the number whose channel was full and the
/// event was discarded according to their [`OverflowPolicy`]. Together
/// they equal the number of per-kind subscribers registered for the
/// event's kind at the moment of dispatch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmitStats {
    pub delivered: usize,
    pub dropped: usize,
}

impl EmitStats {
    /// Total number of per-kind subscribers the event reached.
    pub fn total(&self) -> usize {
        self.delivered + self.dropped
    }
}

/// Internal state for a single registered subscriber.
struct SubscriberSlot {
    id: SubscriptionId,
    tx: mpsc::Sender<Event>,
    policy: OverflowPolicy,
    drainer: JoinHandle<()>,
}

impl Drop for SubscriberSlot {
    fn drop(&mut self) {
        // Aborting closes the channel, which causes the drainer's
        // `recv().await` to return `None` and exit cleanly.
        self.drainer.abort();
    }
}

/// Non-blocking event bus for observability.
///
/// See the module docs for the dispatch model and
/// rationale for the per-subscriber bounded mpsc design.
pub struct EventBus {
    subscribers: DashMap<EventKind, Vec<SubscriberSlot>>,
    broadcast: broadcast::Sender<Event>,
    next_id: AtomicU64,
    default_buffer_size: usize,
}

impl EventBus {
    /// Create a new `EventBus` with the given broadcast channel capacity.
    ///
    /// `broadcast_capacity` controls the all-events broadcast channel
    /// returned by [`Self::subscribe_all`]; lagged receivers there miss
    /// events. Per-kind subscribers use their own bounded mpsc channels
    /// of `DEFAULT_SUBSCRIBER_BUFFER` events each (override per
    /// subscription with [`Self::subscribe_with`]).
    pub fn new(broadcast_capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(broadcast_capacity);
        Self {
            subscribers: DashMap::new(),
            broadcast: tx,
            next_id: AtomicU64::new(0),
            default_buffer_size: DEFAULT_SUBSCRIBER_BUFFER,
        }
    }

    /// Override the default per-subscriber mpsc buffer size used by future
    /// [`Self::subscribe`] calls.
    pub fn with_default_buffer(mut self, buffer_size: usize) -> Self {
        self.default_buffer_size = buffer_size.max(1);
        self
    }

    /// Subscribe to a specific event kind with a callback.
    ///
    /// The bus spawns one drainer task per subscriber that pulls events
    /// from a bounded mpsc channel and invokes the callback. Uses the
    /// default buffer size and `OverflowPolicy::Drop`.
    pub fn subscribe(&self, kind: EventKind, callback: SubscriberFn) -> SubscriptionId {
        self.subscribe_with(
            kind,
            callback,
            self.default_buffer_size,
            OverflowPolicy::Drop,
        )
    }

    /// Subscribe with explicit buffer size and overflow policy.
    pub fn subscribe_with(
        &self,
        kind: EventKind,
        callback: SubscriberFn,
        buffer_size: usize,
        policy: OverflowPolicy,
    ) -> SubscriptionId {
        let id = SubscriptionId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, mut rx) = mpsc::channel::<Event>(buffer_size.max(1));
        let cb = Arc::clone(&callback);
        let drainer = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                cb(event);
            }
        });
        self.subscribers
            .entry(kind)
            .or_default()
            .push(SubscriberSlot {
                id,
                tx,
                policy,
                drainer,
            });
        id
    }

    /// Remove a previously registered subscription. The subscriber's
    /// drainer task is aborted via `Drop` on the slot.
    pub fn unsubscribe(&self, kind: EventKind, id: SubscriptionId) {
        if let Some(mut subs) = self.subscribers.get_mut(&kind) {
            subs.retain(|slot| slot.id != id);
        }
    }

    /// Subscribe and return a [`SubscriptionHandle`] that automatically
    /// unsubscribes when dropped.
    pub fn subscribe_with_handle(
        self: &Arc<Self>,
        kind: EventKind,
        callback: SubscriberFn,
    ) -> SubscriptionHandle {
        let id = self.subscribe(kind, callback);
        SubscriptionHandle {
            id,
            kind,
            bus: Arc::downgrade(self),
        }
    }

    /// Subscribe to all events via a broadcast channel.
    ///
    /// Returns a receiver that will get a clone of every emitted event.
    /// If the receiver falls behind by more than the broadcast capacity
    /// it will experience lag (missed events).
    pub fn subscribe_all(&self) -> broadcast::Receiver<Event> {
        self.broadcast.subscribe()
    }

    /// Emit an event (non-blocking).
    ///
    /// 1. Broadcasts to all-event subscribers (errors silently ignored).
    /// 2. Hands the event to each per-kind subscriber's mpsc channel via
    ///    `try_send`. On `Full`, applies the subscriber's
    ///    `OverflowPolicy` and counts the drop.
    ///
    /// Returns `EmitStats` so callers can detect lag in production.
    pub fn emit(&self, event: Event) -> EmitStats {
        // Broadcast to all-event subscribers. Ignore errors (no active
        // receivers is not an error condition for fire-and-forget).
        let _ = self.broadcast.send(event.clone());

        let mut stats = EmitStats::default();
        if let Some(subs) = self.subscribers.get(&event.kind) {
            for slot in subs.value().iter() {
                match slot.tx.try_send(event.clone()) {
                    Ok(()) => stats.delivered += 1,
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        stats.dropped += 1;
                        if matches!(slot.policy, OverflowPolicy::WarnAndDrop) {
                            tracing::warn!(
                                event_kind = ?event.kind,
                                subscription_id = ?slot.id,
                                "EventBus: subscriber channel full, dropping event"
                            );
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        // Drainer has exited (e.g. callback panicked).
                        // Count as dropped; the subscriber is effectively
                        // dead and will be cleaned up at unsubscribe.
                        stats.dropped += 1;
                    }
                }
            }
        }
        stats
    }

    /// Convenience: emit with just a kind and data. Discards `EmitStats`.
    pub fn emit_simple(&self, kind: EventKind, data: serde_json::Value) {
        let _ = self.emit(Event::new(kind, data));
    }

    /// Remove all subscribers for a specific event kind. Their drainer
    /// tasks are aborted via `Drop` on the slots.
    pub fn clear_subscribers(&self, kind: EventKind) {
        self.subscribers.remove(&kind);
    }

    /// Get the count of subscribers registered for a specific event kind.
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

/// RAII handle that unsubscribes when dropped.
pub struct SubscriptionHandle {
    id: SubscriptionId,
    kind: EventKind,
    bus: Weak<EventBus>,
}

impl Drop for SubscriptionHandle {
    fn drop(&mut self) {
        if let Some(bus) = self.bus.upgrade() {
            bus.unsubscribe(self.kind, self.id);
        }
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
        let _id = bus.subscribe(
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
        let _id = bus.subscribe(
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

    #[tokio::test]
    async fn subscriber_count_and_clear() {
        let bus = EventBus::default();

        assert_eq!(bus.subscriber_count(EventKind::Error), 0);

        let _id1 = bus.subscribe(EventKind::Error, Arc::new(|_| {}));
        let _id2 = bus.subscribe(EventKind::Error, Arc::new(|_| {}));
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

        let _id = bus.subscribe(
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

    #[tokio::test]
    async fn subscribe_returns_unique_ids() {
        let bus = EventBus::default();
        let id1 = bus.subscribe(EventKind::Error, Arc::new(|_| {}));
        let id2 = bus.subscribe(EventKind::Error, Arc::new(|_| {}));
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn unsubscribe_removes_callback() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bus = EventBus::default();

        let c = Arc::clone(&counter);
        let id = bus.subscribe(
            EventKind::ToolExecuted,
            Arc::new(move |_event| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert_eq!(bus.subscriber_count(EventKind::ToolExecuted), 1);

        bus.unsubscribe(EventKind::ToolExecuted, id);
        assert_eq!(bus.subscriber_count(EventKind::ToolExecuted), 0);

        bus.emit_simple(EventKind::ToolExecuted, serde_json::json!({}));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn emit_returns_delivered_stats() {
        let bus = EventBus::default();
        let _id = bus.subscribe(EventKind::Error, Arc::new(|_| {}));
        let _id2 = bus.subscribe(EventKind::Error, Arc::new(|_| {}));

        let stats = bus.emit(Event::new(EventKind::Error, serde_json::json!({})));
        assert_eq!(stats.delivered, 2);
        assert_eq!(stats.dropped, 0);
        assert_eq!(stats.total(), 2);
    }

    #[tokio::test]
    async fn emit_no_subscribers_returns_zero_stats() {
        let bus = EventBus::default();
        let stats = bus.emit(Event::new(EventKind::StreamChunk, serde_json::json!({})));
        assert_eq!(stats.delivered, 0);
        assert_eq!(stats.dropped, 0);
    }

    /// Helper: a callback that blocks the drainer for `dur` on each call.
    /// This lets tests deterministically fill the bounded channel.
    fn blocking_callback(dur: std::time::Duration) -> SubscriberFn {
        Arc::new(move |_event| {
            std::thread::sleep(dur);
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_channel_drops_events_under_drop_policy() {
        let bus = EventBus::default();
        let _id = bus.subscribe_with(
            EventKind::StreamChunk,
            blocking_callback(std::time::Duration::from_secs(2)),
            2,
            OverflowPolicy::Drop,
        );

        // Emit faster than the (very slow) callback can drain. With buffer
        // size 2, we expect drops within a small number of emits.
        let mut total_dropped = 0;
        for i in 0..20 {
            let stats = bus.emit(Event::new(
                EventKind::StreamChunk,
                serde_json::json!({"i": i}),
            ));
            total_dropped += stats.dropped;
        }
        assert!(
            total_dropped > 0,
            "expected at least one drop with full bounded channel, got {total_dropped}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn warn_and_drop_policy_records_drops() {
        let bus = EventBus::default();
        let _id = bus.subscribe_with(
            EventKind::Error,
            blocking_callback(std::time::Duration::from_secs(2)),
            1,
            OverflowPolicy::WarnAndDrop,
        );

        let mut dropped = 0;
        for _ in 0..10 {
            let stats = bus.emit(Event::new(EventKind::Error, serde_json::json!({})));
            dropped += stats.dropped;
        }
        assert!(dropped >= 1, "expected at least one drop, got {dropped}");
    }

    #[tokio::test]
    async fn subscription_handle_unsubscribes_on_drop() {
        let counter = Arc::new(AtomicUsize::new(0));
        let bus = Arc::new(EventBus::default());

        let c = Arc::clone(&counter);
        {
            let _handle = bus.subscribe_with_handle(
                EventKind::ToolExecuted,
                Arc::new(move |_event| {
                    c.fetch_add(1, Ordering::SeqCst);
                }),
            );
            assert_eq!(bus.subscriber_count(EventKind::ToolExecuted), 1);
            // _handle drops here
        }

        assert_eq!(bus.subscriber_count(EventKind::ToolExecuted), 0);

        bus.emit_simple(EventKind::ToolExecuted, serde_json::json!({}));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }
}
