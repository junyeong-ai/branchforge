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
#[non_exhaustive]
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

    /// Enqueue a text-only assistant response. Shortcut for
    /// `then_response(ModelResponse::from_text(text))`.
    pub fn then_text(self, text: impl Into<String>) -> Self {
        self.then_response(ModelResponse::from_text(text))
    }

    /// Enqueue an assistant response that calls a single tool.
    /// Shortcut for `then_response(ModelResponse::from_tool_call(...))`.
    pub fn then_tool_call(
        self,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        self.then_response(ModelResponse::from_tool_call(id, name, arguments))
    }

    /// Enqueue an assistant response that narrates a text block and
    /// then calls a tool — the common "let me check …" pattern.
    /// Shortcut for
    /// `then_response(ModelResponse::from_text_and_tool_call(...))`.
    pub fn then_text_and_tool_call(
        self,
        text: impl Into<String>,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        self.then_response(ModelResponse::from_text_and_tool_call(
            text, id, name, arguments,
        ))
    }

    /// Enqueue a streaming response as a vec of chunk results.
    pub fn then_stream(self, chunks: Vec<Result<ModelStreamChunk>>) -> Self {
        self.queue
            .lock()
            .unwrap()
            .push_back(MockResponse::Stream(chunks));
        self
    }

    /// Enqueue a scripted text-only streaming response: a
    /// `MessageStart`, one `TextDelta` per chunk in `deltas`, then a
    /// `Finish` with `FinishReason::Stop`. Matches the wire shape
    /// every real codec emits, so agent-loop tests that exercise
    /// streaming can avoid hand-rolling the Start/Delta/Finish
    /// triad.
    pub fn then_stream_text(self, deltas: impl IntoIterator<Item = impl Into<String>>) -> Self {
        use crate::ir::{FinishReason, Role, Usage};

        let mut chunks: Vec<Result<ModelStreamChunk>> = vec![Ok(ModelStreamChunk::MessageStart {
            id: String::new(),
            model: String::new(),
            role: Role::Assistant,
        })];
        for delta in deltas {
            chunks.push(Ok(ModelStreamChunk::TextDelta {
                index: 0,
                text: delta.into(),
            }));
        }
        chunks.push(Ok(ModelStreamChunk::Finish {
            reason: FinishReason::Stop,
            usage: Usage::default(),
        }));
        self.then_stream(chunks)
    }

    /// Enqueue a scripted streaming response that emits one tool
    /// call. The generated sequence is
    /// `MessageStart → ToolCallStart → ToolCallArgsDelta → ToolCallEnd → Finish(ToolCalls)`
    /// with `arguments` serialised as a single JSON fragment —
    /// matches the canonical wire shape every codec produces for
    /// non-streaming tool arguments.
    pub fn then_stream_tool_call(
        self,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        use crate::ir::{FinishReason, Role, ToolOrigin, Usage};

        let partial_json = serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string());
        let chunks: Vec<Result<ModelStreamChunk>> = vec![
            Ok(ModelStreamChunk::MessageStart {
                id: String::new(),
                model: String::new(),
                role: Role::Assistant,
            }),
            Ok(ModelStreamChunk::ToolCallStart {
                index: 0,
                id: id.into(),
                name: name.into(),
                origin: ToolOrigin::Local,
            }),
            Ok(ModelStreamChunk::ToolCallArgsDelta {
                index: 0,
                partial_json,
            }),
            Ok(ModelStreamChunk::ToolCallEnd { index: 0 }),
            Ok(ModelStreamChunk::Finish {
                reason: FinishReason::ToolCalls,
                usage: Usage::default(),
            }),
        ];
        self.then_stream(chunks)
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

    #[tokio::test]
    async fn then_text_shortcut() {
        let mock = MockLlmCall::new().then_text("hi");
        let resp = mock.send(&simple_request()).await.unwrap();
        assert_eq!(resp.text(), "hi");
    }

    #[tokio::test]
    async fn then_tool_call_shortcut() {
        let mock =
            MockLlmCall::new().then_tool_call("call_1", "Bash", serde_json::json!({"cmd": "ls"}));
        let resp = mock.send(&simple_request()).await.unwrap();
        assert!(matches!(resp.finish_reason, FinishReason::ToolCalls));
        let tool_calls: Vec<_> = resp.tool_calls().collect();
        assert_eq!(tool_calls.len(), 1);
        if let crate::ir::ContentPart::ToolCall { id, name, .. } = tool_calls[0] {
            assert_eq!(id, "call_1");
            assert_eq!(name, "Bash");
        } else {
            panic!("expected ToolCall");
        }
    }

    #[tokio::test]
    async fn then_stream_text_builds_full_chunk_sequence() {
        let mock = MockLlmCall::new().then_stream_text(["Hello ", "world"]);
        let stream = mock
            .send_stream(
                &simple_request(),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        let chunks: Vec<_> = stream.collect().await;
        // Start + 2 deltas + Finish
        assert_eq!(chunks.len(), 4);
        assert!(matches!(
            chunks[0].as_ref().unwrap(),
            ModelStreamChunk::MessageStart { .. }
        ));
        assert!(matches!(
            chunks[3].as_ref().unwrap(),
            ModelStreamChunk::Finish { .. }
        ));
    }

    #[tokio::test]
    async fn then_stream_tool_call_builds_full_sequence() {
        let mock = MockLlmCall::new().then_stream_tool_call(
            "call_1",
            "Bash",
            serde_json::json!({"cmd": "ls"}),
        );
        let stream = mock
            .send_stream(
                &simple_request(),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        let chunks: Vec<_> = stream.collect().await;
        // MessageStart + ToolCallStart + ToolCallArgsDelta + ToolCallEnd + Finish(ToolCalls)
        assert_eq!(chunks.len(), 5);
        assert!(matches!(
            chunks[1].as_ref().unwrap(),
            ModelStreamChunk::ToolCallStart { .. }
        ));
        assert!(matches!(
            chunks[4].as_ref().unwrap(),
            ModelStreamChunk::Finish {
                reason: FinishReason::ToolCalls,
                ..
            }
        ));
    }

    /// B-4 end-to-end: a three-turn scripted conversation
    /// (text + tool call → text → text) using nothing but the
    /// fluent `then_*` helpers. Proves the full agent-loop shape
    /// is testable without hand-rolling a single JSON literal.
    #[tokio::test]
    async fn scripted_three_turn_conversation() {
        let mock = MockLlmCall::new()
            .then_text_and_tool_call(
                "Let me search",
                "call_1",
                "Search",
                serde_json::json!({"query": "rust async"}),
            )
            .then_text("Here are the results")
            .then_text("Done");

        // 3 items in the queue.
        assert_eq!(mock.remaining(), 3);

        // First turn: assistant narrates + calls a tool.
        let r1 = mock.send(&simple_request()).await.unwrap();
        assert_eq!(r1.text(), "Let me search");
        assert!(matches!(r1.finish_reason, FinishReason::ToolCalls));

        // Second turn: assistant reports after tool result fed in.
        let r2 = mock.send(&simple_request()).await.unwrap();
        assert_eq!(r2.text(), "Here are the results");

        // Third turn: final answer.
        let r3 = mock.send(&simple_request()).await.unwrap();
        assert_eq!(r3.text(), "Done");

        assert_eq!(mock.call_count(), 3);
        assert_eq!(mock.remaining(), 0);
    }
}
