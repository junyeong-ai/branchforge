//! SSE streaming support for the Anthropic Messages API.

use bytes::Bytes;
use futures::Stream;
use pin_project_lite::pin_project;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::recovery::StreamRecoveryState;
use crate::Result;
use crate::types::{Citation, ContentDelta, StreamEvent};

#[derive(Debug, Clone)]
pub enum StreamItem {
    Event(StreamEvent),
    Text(String),
    Thinking(String),
    Citation(Citation),
    ToolUseComplete(crate::types::ToolUseBlock),
}

/// Convert a [`StreamEvent`] into the corresponding [`StreamItem`].
///
/// This is used by both [`StreamParser`]'s default path and by provider
/// adapters that map their wire events to `StreamEvent` first.
pub fn stream_event_to_item(event: StreamEvent) -> StreamItem {
    match &event {
        StreamEvent::ContentBlockDelta {
            delta: ContentDelta::TextDelta { text },
            ..
        } => StreamItem::Text(text.clone()),
        StreamEvent::ContentBlockDelta {
            delta: ContentDelta::ThinkingDelta { thinking },
            ..
        } => StreamItem::Thinking(thinking.clone()),
        StreamEvent::ContentBlockDelta {
            delta: ContentDelta::CitationsDelta { citation },
            ..
        } => StreamItem::Citation(citation.clone()),
        _ => StreamItem::Event(event),
    }
}

pin_project! {
    pub struct StreamParser<S> {
        #[pin]
        inner: S,
        buffer: Vec<u8>,
        pos: usize,
        event_parser: Option<Box<dyn Fn(&str) -> Option<StreamItem> + Send + Sync>>,
    }
}

impl<S> StreamParser<S>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>>,
{
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: Vec::with_capacity(4096),
            pos: 0,
            event_parser: None,
        }
    }

    pub fn with_event_parser(
        inner: S,
        parser: impl Fn(&str) -> Option<StreamItem> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            buffer: Vec::with_capacity(4096),
            pos: 0,
            event_parser: Some(Box::new(parser)),
        }
    }

    #[inline]
    fn find_delimiter(buf: &[u8]) -> Option<usize> {
        buf.windows(2).position(|w| w == b"\n\n")
    }

    fn extract_json_data(event_block: &str) -> Option<&str> {
        for line in event_block.lines() {
            let line = line.trim();
            if let Some(json_str) = line.strip_prefix("data: ") {
                let json_str = json_str.trim();
                if json_str == "[DONE]"
                    || json_str.contains("\"type\": \"ping\"")
                    || json_str.contains("\"type\":\"ping\"")
                {
                    return None;
                }
                if !json_str.is_empty() {
                    return Some(json_str);
                }
            }
        }
        None
    }

    /// Parse an SSE event block into a [`StreamItem`], using the custom parser
    /// if one was provided, otherwise falling back to Anthropic `StreamEvent`
    /// deserialization.
    #[allow(clippy::needless_borrow)]
    fn try_parse(
        event_parser: &Option<Box<dyn Fn(&str) -> Option<StreamItem> + Send + Sync>>,
        event_block: &str,
    ) -> Option<StreamItem> {
        let trimmed = event_block.trim();
        if trimmed.is_empty() || trimmed.starts_with(':') {
            return None;
        }
        let json_str = Self::extract_json_data(event_block)?;
        if let Some(parser) = event_parser {
            parser(json_str)
        } else {
            serde_json::from_str::<StreamEvent>(json_str)
                .inspect_err(|e| {
                    tracing::warn!("Failed to parse stream event: {} - data: {}", e, json_str)
                })
                .ok()
                .map(stream_event_to_item)
        }
    }
}

impl<S> Stream for StreamParser<S>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>>,
{
    type Item = Result<StreamItem>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            let search_slice = &this.buffer[*this.pos..];
            if let Some(rel_pos) = Self::find_delimiter(search_slice) {
                let start_pos = *this.pos;
                let end_pos = start_pos + rel_pos;
                let event_block = match std::str::from_utf8(&this.buffer[start_pos..end_pos]) {
                    Ok(s) => s,
                    Err(e) => {
                        return Poll::Ready(Some(Err(crate::Error::Config(format!(
                            "Invalid UTF-8 in event: {}",
                            e
                        )))));
                    }
                };

                let item = Self::try_parse(&*this.event_parser, event_block);

                *this.pos = end_pos + 2;

                if this.buffer.len() > 8192 && *this.pos > this.buffer.len() / 2 {
                    this.buffer.drain(..*this.pos);
                    *this.pos = 0;
                }

                if let Some(item) = item {
                    return Poll::Ready(Some(Ok(item)));
                }
                continue;
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    if *this.pos > 0 && this.buffer.len() + bytes.len() > 16384 {
                        this.buffer.drain(..*this.pos);
                        *this.pos = 0;
                    }
                    this.buffer.extend_from_slice(&bytes);
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(crate::Error::Network(e))));
                }
                Poll::Ready(None) => {
                    if *this.pos < this.buffer.len() {
                        let remaining = match std::str::from_utf8(&this.buffer[*this.pos..]) {
                            Ok(s) => s,
                            Err(_) => return Poll::Ready(None),
                        };
                        let item = Self::try_parse(&*this.event_parser, remaining);
                        if let Some(item) = item {
                            return Poll::Ready(Some(Ok(item)));
                        }
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(feature = "aws")]
pin_project! {
    /// AWS Event Stream binary frame parser.
    ///
    /// Wraps a byte stream from Bedrock's `converse-stream` endpoint and yields
    /// [`StreamItem`]s by decoding the binary Event Stream frames, extracting the
    /// JSON payload from each, and converting them via a caller-supplied closure.
    pub struct AwsEventStreamParser<S> {
        #[pin]
        inner: S,
        decoder: crate::client::adapter::bedrock_stream::AwsEventStreamDecoder,
        pending: std::collections::VecDeque<StreamItem>,
        event_parser: Box<dyn Fn(&str) -> Option<StreamItem> + Send + Sync>,
    }
}

#[cfg(feature = "aws")]
impl<S> AwsEventStreamParser<S>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>>,
{
    pub fn new(
        inner: S,
        event_parser: impl Fn(&str) -> Option<StreamItem> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            decoder: crate::client::adapter::bedrock_stream::AwsEventStreamDecoder::new(),
            pending: std::collections::VecDeque::new(),
            event_parser: Box::new(event_parser),
        }
    }
}

#[cfg(feature = "aws")]
impl<S> Stream for AwsEventStreamParser<S>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>>,
{
    type Item = Result<StreamItem>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        loop {
            // Yield any already-decoded items first.
            if let Some(item) = this.pending.pop_front() {
                return Poll::Ready(Some(Ok(item)));
            }

            // Try to decode buffered frames.
            match this.decoder.decode_all() {
                Ok(messages) => {
                    for msg in messages {
                        let message_type = msg.header_str(":message-type").unwrap_or("");
                        let event_type = msg.header_str(":event-type").unwrap_or("");
                        tracing::trace!(
                            message_type,
                            event_type,
                            payload_len = msg.payload.len(),
                            "AwsEventStream frame decoded"
                        );
                        match message_type {
                            "event" => {
                                if let Some(json_str) = msg.payload_str()
                                    && !json_str.is_empty()
                                {
                                    // Prepend event type so the parser can dispatch
                                    // without needing a separate channel for headers.
                                    let prefixed = if !event_type.is_empty() {
                                        format!("__event_type={event_type}\n{json_str}")
                                    } else {
                                        json_str.to_string()
                                    };
                                    if let Some(item) = (this.event_parser)(&prefixed) {
                                        this.pending.push_back(item);
                                    }
                                }
                            }
                            "exception" => {
                                let exc_type = msg
                                    .header_str(":exception-type")
                                    .unwrap_or("unknown")
                                    .to_string();
                                let detail = msg.payload_str().unwrap_or("").to_string();
                                return Poll::Ready(Some(Err(crate::Error::Api {
                                    message: format!("{}: {}", exc_type, detail),
                                    status: None,
                                    error_type: Some(exc_type),
                                })));
                            }
                            _ => {
                                // Unknown message type; skip.
                            }
                        }
                    }
                }
                Err(e) => {
                    return Poll::Ready(Some(Err(crate::Error::Parse(e.to_string()))));
                }
            }

            // If we produced items from decoding, yield them next iteration.
            if !this.pending.is_empty() {
                continue;
            }

            // Need more data from the inner stream.
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    this.decoder.push(&bytes);
                    // Loop back to attempt decoding.
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(crate::Error::Network(e))));
                }
                Poll::Ready(None) => {
                    // Stream ended. Drain any remaining frames.
                    if let Ok(messages) = this.decoder.decode_all() {
                        for msg in messages {
                            if msg.header_str(":message-type") == Some("event")
                                && let Some(json_str) = msg.payload_str()
                                && !json_str.is_empty()
                            {
                                let et = msg.header_str(":event-type").unwrap_or("");
                                let prefixed = if !et.is_empty() {
                                    format!("__event_type={et}\n{json_str}")
                                } else {
                                    json_str.to_string()
                                };
                                if let Some(item) = (this.event_parser)(&prefixed) {
                                    this.pending.push_back(item);
                                }
                            }
                        }
                    }
                    if let Some(item) = this.pending.pop_front() {
                        return Poll::Ready(Some(Ok(item)));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

pin_project! {
    pub struct RecoverableStream<S> {
        #[pin]
        inner: StreamParser<S>,
        recovery: StreamRecoveryState,
        current_block_type: Option<BlockType>,
    }
}

#[derive(Debug, Clone, Copy)]
enum BlockType {
    Text,
    Thinking,
    ToolUse,
}

impl<S> RecoverableStream<S>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>>,
{
    pub fn new(inner: S) -> Self {
        Self {
            inner: StreamParser::new(inner),
            recovery: StreamRecoveryState::new(),
            current_block_type: None,
        }
    }

    pub fn with_event_parser(
        inner: S,
        parser: impl Fn(&str) -> Option<StreamItem> + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: StreamParser::with_event_parser(inner, parser),
            recovery: StreamRecoveryState::new(),
            current_block_type: None,
        }
    }

    pub fn recovery_state(&self) -> &StreamRecoveryState {
        &self.recovery
    }

    pub fn take_recovery_state(self) -> StreamRecoveryState {
        self.recovery
    }
}

impl<S> Stream for RecoverableStream<S>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>>,
{
    type Item = Result<StreamItem>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        match this.inner.poll_next(cx) {
            Poll::Ready(Some(Ok(item))) => {
                match &item {
                    StreamItem::Text(text) => {
                        *this.current_block_type = Some(BlockType::Text);
                        this.recovery.append_text(text);
                    }
                    StreamItem::Thinking(thinking) => {
                        *this.current_block_type = Some(BlockType::Thinking);
                        this.recovery.append_thinking(thinking);
                    }
                    StreamItem::ToolUseComplete(_) => {}
                    StreamItem::Event(event) => match event {
                        StreamEvent::ContentBlockStart {
                            content_block: crate::types::ContentBlock::ToolUse(tu),
                            ..
                        } => {
                            *this.current_block_type = Some(BlockType::ToolUse);
                            this.recovery.start_tool_use(tu.id.clone(), tu.name.clone());
                        }
                        StreamEvent::ContentBlockDelta {
                            delta: ContentDelta::InputJsonDelta { partial_json },
                            ..
                        } => {
                            this.recovery.append_tool_json(partial_json);
                        }
                        StreamEvent::ContentBlockDelta {
                            delta: ContentDelta::SignatureDelta { signature },
                            ..
                        } => {
                            this.recovery.append_signature(signature);
                        }
                        StreamEvent::ContentBlockStop { .. } => {
                            match this.current_block_type.take() {
                                Some(BlockType::Text) => this.recovery.complete_text_block(),
                                Some(BlockType::Thinking) => {
                                    this.recovery.complete_thinking_block()
                                }
                                Some(BlockType::ToolUse) => {
                                    if let Some(tool_use) = this.recovery.complete_tool_use_block()
                                    {
                                        return Poll::Ready(Some(Ok(StreamItem::ToolUseComplete(
                                            tool_use,
                                        ))));
                                    }
                                }
                                None => {}
                            }
                        }
                        _ => {}
                    },
                    StreamItem::Citation(_) => {}
                }
                Poll::Ready(Some(Ok(item)))
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type EmptyStream = futures::stream::Empty<std::result::Result<Bytes, reqwest::Error>>;

    #[test]
    fn test_parse_simple_data() {
        let data = r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let item = StreamParser::<EmptyStream>::try_parse(&None, data);
        assert!(item.is_some());
        assert!(matches!(item, Some(StreamItem::Text(_))));
    }

    #[test]
    fn test_parse_event_with_type() {
        let data = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}";
        let item = StreamParser::<EmptyStream>::try_parse(&None, data);
        assert!(item.is_some());
        assert!(matches!(item, Some(StreamItem::Text(_))));
    }

    #[test]
    fn test_parse_message_start() {
        let data = r#"event: message_start
data: {"type":"message_start","message":{"model":"claude-sonnet-4-5","id":"msg_123","type":"message","role":"assistant","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#;
        let item = StreamParser::<EmptyStream>::try_parse(&None, data);
        assert!(item.is_some());
        assert!(matches!(
            item,
            Some(StreamItem::Event(StreamEvent::MessageStart { .. }))
        ));
    }

    #[test]
    fn test_skip_done_marker() {
        let data = "data: [DONE]";
        let item = StreamParser::<EmptyStream>::try_parse(&None, data);
        assert!(item.is_none());
    }

    #[test]
    fn test_skip_ping_event() {
        let data = "event: ping\ndata: {\"type\": \"ping\"}";
        let item = StreamParser::<EmptyStream>::try_parse(&None, data);
        assert!(item.is_none());
    }

    #[test]
    fn test_skip_empty_block() {
        assert!(StreamParser::<EmptyStream>::try_parse(&None, "").is_none());
        assert!(StreamParser::<EmptyStream>::try_parse(&None, "   \n  ").is_none());
    }

    #[test]
    fn test_skip_comment() {
        let data = ": this is a comment";
        let item = StreamParser::<EmptyStream>::try_parse(&None, data);
        assert!(item.is_none());
    }

    #[test]
    fn test_extract_json_data() {
        let json = StreamParser::<EmptyStream>::extract_json_data("data: {\"foo\":\"bar\"}");
        assert_eq!(json, Some("{\"foo\":\"bar\"}"));

        let json =
            StreamParser::<EmptyStream>::extract_json_data("event: test\ndata: {\"foo\":\"bar\"}");
        assert_eq!(json, Some("{\"foo\":\"bar\"}"));

        let json = StreamParser::<EmptyStream>::extract_json_data("data: [DONE]");
        assert!(json.is_none());

        let json = StreamParser::<EmptyStream>::extract_json_data(
            "event: ping\ndata: {\"type\": \"ping\"}",
        );
        assert!(json.is_none());
    }
}
