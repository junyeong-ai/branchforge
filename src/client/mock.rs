//! `MockLlmCall` — test double for [`LlmCall`].
//!
//! Provides a queue-based mock that returns pre-configured responses in
//! order. Useful for deterministic agent-loop and retry tests.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream;

use super::llm_call::LlmCall;
use super::provider_client::ChunkStream;
use crate::Result;
use crate::ir::{ModelResponse, ModelStreamChunk};

/// One scripted response in the mock queue.
pub enum MockResponse {
    /// A complete unary response.
    Unary(Box<ModelResponse>),
    /// A streaming response delivered as a sequence of chunks.
    Stream(Vec<Result<ModelStreamChunk>>),
    /// An error to return from `send()` or `send_stream()`.
    Error(crate::Error),
}

impl std::fmt::Debug for MockResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unary(r) => f.debug_tuple("Unary").field(&r.id).finish(),
            Self::Stream(chunks) => f.debug_tuple("Stream").field(&chunks.len()).finish(),
            Self::Error(e) => f.debug_tuple("Error").field(e).finish(),
        }
    }
}

/// A test double for [`LlmCall`] backed by a FIFO queue of scripted
/// responses.
///
/// # Example
///
/// ```ignore
/// let mock = MockLlmCall::new()
///     .then_response(ModelResponse::from_text("hello"))
///     .then_error(Error::Config("boom".into()));
/// assert_eq!(mock.remaining(), 2);
/// ```
pub struct MockLlmCall {
    queue: Mutex<VecDeque<MockResponse>>,
    calls: AtomicUsize,
}

impl std::fmt::Debug for MockLlmCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockLlmCall")
            .field("calls", &self.calls.load(Ordering::Relaxed))
            .field("remaining", &self.remaining())
            .finish()
    }
}

impl MockLlmCall {
    /// Create an empty mock with no scripted responses.
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            calls: AtomicUsize::new(0),
        }
    }

    /// Enqueue a unary [`ModelResponse`].
    pub fn then_response(self, resp: ModelResponse) -> Self {
        self.queue
            .lock()
            .unwrap()
            .push_back(MockResponse::Unary(Box::new(resp)));
        self
    }

    /// Enqueue a streaming response as a vec of chunk results.
    pub fn then_stream(self, chunks: Vec<Result<ModelStreamChunk>>) -> Self {
        self.queue
            .lock()
            .unwrap()
            .push_back(MockResponse::Stream(chunks));
        self
    }

    /// Enqueue an error.
    pub fn then_error(self, err: crate::Error) -> Self {
        self.queue
            .lock()
            .unwrap()
            .push_back(MockResponse::Error(err));
        self
    }

    /// Number of `send()` or `send_stream()` calls made so far.
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    /// Number of scripted responses still in the queue.
    pub fn remaining(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    fn pop(&self) -> MockResponse {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.queue
            .lock()
            .unwrap()
            .pop_front()
            .expect("MockLlmCall: queue exhausted — enqueue more responses")
    }
}

impl Default for MockLlmCall {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmCall for MockLlmCall {
    async fn send(&self, _request: &crate::ir::ModelRequest) -> Result<ModelResponse> {
        match self.pop() {
            MockResponse::Unary(r) => Ok(*r),
            MockResponse::Error(e) => Err(e),
            MockResponse::Stream(_) => Err(crate::Error::Config(
                "MockLlmCall: send() called but next queued item is Stream; use send_stream()"
                    .into(),
            )),
        }
    }

    async fn send_stream(
        &self,
        _request: &crate::ir::ModelRequest,
        _cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<ChunkStream> {
        match self.pop() {
            MockResponse::Stream(chunks) => Ok(Box::pin(stream::iter(chunks))),
            MockResponse::Error(e) => Err(e),
            MockResponse::Unary(_) => Err(crate::Error::Config(
                "MockLlmCall: send_stream() called but next queued item is Unary; use send()"
                    .into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use crate::ir::{FinishReason, Message, ModelRequest, ModelResponse, ModelStreamChunk, Usage};

    fn simple_request() -> ModelRequest {
        ModelRequest::new("test-model", vec![Message::user("hi")])
    }

    #[tokio::test]
    async fn unary_response() {
        let mock = MockLlmCall::new().then_response(ModelResponse::from_text("hello"));

        let resp = mock.send(&simple_request()).await.unwrap();
        assert_eq!(resp.text(), "hello");
        assert_eq!(mock.call_count(), 1);
        assert_eq!(mock.remaining(), 0);
    }

    #[tokio::test]
    async fn stream_response() {
        let chunks = vec![
            Ok(ModelStreamChunk::MessageStart {
                id: "msg_1".into(),
                model: "test".into(),
                role: crate::ir::Role::Assistant,
            }),
            Ok(ModelStreamChunk::TextDelta {
                index: 0,
                text: "hi".into(),
            }),
            Ok(ModelStreamChunk::Finish {
                reason: FinishReason::Stop,
                usage: Usage::default(),
            }),
        ];

        let mock = MockLlmCall::new().then_stream(chunks);
        let stream = mock
            .send_stream(
                &simple_request(),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        let collected: Vec<_> = stream.collect().await;
        assert_eq!(collected.len(), 3);
        assert_eq!(mock.call_count(), 1);
    }

    #[tokio::test]
    async fn error_response() {
        let mock = MockLlmCall::new().then_error(crate::Error::Config("boom".into()));

        let err = mock.send(&simple_request()).await.unwrap_err();
        assert!(err.to_string().contains("boom"));
        assert_eq!(mock.call_count(), 1);
    }

    #[tokio::test]
    async fn multiple_responses_in_order() {
        let mock = MockLlmCall::new()
            .then_response(ModelResponse::from_text("first"))
            .then_response(ModelResponse::from_text("second"))
            .then_error(crate::Error::Config("third".into()));

        assert_eq!(mock.remaining(), 3);

        let r1 = mock.send(&simple_request()).await.unwrap();
        assert_eq!(r1.text(), "first");

        let r2 = mock.send(&simple_request()).await.unwrap();
        assert_eq!(r2.text(), "second");

        let err = mock.send(&simple_request()).await.unwrap_err();
        assert!(err.to_string().contains("third"));

        assert_eq!(mock.call_count(), 3);
        assert_eq!(mock.remaining(), 0);
    }

    #[tokio::test]
    #[should_panic(expected = "queue exhausted")]
    async fn panics_when_queue_empty() {
        let mock = MockLlmCall::new();
        let _ = mock.send(&simple_request()).await;
    }
}
