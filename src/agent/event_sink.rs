//! Phase D Workstream D-1: host-neutral agent event sink.
//!
//! # Positioning
//!
//! BranchForge ships as a **dual-use SDK**: the same crate can power
//! a CLI REPL and an HTTP API server without a second architecture.
//! The single missing primitive for that positioning has always
//! been a clean way to emit [`super::AgentEvent`]s to the host
//! application's preferred transport. Consumers already had
//! `Agent::execute_stream()` which returns a `Stream<Item = Result<AgentEvent>>`,
//! but every host ended up hand-rolling a loop like:
//!
//! ```text
//! let stream = agent.execute_stream(prompt).await?;
//! futures::pin_mut!(stream);
//! while let Some(event) = stream.next().await {
//!     match event? {
//!         AgentEvent::Text { delta } => stdout.write_all(delta.as_bytes())?,
//!         AgentEvent::ToolStart { .. } => { /* ... */ },
//!         // ...
//!     }
//! }
//! ```
//!
//! Each host then reinvented:
//! - NDJSON framing for pipes / CLI
//! - SSE framing for HTTP streaming responses
//! - A channel adapter for tests
//! - A noop sink for silent/batch execution
//!
//! This module provides the canonical [`AgentEventSink`] trait plus
//! four reference implementations that cover those four cases.
//! Host code shrinks to:
//!
//! ```text
//! let sink = SseSink::new(response_body);
//! agent.execute_stream_into(prompt, sink).await?;
//! ```
//!
//! # Extensibility
//!
//! [`AgentEventSink`] is a single async trait. Any host that needs a
//! new egress format (WebSocket frames, Kafka, gRPC streaming, a
//! structured logger) implements one method and plugs in. There is
//! no central registry, no feature gate, no dispatch table — this
//! is the same open-closed pattern [`crate::client::codec::ModelCodec`]
//! follows for provider codecs.

#![allow(missing_docs)]

use std::sync::Arc;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc};

use super::AgentEvent;

/// Errors returned by [`AgentEventSink::emit`] and
/// [`AgentEventSink::flush`]. Every variant carries enough context
/// for the agent loop to decide whether to keep streaming or abort.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    /// The underlying transport (stdout pipe, HTTP body, channel
    /// receiver) is gone. The agent loop treats this as a
    /// non-recoverable "consumer disconnected" and stops.
    #[error("agent event sink is closed: {0}")]
    Closed(String),
    /// The event could not be serialised to the sink's wire format.
    #[error("agent event serialisation failed: {0}")]
    Serialise(String),
    /// The transport returned an I/O error.
    #[error("agent event sink I/O: {0}")]
    Io(String),
}

impl From<serde_json::Error> for SinkError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialise(e.to_string())
    }
}

impl From<std::io::Error> for SinkError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// Host-neutral agent event egress trait.
///
/// Implementors own a single output transport (stdout, an HTTP
/// response body, a test channel, an S3 object, …) and emit each
/// [`AgentEvent`] the agent loop produces. The contract is:
///
/// - `emit` MUST NOT block — a slow sink stalls the agent loop,
///   which is the whole motivation for streaming backpressure
///   (Workstream D-2).
/// - `emit` MAY buffer internally; `flush` drains any buffer.
/// - Returning `Err(SinkError::Closed)` tells the agent loop the
///   consumer is gone. The loop stops emitting and shuts down
///   gracefully on the next iteration.
/// - Other error kinds are logged at `warn!` but streaming continues
///   — one malformed event does not terminate the session.
#[async_trait]
pub trait AgentEventSink: Send + Sync {
    /// Emit a single agent event. Called on the hot path — keep
    /// this as fast as the transport allows.
    async fn emit(&self, event: &AgentEvent) -> Result<(), SinkError>;

    /// Flush any internally buffered events. Default is a no-op
    /// for sinks that are already unbuffered. Called at the end
    /// of a stream run before `execute_stream_into` returns.
    async fn flush(&self) -> Result<(), SinkError> {
        Ok(())
    }
}

// ── Reference implementations ───────────────────────────────────

/// Silent sink that drops every event. Useful for batch /
/// unattended runs and as a default when no egress is configured.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSink;

#[async_trait]
impl AgentEventSink for NoopSink {
    async fn emit(&self, _event: &AgentEvent) -> Result<(), SinkError> {
        Ok(())
    }
}

/// In-memory mpsc channel sink. Wraps a `tokio::sync::mpsc::Sender`
/// so test harnesses and in-process consumers can pull events via
/// a regular channel receiver. When the receiver is dropped the
/// next `emit` returns [`SinkError::Closed`] and the agent loop
/// stops.
#[derive(Debug, Clone)]
pub struct ChannelSink {
    tx: mpsc::Sender<AgentEvent>,
}

impl ChannelSink {
    /// Construct a new channel sink around a bounded sender. The
    /// caller retains the matching receiver and drains it at its
    /// own pace.
    pub fn new(tx: mpsc::Sender<AgentEvent>) -> Self {
        Self { tx }
    }

    /// Convenience: allocate a bounded channel with capacity
    /// `buffer` and return `(sink, receiver)`. The caller can then
    /// spawn a draining task while passing the sink to
    /// [`crate::agent::Agent::execute_stream_into`].
    pub fn bounded(buffer: usize) -> (Self, mpsc::Receiver<AgentEvent>) {
        let (tx, rx) = mpsc::channel(buffer);
        (Self { tx }, rx)
    }
}

#[async_trait]
impl AgentEventSink for ChannelSink {
    async fn emit(&self, event: &AgentEvent) -> Result<(), SinkError> {
        self.tx
            .send(event.clone())
            .await
            .map_err(|e| SinkError::Closed(e.to_string()))
    }
}

/// Phase D D-2: bounded decorator that drops events instead of
/// blocking when the inner sink is slow.
///
/// # Motivation
///
/// Every reference sink in this module is **backpressured** — a
/// slow consumer stalls the agent loop. That is the correct
/// default for CLI REPLs, batch jobs, and tests. But for
/// production API servers, a single slow client should not be
/// able to freeze the agent process indefinitely. `DroppingSink`
/// is the escape hatch: wrap any [`AgentEventSink`] and get
/// best-effort delivery with a bounded drop counter.
///
/// # Semantics
///
/// - `emit` calls the inner sink with [`tokio::time::timeout`]
///   of `per_event_timeout`. When the timeout expires, the event
///   is dropped and `dropped_count` is incremented.
/// - Critical events — [`AgentEvent::Complete`] and
///   [`AgentEvent::Init`] — are **never dropped**. They carry the
///   load-bearing session state that the host needs to render
///   the final result; dropping them would corrupt the caller's
///   view of the run.
/// - The drop counter is exposed via [`Self::dropped`] so hosts
///   can surface "N events lost" to operators after the stream
///   ends.
///
/// # Positioning
///
/// `DroppingSink` is a **decorator**, not a replacement for the
/// pull-based backpressure built into the rest of the stack.
/// The internal agent channels (progress channel, `ChunkStream`,
/// `execute_stream`) stay backpressured. This type is only for
/// the egress boundary where the host application decides
/// "drop is better than hang".
pub struct DroppingSink<Inner: AgentEventSink> {
    inner: Inner,
    per_event_timeout: std::time::Duration,
    dropped: std::sync::atomic::AtomicUsize,
}

impl<Inner: AgentEventSink> DroppingSink<Inner> {
    /// Wrap `inner` with a per-event timeout. Events that take
    /// longer than `per_event_timeout` to emit are dropped and
    /// counted in [`Self::dropped`].
    pub fn new(inner: Inner, per_event_timeout: std::time::Duration) -> Self {
        Self {
            inner,
            per_event_timeout,
            dropped: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Total events dropped since this sink was constructed.
    /// Thread-safe via a relaxed atomic load.
    pub fn dropped(&self) -> usize {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl<Inner: AgentEventSink + std::fmt::Debug> std::fmt::Debug for DroppingSink<Inner> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DroppingSink")
            .field("inner", &self.inner)
            .field("per_event_timeout", &self.per_event_timeout)
            .field("dropped", &self.dropped())
            .finish()
    }
}

/// `true` when the event is load-bearing and must never be
/// dropped by the overflow path. Exposed as a free function so
/// custom sink decorators in downstream crates can reuse the
/// same "critical events" vocabulary.
pub fn event_is_critical(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::Complete(_) | AgentEvent::Init { .. })
}

#[async_trait]
impl<Inner: AgentEventSink> AgentEventSink for DroppingSink<Inner> {
    async fn emit(&self, event: &AgentEvent) -> Result<(), SinkError> {
        if event_is_critical(event) {
            // Critical events bypass the timeout entirely — we
            // block until the inner sink accepts them.
            return self.inner.emit(event).await;
        }

        match tokio::time::timeout(self.per_event_timeout, self.inner.emit(event)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(SinkError::Closed(msg))) => Err(SinkError::Closed(msg)),
            Ok(Err(other)) => Err(other),
            Err(_) => {
                // Timed out — drop the event and record the miss.
                self.dropped
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
        }
    }

    async fn flush(&self) -> Result<(), SinkError> {
        self.inner.flush().await
    }
}

/// Newline-delimited JSON sink. Writes each event as a single line
/// of JSON followed by `\n`, suitable for piping into `jq`, feeding
/// into a line-based log aggregator, or delivering over a plain
/// HTTP stream that the client parses line-by-line.
///
/// Wraps the writer in a `tokio::sync::Mutex` so the sink is
/// `Send + Sync` even when `W` is not (the concrete write handle,
/// e.g. a `TcpStream`, is pinned to one task via the mutex guard).
pub struct NdjsonSink<W: AsyncWrite + Unpin + Send> {
    writer: Arc<Mutex<W>>,
}

impl<W: AsyncWrite + Unpin + Send> NdjsonSink<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
        }
    }
}

impl<W: AsyncWrite + Unpin + Send> std::fmt::Debug for NdjsonSink<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NdjsonSink").finish_non_exhaustive()
    }
}

#[async_trait]
impl<W: AsyncWrite + Unpin + Send + Sync + 'static> AgentEventSink for NdjsonSink<W> {
    async fn emit(&self, event: &AgentEvent) -> Result<(), SinkError> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        let mut w = self.writer.lock().await;
        w.write_all(&line).await?;
        Ok(())
    }

    async fn flush(&self) -> Result<(), SinkError> {
        let mut w = self.writer.lock().await;
        w.flush().await?;
        Ok(())
    }
}

/// Server-Sent Events sink. Writes each event in the
/// `event: <type>\ndata: <json>\n\n` form mandated by the HTML5
/// SSE spec, which every browser's `EventSource` understands out
/// of the box.
///
/// The `event:` field uses the agent event's tag
/// ([`AgentEvent::event_type`]) so a browser-side handler can
/// subscribe to e.g. `event: tool_start` without parsing the
/// JSON payload.
pub struct SseSink<W: AsyncWrite + Unpin + Send> {
    writer: Arc<Mutex<W>>,
}

impl<W: AsyncWrite + Unpin + Send> SseSink<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer: Arc::new(Mutex::new(writer)),
        }
    }
}

impl<W: AsyncWrite + Unpin + Send> std::fmt::Debug for SseSink<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseSink").finish_non_exhaustive()
    }
}

#[async_trait]
impl<W: AsyncWrite + Unpin + Send + Sync + 'static> AgentEventSink for SseSink<W> {
    async fn emit(&self, event: &AgentEvent) -> Result<(), SinkError> {
        let json = serde_json::to_string(event)?;
        let mut frame = Vec::with_capacity(json.len() + 32);
        frame.extend_from_slice(b"event: ");
        frame.extend_from_slice(event.event_type().as_bytes());
        frame.extend_from_slice(b"\ndata: ");
        frame.extend_from_slice(json.as_bytes());
        frame.extend_from_slice(b"\n\n");

        let mut w = self.writer.lock().await;
        w.write_all(&frame).await?;
        // SSE clients generally want each event to arrive
        // immediately rather than being held in a buffer. Flushing
        // per emit preserves low-latency semantics at the cost of
        // a syscall per event — acceptable for the expected volume.
        w.flush().await?;
        Ok(())
    }

    async fn flush(&self) -> Result<(), SinkError> {
        let mut w = self.writer.lock().await;
        w.flush().await?;
        Ok(())
    }
}

// ── Stream driver ───────────────────────────────────────────────

/// Drive an [`AgentEvent`] stream into a sink. Used by
/// [`crate::agent::Agent::execute_stream_into`] and available as a
/// free function for hosts that already have a stream in hand
/// (e.g. after applying their own middleware).
///
/// Terminates on the first [`SinkError::Closed`] — every other
/// error is logged at `warn!` and streaming continues so a
/// transient serialisation failure does not abort the whole run.
/// On normal stream end, flushes the sink and returns any error
/// from the stream itself (e.g. a provider failure that produced
/// `Err` instead of `Ok(AgentEvent::Complete(..))`).
pub async fn drive_stream_into_sink<S, K>(stream: S, sink: &K) -> crate::Result<()>
where
    S: Stream<Item = crate::Result<AgentEvent>> + Send,
    K: AgentEventSink + ?Sized,
{
    futures::pin_mut!(stream);
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => match sink.emit(&event).await {
                Ok(()) => {}
                Err(SinkError::Closed(reason)) => {
                    tracing::info!(%reason, "agent event sink closed; stopping stream");
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "agent event sink emit error; continuing");
                }
            },
            Err(e) => {
                // Flush before propagating so any events already
                // queued in a buffered sink make it to the host.
                let _ = sink.flush().await;
                return Err(e);
            }
        }
    }
    let _ = sink.flush().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentEvent;

    fn text_event(s: &str) -> AgentEvent {
        AgentEvent::Text {
            delta: s.to_string(),
        }
    }

    #[tokio::test]
    async fn noop_sink_accepts_everything() {
        let sink = NoopSink;
        assert!(sink.emit(&text_event("hi")).await.is_ok());
        assert!(sink.flush().await.is_ok());
    }

    #[tokio::test]
    async fn channel_sink_delivers_to_receiver() {
        let (sink, mut rx) = ChannelSink::bounded(4);
        sink.emit(&text_event("hello")).await.unwrap();
        sink.emit(&text_event("world")).await.unwrap();
        drop(sink);
        let mut out = Vec::new();
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::Text { delta } = ev {
                out.push(delta);
            }
        }
        assert_eq!(out, vec!["hello".to_string(), "world".to_string()]);
    }

    #[tokio::test]
    async fn channel_sink_reports_closed_receiver() {
        let (sink, rx) = ChannelSink::bounded(4);
        drop(rx);
        let err = sink.emit(&text_event("dead")).await.unwrap_err();
        assert!(matches!(err, SinkError::Closed(_)));
    }

    #[tokio::test]
    async fn ndjson_sink_writes_newline_delimited_json() {
        let buf: Vec<u8> = Vec::new();
        let sink = NdjsonSink::new(buf);
        sink.emit(&text_event("one")).await.unwrap();
        sink.emit(&text_event("two")).await.unwrap();
        sink.flush().await.unwrap();

        // Retrieve the buffer through the internal Arc<Mutex<W>>.
        let written = {
            let guard = sink.writer.lock().await;
            guard.clone()
        };
        let text = String::from_utf8(written).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["type"], "text");
        assert_eq!(parsed["delta"], "one");
    }

    #[tokio::test]
    async fn sse_sink_writes_event_and_data_lines() {
        let buf: Vec<u8> = Vec::new();
        let sink = SseSink::new(buf);
        sink.emit(&text_event("hi")).await.unwrap();

        let written = {
            let guard = sink.writer.lock().await;
            guard.clone()
        };
        let text = String::from_utf8(written).unwrap();
        assert!(text.starts_with("event: text\n"), "got: {text}");
        assert!(text.contains("data: "));
        assert!(text.ends_with("\n\n"));
        // Round-trip the data line through serde.
        let data_line = text
            .lines()
            .find_map(|l| l.strip_prefix("data: "))
            .expect("data line present");
        let parsed: serde_json::Value = serde_json::from_str(data_line).unwrap();
        assert_eq!(parsed["type"], "text");
        assert_eq!(parsed["delta"], "hi");
    }

    #[tokio::test]
    async fn drive_stream_into_sink_emits_each_event_and_flushes() {
        use futures::stream;
        let events: Vec<crate::Result<AgentEvent>> = vec![
            Ok(text_event("a")),
            Ok(text_event("b")),
            Ok(text_event("c")),
        ];
        let (sink, mut rx) = ChannelSink::bounded(16);
        let s = stream::iter(events);
        drive_stream_into_sink(s, &sink).await.unwrap();
        drop(sink);

        let mut collected = Vec::new();
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::Text { delta } = ev {
                collected.push(delta);
            }
        }
        assert_eq!(collected, vec!["a".to_string(), "b".into(), "c".into()]);
    }

    #[tokio::test]
    async fn dropping_sink_drops_non_critical_when_inner_slow() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        /// Inner sink that sleeps for 50ms on every non-critical
        /// event. With a 10ms per-event timeout the `DroppingSink`
        /// should drop every delta and pass Complete through.
        #[derive(Debug)]
        struct SlowInner {
            accepted: AtomicUsize,
        }
        #[async_trait]
        impl AgentEventSink for SlowInner {
            async fn emit(&self, _event: &AgentEvent) -> Result<(), SinkError> {
                tokio::time::sleep(Duration::from_millis(50)).await;
                self.accepted.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }

        let inner = SlowInner {
            accepted: AtomicUsize::new(0),
        };
        let sink = DroppingSink::new(inner, Duration::from_millis(10));

        // 3 Text deltas — all should time out and be dropped.
        sink.emit(&text_event("a")).await.unwrap();
        sink.emit(&text_event("b")).await.unwrap();
        sink.emit(&text_event("c")).await.unwrap();
        assert_eq!(sink.dropped(), 3, "all 3 slow emits must be dropped");

        // An Init event (critical) must bypass the timeout and
        // reach the inner sink, even though it takes >10ms.
        let init = AgentEvent::Init {
            model: "m".into(),
            execution_mode: "auto".into(),
            tools: vec![],
            subagents: vec![],
            skills: vec![],
            mcp_servers: vec![],
        };
        sink.emit(&init).await.unwrap();
        assert_eq!(sink.dropped(), 3, "critical events are not counted");
        assert_eq!(
            sink.inner.accepted.load(Ordering::Relaxed),
            1,
            "inner sink saw exactly the critical Init event"
        );
    }

    #[tokio::test]
    async fn event_is_critical_classifier() {
        use crate::agent::{AgentMetrics, AgentResult, AgentState};
        use crate::ir::FinishReason;

        assert!(event_is_critical(&AgentEvent::Init {
            model: "x".into(),
            execution_mode: "auto".into(),
            tools: vec![],
            subagents: vec![],
            skills: vec![],
            mcp_servers: vec![],
        }));

        let result = AgentResult {
            text: "done".into(),
            usage: crate::ir::Usage::default(),
            tool_calls: 0,
            iterations: 1,
            stop_reason: FinishReason::Stop,
            state: AgentState::Completed,
            metrics: AgentMetrics::default(),
            session_id: "s1".into(),
            structured_output: None,
            messages: vec![],
            uuid: "u1".into(),
        };
        assert!(event_is_critical(&AgentEvent::Complete(Box::new(result))));

        // Text deltas are not critical and may be dropped under
        // backpressure.
        assert!(!event_is_critical(&text_event("hi")));
    }

    #[tokio::test]
    async fn drive_stream_into_sink_stops_on_closed_sink() {
        use futures::stream;
        let events: Vec<crate::Result<AgentEvent>> =
            vec![Ok(text_event("keep")), Ok(text_event("dropped"))];
        let (sink, rx) = ChannelSink::bounded(1);
        drop(rx); // consumer gone immediately
        let s = stream::iter(events);
        // Should NOT return an error — a closed sink is a graceful
        // stop, not a failure of the agent loop.
        drive_stream_into_sink(s, &sink).await.unwrap();
    }
}
