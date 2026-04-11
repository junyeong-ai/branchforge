//! `ProviderClient` — composition of one [`ModelCodec`] and one
//! [`ModelTransport`] into something that can actually execute requests.
//!
//! This is the new minimal client surface that replaces the old monolithic
//! `Client + ProviderAdapter` design (which is still alongside it during
//! migration). It owns no retry policy, no fallback, no circuit breaker —
//! those layers will be re-introduced as `ModelMiddleware` in a later
//! phase.
//!
//! # Composition validation
//!
//! [`ProviderClient::new`] enforces three invariants at construction time:
//!
//! 1. [`ModelCodec::pinned_transport`] must match `transport.id()` if set.
//! 2. [`ModelTransport::supports_codec`] must return `true` for the
//!    codec's id.
//! 3. The codec's [`InvocationMode::Unary`] must be supported (always
//!    true today, but checked for forward compatibility).
//!
//! Invalid compositions return an [`Error::InvalidComposition`].

use std::pin::Pin;
use std::sync::Arc;

use futures::stream::{Stream, StreamExt};

use crate::client::codec::{InvocationMode, ModelCodec};
use crate::client::transport::ModelTransport;
use crate::ir::{ModelRequest, ModelResponse, ModelStreamChunk, StreamDecodeState, StreamFraming};
use crate::{Error, Result};

/// Boxed stream of decoded [`ModelStreamChunk`]s.
pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<ModelStreamChunk>> + Send>>;

/// One codec + one transport, ready to execute model requests.
#[derive(Clone)]
pub struct ProviderClient {
    codec: Arc<dyn ModelCodec>,
    transport: Arc<dyn ModelTransport>,
    http: reqwest::Client,
}

impl std::fmt::Debug for ProviderClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderClient")
            .field("codec", &self.codec.id())
            .field("transport", &self.transport.id())
            .finish()
    }
}

impl ProviderClient {
    /// Compose a codec and transport with composition checks.
    pub fn new(codec: Arc<dyn ModelCodec>, transport: Arc<dyn ModelTransport>) -> Result<Self> {
        validate_composition(codec.as_ref(), transport.as_ref())?;
        Ok(Self {
            codec,
            transport,
            http: reqwest::Client::new(),
        })
    }

    /// Compose with an externally-built `reqwest::Client` (for shared
    /// connection pools, custom timeouts, proxies, etc.).
    pub fn with_http(
        codec: Arc<dyn ModelCodec>,
        transport: Arc<dyn ModelTransport>,
        http: reqwest::Client,
    ) -> Result<Self> {
        validate_composition(codec.as_ref(), transport.as_ref())?;
        Ok(Self {
            codec,
            transport,
            http,
        })
    }

    /// Codec id.
    pub fn codec_id(&self) -> &'static str {
        self.codec.id()
    }

    /// Transport id.
    pub fn transport_id(&self) -> &'static str {
        self.transport.id()
    }

    /// Borrow the codec for capability/shape inspection.
    pub fn codec(&self) -> &dyn ModelCodec {
        self.codec.as_ref()
    }

    /// Borrow the transport for endpoint / header inspection. Used by
    /// agent-builder regression tests that assert OAuth-specific headers
    /// (`x-app`, `anthropic-beta`, …) and the `?beta=true` URL flag are
    /// applied when wired via [`crate::auth::Auth::ClaudeCli`] /
    /// [`crate::auth::Auth::OAuth`]. Read-only by design.
    pub fn transport(&self) -> &dyn crate::client::transport::ModelTransport {
        self.transport.as_ref()
    }

    /// Send a unary request and decode the response.
    ///
    /// Pipeline:
    /// 1. `codec.encode_request` — IR → wire body, collect warnings.
    /// 2. `transport.resolve_endpoint` — pick URL + headers based on the
    ///    codec's [`crate::client::codec::EndpointShape`].
    /// 3. POST the body, applying transport headers, codec headers, and
    ///    `transport.authorize` for auth.
    /// 4. Check status; map non-2xx to a typed error with hint.
    /// 5. `codec.decode_response` — wire JSON → IR.
    /// 6. Merge encode-time warnings into the final response.
    pub async fn send(&self, request: &ModelRequest) -> Result<ModelResponse> {
        let mode = InvocationMode::Unary;

        // Create the observability span for this API call. The span
        // lives for the full request lifetime; usage / cache / error
        // attributes are recorded as we learn them. `tracing-opentelemetry`
        // (optional, behind the `otel` feature for downstream consumers)
        // turns these attributes into OTel span attributes for free, and
        // a span-to-metric layer can derive histograms / counters from
        // the same events — no separate metrics wiring required on the
        // hot path.
        //
        // We use `Instrument::instrument` rather than a guard / `entered`
        // so the inner future remains `Send` (an `EnteredSpan` guard
        // held across `.await` would break the `Send` bound required
        // by `LlmCall::send`). The instrument wrapper attaches the span
        // to the future and re-enters it every time the executor polls,
        // which is exactly what we want for a cross-`.await` lifetime.
        use tracing::Instrument;
        let api_span =
            crate::observability::ApiCallSpan::with_system(&request.model, self.codec.id());
        let tracing_span = api_span.span().clone();

        let result = self
            .send_inner(request, mode)
            .instrument(tracing_span)
            .await;
        match &result {
            Ok(response) => {
                api_span.record_usage(response.usage.input_tokens, response.usage.output_tokens);
                if let (Some(read), Some(creation)) = (
                    response.usage.cached_input_tokens,
                    response.usage.cache_creation_tokens,
                ) {
                    api_span.record_cache(read, creation);
                } else if let Some(read) = response.usage.cached_input_tokens {
                    api_span.record_cache(read, 0);
                } else if let Some(creation) = response.usage.cache_creation_tokens {
                    api_span.record_cache(0, creation);
                }
                if let Some(reasoning) = response.usage.reasoning_tokens {
                    api_span.record_reasoning_tokens(reasoning);
                }
            }
            Err(err) => {
                api_span.record_error(err.category());
            }
        }
        api_span.finish();
        result
    }

    /// Inner request path, separated from [`send`] so the outer method
    /// can own the [`ApiCallSpan`] lifecycle (create → record on return
    /// → finish) without cluttering the send logic with observability
    /// bookkeeping.
    async fn send_inner(
        &self,
        request: &ModelRequest,
        mode: InvocationMode,
    ) -> Result<ModelResponse> {
        let encoded = self.codec.encode_request(request, mode)?;
        let endpoint = self
            .transport
            .resolve_endpoint(
                self.codec.endpoint_shape(),
                request.routing_model_id(),
                mode,
            )
            .await?;

        let body_bytes = serde_json::to_vec(&encoded.body)?;
        let mut req = self.http.post(&endpoint.url);
        for (name, value) in &endpoint.headers {
            req = req.header(name.as_str(), value.as_str());
        }
        req = self.transport.authorize(req, &body_bytes).await?;
        req = req.body(body_bytes);

        let response = req.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(classify_response_error(
                self.transport.as_ref(),
                status.as_u16(),
                &body,
            ));
        }

        let raw: serde_json::Value = response.json().await?;
        let mut decoded = self.codec.decode_response(raw, mode)?;
        decoded.warnings.extend(encoded.warnings);

        // Unexpected cache-break detection: the request declared one
        // or more cache markers (either top-level
        // `provider_options.anthropic.cache_control` or per-block
        // `SystemBlock.cache_marker`), but the response reported zero
        // `cached_input_tokens`. That mismatch is a strong signal
        // of a cache invalidation upstream — TTL expiry, rolling
        // deploy flush, or an unintentional request-shape change
        // that broke the prefix match. We surface it as a
        // `ModelWarning::LossyEncode` and emit a tracing event so
        // downstream observability can aggregate the rate.
        if request.has_cache_markers() && decoded.usage.cached_input_tokens.unwrap_or(0) == 0 {
            tracing::warn!(
                target: "branchforge::cache::unexpected_break",
                model = %request.model,
                codec = self.codec.id(),
                "Cache markers set but response reported zero cache hits — unexpected break"
            );
            decoded.warnings.push(crate::ir::ModelWarning::lossy(
                "cache.unexpected_break",
                "Request declared cache markers but response had 0 cached_input_tokens",
            ));
        }

        // Runtime JSON-Schema validation. Native structured-output
        // providers claim to honor the declared schema, but streaming
        // interruptions, degraded models, or plain provider bugs can
        // still produce non-conforming bodies. We validate after decode
        // so that application code that calls `response.json::<T>()` is
        // not the first line of defence.
        //
        // `spec.strict == false` is the caller's explicit opt-out —
        // they want the lenient path, so we downgrade any violation to
        // a warning on the response.
        if let Some(crate::ir::ResponseFormat::JsonSchema(spec)) = &request.response_format {
            let text = decoded.text();
            if !text.is_empty() {
                match crate::client::schema::validate_structured_output(&text, spec) {
                    Ok(()) => {}
                    Err(e) if spec.strict => {
                        return Err(validation_error_to_error(e));
                    }
                    Err(e) => {
                        decoded.warnings.push(crate::ir::ModelWarning::lossy(
                            "response_format.runtime_validation",
                            format!("non-strict schema violation: {e}"),
                        ));
                    }
                }
            }
        }

        Ok(decoded)
    }

    /// Send a streaming request and return a chunk stream.
    ///
    /// Pipeline:
    /// 1. `codec.encode_request(_, Stream)` — wire body, encode-time warnings.
    /// 2. `transport.resolve_endpoint(_, _, Stream)` — pick streaming URL.
    /// 3. POST + `transport.authorize`.
    /// 4. Frame the response body according to `codec.stream_framing()`.
    /// 5. For each frame, call `codec.decode_stream_chunk` with shared
    ///    [`StreamDecodeState`] and emit zero or more [`ModelStreamChunk`]s.
    /// 6. Encode-time warnings are surfaced as
    ///    [`ModelStreamChunk::Warning`] before the first decoded chunk.
    pub async fn send_stream(
        &self,
        request: &ModelRequest,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<ChunkStream> {
        let mode = InvocationMode::Stream;
        let encoded = self.codec.encode_request(request, mode)?;
        let endpoint = self
            .transport
            .resolve_endpoint(
                self.codec.endpoint_shape(),
                request.routing_model_id(),
                mode,
            )
            .await?;

        let body_bytes = serde_json::to_vec(&encoded.body)?;
        let mut req = self.http.post(&endpoint.url);
        for (name, value) in &endpoint.headers {
            req = req.header(name.as_str(), value.as_str());
        }
        req = self.transport.authorize(req, &body_bytes).await?;
        req = req.body(body_bytes);

        // Race the HTTP connect / headers phase against cancellation.
        // If the token fires before headers arrive, we drop the in-flight
        // request future, which releases the connection attempt.
        let response = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                return Err(Error::Stream("Streaming request cancelled before response headers".into()));
            }
            res = req.send() => res?,
        };
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(classify_response_error(
                self.transport.as_ref(),
                status.as_u16(),
                &body,
            ));
        }

        let framing = self.codec.stream_framing();
        let codec = self.codec.clone();
        let warnings = encoded.warnings;
        let byte_stream = response.bytes_stream();
        let chunk_stream = build_chunk_stream(codec, framing, byte_stream, warnings, cancel_token);
        Ok(chunk_stream)
    }
}

/// One step of the byte-stream → chunk-stream loop. Lifted to
/// module level so the `try_stream!` macro doesn't try to hoist a
/// generic local enum (which the macro cannot reliably do).
enum ByteStreamStep<T> {
    Chunk(T),
    End,
    Cancelled,
}

/// Wrap a `bytes_stream()` from `reqwest` with a framing-aware decoder
/// that drives `codec.decode_stream_chunk` per frame and yields
/// [`ModelStreamChunk`]s.
fn build_chunk_stream(
    codec: Arc<dyn ModelCodec>,
    framing: StreamFraming,
    byte_stream: impl Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
    warnings: Vec<crate::ir::ModelWarning>,
    cancel_token: tokio_util::sync::CancellationToken,
) -> ChunkStream {
    use async_stream::try_stream;

    let stream = try_stream! {
        // Surface encode-time warnings up front.
        for w in warnings {
            yield ModelStreamChunk::Warning(w);
        }

        let mut state = StreamDecodeState::new();
        let mut buffer: Vec<u8> = Vec::new();
        let mut aws_decoder = if matches!(framing, StreamFraming::AwsEventStream) {
            Some(crate::client::transport::bedrock_stream::AwsEventStreamDecoder::new())
        } else {
            None
        };
        futures::pin_mut!(byte_stream);

        loop {
            // When `cancel_token` fires, we return an error out of the
            // `try_stream!` closure. That drops the closure's locals —
            // including `byte_stream` and therefore the reqwest `Response`
            // body — which releases the hyper connection and closes the
            // TCP socket instead of letting it drain in the background.
            let step = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => ByteStreamStep::Cancelled,
                next = byte_stream.next() => match next {
                    Some(c) => ByteStreamStep::Chunk(c),
                    None => ByteStreamStep::End,
                },
            };
            let chunk_result = match step {
                ByteStreamStep::Chunk(c) => c,
                ByteStreamStep::End => break,
                ByteStreamStep::Cancelled => {
                    Err(Error::Stream("Streaming body cancelled".into()))?;
                    unreachable!()
                }
            };
            let chunk = chunk_result.map_err(Error::from)?;

            if let Some(decoder) = aws_decoder.as_mut() {
                // AWS EventStream framing: decode binary frames, dispatch
                // each (event_type, payload) pair to the codec's
                // event-stream handler.
                decoder.push(&chunk);
                let frames = decoder
                    .decode_all()
                    .map_err(|e| Error::Parse(format!("aws eventstream decode: {e}")))?;
                for msg in frames {
                    let event_type = msg.header_str(":event-type").unwrap_or("");
                    let chunks =
                        codec.decode_eventstream_frame(event_type, &msg.payload, &mut state)?;
                    for c in chunks {
                        yield c;
                    }
                }
                continue;
            }

            buffer.extend_from_slice(&chunk);

            // Pull every complete frame currently in the buffer.
            loop {
                let frame = match framing {
                    StreamFraming::Sse => extract_sse_event(&mut buffer),
                    StreamFraming::JsonArray => extract_json_array_element(&mut buffer),
                    StreamFraming::NdJson => extract_ndjson_line(&mut buffer),
                    StreamFraming::AwsEventStream => unreachable!("handled above"),
                };
                let frame = match frame {
                    Some(f) => f,
                    None => break,
                };
                if frame.is_empty() {
                    continue;
                }
                let chunks = codec.decode_stream_chunk(&frame, &mut state)?;
                for c in chunks {
                    yield c;
                }
            }
        }

        // Drain any final buffered frame for byte-oriented framings (e.g.
        // last NDJSON line without trailing newline). AWS EventStream is
        // self-delimiting and needs no flush.
        if !buffer.is_empty() && !matches!(framing, StreamFraming::AwsEventStream) {
            let trailing = std::mem::take(&mut buffer);
            let chunks = codec.decode_stream_chunk(&trailing, &mut state)?;
            for c in chunks {
                yield c;
            }
        }
    };
    Box::pin(stream)
}

/// Extract one complete SSE event from `buffer` if present, returning the
/// raw event payload (everything between the start and the `\n\n`
/// terminator). Returns `None` if no complete event is buffered yet.
fn extract_sse_event(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    // SSE event boundary is `\n\n` (or `\r\n\r\n`).
    let pos = buffer
        .windows(2)
        .position(|w| w == b"\n\n")
        .or_else(|| buffer.windows(4).position(|w| w == b"\r\n\r\n"));
    let end = pos?;
    let boundary = if buffer[end..].starts_with(b"\r\n\r\n") {
        4
    } else {
        2
    };
    let event = buffer.drain(..end + boundary).collect::<Vec<u8>>();
    // Strip the boundary from the returned payload.
    let payload_len = event.len() - boundary;
    Some(event[..payload_len].to_vec())
}

/// Extract one element from a JSON-array stream
/// (`[{...},{...},...]`). Skips structural commas and brackets.
fn extract_json_array_element(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    // Skip leading whitespace, `[`, `,`.
    let mut start = 0;
    while start < buffer.len() {
        let b = buffer[start];
        if b.is_ascii_whitespace() || b == b'[' || b == b',' {
            start += 1;
        } else {
            break;
        }
    }
    if start >= buffer.len() {
        if start > 0 {
            buffer.drain(..start);
        }
        return None;
    }
    if buffer[start] == b']' {
        // End of stream — drain and signal nothing.
        buffer.drain(..start + 1);
        return None;
    }
    if buffer[start] != b'{' {
        // Malformed — drop one byte and retry.
        buffer.drain(..start + 1);
        return None;
    }
    // Brace-balance scan for one complete JSON object.
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    let mut end = start;
    while end < buffer.len() {
        let b = buffer[end];
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else if b == b'"' {
            in_string = true;
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                end += 1;
                let element = buffer[start..end].to_vec();
                buffer.drain(..end);
                return Some(element);
            }
        }
        end += 1;
    }
    None
}

/// Extract one newline-delimited JSON line from `buffer`.
fn extract_ndjson_line(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let pos = buffer.iter().position(|&b| b == b'\n')?;
    let mut line = buffer.drain(..pos + 1).collect::<Vec<u8>>();
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Some(line)
}

/// Translate a [`crate::client::schema::StructuredOutputValidationError`]
/// into the public [`Error::StructuredOutputInvalid`] variant so callers
/// get a stable, typed failure surface with the violating JSON pointer
/// preserved.
fn validation_error_to_error(err: crate::client::schema::StructuredOutputValidationError) -> Error {
    use crate::client::schema::StructuredOutputValidationError as V;
    match err {
        V::NotJson { reason } => Error::StructuredOutputInvalid {
            pointer: String::new(),
            reason: format!("body is not valid JSON: {reason}"),
        },
        V::Constraint { pointer, reason } => Error::StructuredOutputInvalid { pointer, reason },
    }
}

fn validate_composition(codec: &dyn ModelCodec, transport: &dyn ModelTransport) -> Result<()> {
    if let Some(pinned) = codec.pinned_transport()
        && pinned != transport.id()
    {
        return Err(Error::InvalidComposition {
            codec: codec.id(),
            transport: transport.id(),
            reason: "codec is pinned to a different transport",
        });
    }
    if !transport.supports_codec(codec.id()) {
        return Err(Error::InvalidComposition {
            codec: codec.id(),
            transport: transport.id(),
            reason: "transport does not support this codec",
        });
    }
    if !codec.supports_mode(InvocationMode::Unary) {
        return Err(Error::InvalidComposition {
            codec: codec.id(),
            transport: transport.id(),
            reason: "codec does not support unary invocation",
        });
    }
    Ok(())
}

/// Convert a non-2xx HTTP response into a typed [`Error::Provider`] by
/// asking the transport to classify the failure. Each transport owns its
/// own vendor-specific patterns (Vertex quota project, Bedrock throttling,
/// …), so adding a new transport never requires editing this function.
fn classify_response_error(transport: &dyn ModelTransport, status: u16, body: &str) -> Error {
    use crate::error::ProviderErrorKind;
    let snippet = body.chars().take(500).collect::<String>();
    let (kind, hint) = transport.classify_error(status, body);
    Error::Provider {
        provider: transport.id(),
        kind,
        message: snippet,
        hint,
        retryable: matches!(
            kind,
            ProviderErrorKind::RateLimit | ProviderErrorKind::Server | ProviderErrorKind::Network
        ),
        status: Some(status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::codec::{
        AnthropicMessagesCodec, EncodedRequest, EndpointShape, GeminiGenerateCodec, ModelCodec,
    };
    use crate::client::transport::{DirectAuth, DirectTransport};
    use crate::ir::{
        Message, ModelRequest, ModelStreamChunk, ProviderCapabilities, StreamDecodeState,
    };
    use async_trait::async_trait;
    use secrecy::SecretString;

    /// A codec pinned to transport id "fake" — used to test pin enforcement.
    #[derive(Debug)]
    struct PinnedCodec;
    impl ModelCodec for PinnedCodec {
        fn id(&self) -> &'static str {
            "pinned"
        }
        fn capabilities(&self) -> &'static ProviderCapabilities {
            const C: ProviderCapabilities = ProviderCapabilities::unsupported("pinned");
            &C
        }
        fn endpoint_shape(&self) -> &'static EndpointShape {
            const S: EndpointShape = EndpointShape {
                codec_id: "pinned",
                path_template: "v1/x",
                verb_unary: "",
                verb_stream: "",
                stream_query: &[],
                required_headers: &[],
                api_version_hint: crate::client::codec::ApiVersionHint::Stable,
            };
            &S
        }
        fn pinned_transport(&self) -> Option<&'static str> {
            Some("nonexistent")
        }
        fn encode_request(&self, _r: &ModelRequest, _m: InvocationMode) -> Result<EncodedRequest> {
            Ok(EncodedRequest::new(serde_json::json!({})))
        }
        fn decode_response(
            &self,
            _r: serde_json::Value,
            _m: InvocationMode,
        ) -> Result<ModelResponse> {
            unimplemented!()
        }
        fn decode_stream_chunk(
            &self,
            _f: &[u8],
            _s: &mut StreamDecodeState,
        ) -> Result<Vec<ModelStreamChunk>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl ModelTransport for FakeTransport {
        fn id(&self) -> &'static str {
            "fake"
        }
        fn supports_codec(&self, codec_id: &str) -> bool {
            codec_id != "rejected"
        }
        async fn resolve_endpoint(
            &self,
            _shape: &EndpointShape,
            _model: &str,
            _mode: InvocationMode,
        ) -> Result<crate::client::transport::Endpoint> {
            Ok(crate::client::transport::Endpoint::new("https://x"))
        }
        async fn authorize(
            &self,
            req: reqwest::RequestBuilder,
            _body_bytes: &[u8],
        ) -> Result<reqwest::RequestBuilder> {
            Ok(req)
        }
    }

    #[derive(Debug)]
    struct FakeTransport;

    #[test]
    fn pinned_codec_rejects_wrong_transport() {
        let result = ProviderClient::new(Arc::new(PinnedCodec), Arc::new(FakeTransport));
        match result {
            Err(Error::InvalidComposition {
                codec, transport, ..
            }) => {
                assert_eq!(codec, "pinned");
                assert_eq!(transport, "fake");
            }
            other => panic!("expected InvalidComposition, got {other:?}"),
        }
    }

    #[test]
    fn anthropic_messages_on_direct_transport_composes() {
        let codec = Arc::new(AnthropicMessagesCodec::new());
        let transport = Arc::new(DirectTransport::new(
            "https://api.anthropic.com",
            DirectAuth::XApiKey(SecretString::from("sk-test")),
        ));
        let client = ProviderClient::new(codec, transport).unwrap();
        assert_eq!(client.codec_id(), "anthropic-messages");
        assert_eq!(client.transport_id(), "direct");
    }

    #[test]
    fn gemini_on_direct_composes() {
        let codec = Arc::new(GeminiGenerateCodec::new());
        let transport = Arc::new(DirectTransport::new(
            "https://generativelanguage.googleapis.com",
            DirectAuth::QueryParam {
                param: "key",
                value: SecretString::from("k"),
            },
        ));
        let _ = ProviderClient::new(codec, transport).unwrap();
    }

    #[test]
    fn unsupported_codec_rejected() {
        // Use a DirectTransport restricted to anthropic-messages, then try
        // to compose it with the gemini codec.
        let codec = Arc::new(GeminiGenerateCodec::new());
        let transport = Arc::new(
            DirectTransport::new(
                "https://api.anthropic.com",
                DirectAuth::XApiKey(SecretString::from("k")),
            )
            .with_allowed_codecs(&["anthropic-messages"]),
        );
        let result = ProviderClient::new(codec, transport);
        assert!(matches!(result, Err(Error::InvalidComposition { .. })));
    }

    #[tokio::test]
    async fn live_call_against_mock_anthropic() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/messages"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "msg_42",
                    "model": "claude-sonnet-4-5",
                    "content": [{"type": "text", "text": "pong"}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 5, "output_tokens": 1}
                })),
            )
            .mount(&mock)
            .await;

        let codec = Arc::new(AnthropicMessagesCodec::new());
        let transport = Arc::new(DirectTransport::new(
            mock.uri(),
            DirectAuth::XApiKey(SecretString::from("sk-test")),
        ));
        let client = ProviderClient::new(codec, transport).unwrap();
        let req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("ping")]);
        let resp = client.send(&req).await.unwrap();
        assert_eq!(resp.id, "msg_42");
        assert_eq!(resp.text(), "pong");
        assert_eq!(resp.usage.input_tokens, 5);
    }

    #[tokio::test]
    async fn live_call_against_mock_gemini() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(
                "/v1beta/models/gemini-2.5-flash:generateContent",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "responseId": "r_1",
                    "modelVersion": "gemini-2.5-flash",
                    "candidates": [{
                        "content": {"parts": [{"text": "pong"}]},
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 1}
                })),
            )
            .mount(&mock)
            .await;

        let codec = Arc::new(GeminiGenerateCodec::new());
        let transport = Arc::new(DirectTransport::new(
            mock.uri(),
            DirectAuth::QueryParam {
                param: "key",
                value: SecretString::from("test-key"),
            },
        ));
        let client = ProviderClient::new(codec, transport).unwrap();
        let req = ModelRequest::new("gemini-2.5-flash", vec![Message::user("ping")]);
        let resp = client.send(&req).await.unwrap();
        assert_eq!(resp.text(), "pong");
        assert_eq!(resp.usage.input_tokens, 5);
    }

    #[tokio::test]
    async fn http_error_classified_with_hint_for_vertex_403_quota() {
        // The classify_error trait method is what makes the OCP-clean
        // distributed error classification work. This test stands in for a
        // real VertexTransport (which would need ADC) and reproduces the
        // vendor-specific quota-project detection logic.
        #[derive(Debug)]
        struct FakeVertex(String);
        #[async_trait]
        impl ModelTransport for FakeVertex {
            fn id(&self) -> &'static str {
                "vertex"
            }
            fn supports_codec(&self, _id: &str) -> bool {
                true
            }
            async fn resolve_endpoint(
                &self,
                _shape: &EndpointShape,
                _model: &str,
                _mode: InvocationMode,
            ) -> Result<crate::client::transport::Endpoint> {
                Ok(crate::client::transport::Endpoint::new(self.0.clone()))
            }
            async fn authorize(
                &self,
                req: reqwest::RequestBuilder,
                _body_bytes: &[u8],
            ) -> Result<reqwest::RequestBuilder> {
                Ok(req)
            }
            fn classify_error(
                &self,
                status: u16,
                body: &str,
            ) -> (crate::error::ProviderErrorKind, Option<&'static str>) {
                use crate::error::ProviderErrorKind;
                match status {
                    401 | 403 if body.contains("quota") || body.contains("user-project") => (
                        ProviderErrorKind::Quota,
                        Some(
                            "Set GOOGLE_CLOUD_QUOTA_PROJECT or pass VertexTransport::with_quota_project(...). \
                             If using gcloud creds, run: gcloud auth application-default set-quota-project <project>",
                        ),
                    ),
                    _ => crate::client::transport::default_classify_status(status),
                }
            }
        }

        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(403).set_body_string(
                r#"{"error":{"code":403,"message":"quota project not set; pass x-goog-user-project header","status":"PERMISSION_DENIED"}}"#,
            ))
            .mount(&mock)
            .await;

        let codec = Arc::new(GeminiGenerateCodec::new());
        let transport = Arc::new(FakeVertex(mock.uri()));
        let client = ProviderClient::new(codec, transport).unwrap();
        let req = ModelRequest::new("gemini-2.5-flash", vec![Message::user("hi")]);
        let err = client.send(&req).await.unwrap_err();
        match err {
            Error::Provider {
                kind: crate::error::ProviderErrorKind::Quota,
                hint: Some(h),
                status: Some(403),
                ..
            } => {
                assert!(h.contains("GOOGLE_CLOUD_QUOTA_PROJECT") || h.contains("quota-project"));
            }
            other => panic!("expected Provider Quota error with hint, got {other:?}"),
        }
    }

    // =============================================================================
    // Streaming
    // =============================================================================

    use super::{extract_json_array_element, extract_ndjson_line, extract_sse_event};

    #[test]
    fn extract_sse_event_finds_complete_frame() {
        let mut buf = b"data: hello\n\nleftover".to_vec();
        let frame = extract_sse_event(&mut buf).unwrap();
        assert_eq!(frame, b"data: hello");
        assert_eq!(buf, b"leftover");
    }

    #[test]
    fn extract_sse_event_returns_none_when_incomplete() {
        let mut buf = b"data: half".to_vec();
        assert!(extract_sse_event(&mut buf).is_none());
        assert_eq!(buf, b"data: half");
    }

    #[test]
    fn extract_sse_event_supports_crlf() {
        let mut buf = b"data: x\r\n\r\nrest".to_vec();
        let frame = extract_sse_event(&mut buf).unwrap();
        assert_eq!(frame, b"data: x");
        assert_eq!(buf, b"rest");
    }

    #[test]
    fn extract_json_array_element_pulls_one_object() {
        let mut buf = br#"[{"a":1},{"b":{"c":2}},"#.to_vec();
        let f1 = extract_json_array_element(&mut buf).unwrap();
        assert_eq!(f1, br#"{"a":1}"#);
        let f2 = extract_json_array_element(&mut buf).unwrap();
        assert_eq!(f2, br#"{"b":{"c":2}}"#);
    }

    #[test]
    fn extract_json_array_element_handles_nested_strings_and_braces() {
        let mut buf = br#"[{"text":"has } and \" quote"}]"#.to_vec();
        let f = extract_json_array_element(&mut buf).unwrap();
        assert_eq!(f, br#"{"text":"has } and \" quote"}"#);
    }

    #[test]
    fn extract_ndjson_line_pulls_lines() {
        let mut buf = b"line1\nline2\n".to_vec();
        assert_eq!(extract_ndjson_line(&mut buf).unwrap(), b"line1");
        assert_eq!(extract_ndjson_line(&mut buf).unwrap(), b"line2");
        assert!(extract_ndjson_line(&mut buf).is_none());
    }

    /// SSE / NDJSON / JsonArray framing operates on raw bytes; the
    /// extracted frame is then handed to a JSON parser. The framing layer
    /// only needs to find the boundary bytes (`\n\n`, `\n`, `}`) — it
    /// must not corrupt the multi-byte UTF-8 sequences inside. These
    /// tests guard the byte-buffer assembly path against the most likely
    /// regression: a multi-byte character split across chunk boundaries.

    #[test]
    fn extract_sse_event_preserves_multibyte_korean() {
        // "안녕하세요" is 15 bytes in UTF-8 (5 chars × 3 bytes).
        let mut buf = "data: 안녕하세요\n\nrest".as_bytes().to_vec();
        let frame = extract_sse_event(&mut buf).unwrap();
        assert_eq!(std::str::from_utf8(&frame).unwrap(), "data: 안녕하세요");
        assert_eq!(buf, b"rest");
    }

    #[test]
    fn extract_sse_event_preserves_multibyte_emoji() {
        // 🎉 is 4 bytes in UTF-8.
        let mut buf = "data: 🎉🎉\n\n".as_bytes().to_vec();
        let frame = extract_sse_event(&mut buf).unwrap();
        assert_eq!(std::str::from_utf8(&frame).unwrap(), "data: 🎉🎉");
    }

    #[test]
    fn extract_ndjson_line_preserves_multibyte_chars() {
        let mut buf = "한국어\n日本語\n".as_bytes().to_vec();
        let l1 = extract_ndjson_line(&mut buf).unwrap();
        assert_eq!(std::str::from_utf8(&l1).unwrap(), "한국어");
        let l2 = extract_ndjson_line(&mut buf).unwrap();
        assert_eq!(std::str::from_utf8(&l2).unwrap(), "日本語");
    }

    #[test]
    fn extract_json_array_element_preserves_multibyte_in_string_value() {
        let mut buf = r#"[{"text":"안녕 🎉"}]"#.as_bytes().to_vec();
        let f = extract_json_array_element(&mut buf).unwrap();
        assert_eq!(std::str::from_utf8(&f).unwrap(), r#"{"text":"안녕 🎉"}"#);
    }

    #[test]
    fn extract_sse_event_handles_partial_frame_then_completion() {
        // First push: incomplete (boundary not yet seen).
        let mut buf = "data: 안녕".as_bytes().to_vec();
        assert!(extract_sse_event(&mut buf).is_none());
        // Buffer is unchanged — multi-byte char preserved at the end.
        assert_eq!(std::str::from_utf8(&buf).unwrap(), "data: 안녕");
        // Subsequent push completes the frame.
        buf.extend_from_slice("하세요\n\n".as_bytes());
        let frame = extract_sse_event(&mut buf).unwrap();
        assert_eq!(std::str::from_utf8(&frame).unwrap(), "data: 안녕하세요");
    }

    #[tokio::test]
    async fn stream_anthropic_text_via_mock_server() {
        // wiremock supports raw body but not chunked SSE replay; we
        // simulate a single response body with two SSE events back-to-back.
        let body = "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-sonnet-4-5\"}}\n\n\
                    data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi \"}}\n\n\
                    data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"there\"}}\n\n\
                    data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":10,\"output_tokens\":2}}\n\n\
                    data: {\"type\":\"message_stop\"}\n\n";
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/messages"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&mock)
            .await;

        let codec = Arc::new(AnthropicMessagesCodec::new());
        let transport = Arc::new(DirectTransport::new(
            mock.uri(),
            DirectAuth::XApiKey(SecretString::from("sk-test")),
        ));
        let client = ProviderClient::new(codec, transport).unwrap();
        let req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("ping")]);
        let mut stream = client
            .send_stream(&req, tokio_util::sync::CancellationToken::new())
            .await
            .unwrap();

        let mut text = String::new();
        let mut saw_message_start = false;
        let mut saw_finish = false;
        while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
            match chunk.unwrap() {
                ModelStreamChunk::MessageStart { id, .. } => {
                    saw_message_start = true;
                    assert_eq!(id, "msg_1");
                }
                ModelStreamChunk::TextDelta { text: t, .. } => text.push_str(&t),
                ModelStreamChunk::Finish { .. } => saw_finish = true,
                _ => {}
            }
        }
        assert!(saw_message_start);
        assert_eq!(text, "hi there");
        assert!(saw_finish);
    }

    #[tokio::test]
    async fn stream_openai_chat_via_mock_server() {
        let body = "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"He\"}}]}\n\n\
                    data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"llo\"}}]}\n\n\
                    data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                    data: [DONE]\n\n";
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&mock)
            .await;

        let codec = Arc::new(crate::client::codec::OpenAiChatCodec::new());
        let transport = Arc::new(DirectTransport::new(
            mock.uri(),
            DirectAuth::Bearer(SecretString::from("k")),
        ));
        let client = ProviderClient::new(codec, transport).unwrap();
        let req = ModelRequest::new("gpt-4o-mini", vec![Message::user("hi")]);
        let mut stream = client
            .send_stream(&req, tokio_util::sync::CancellationToken::new())
            .await
            .unwrap();

        let mut text = String::new();
        let mut saw_finish = false;
        while let Some(chunk) = futures::StreamExt::next(&mut stream).await {
            match chunk.unwrap() {
                ModelStreamChunk::TextDelta { text: t, .. } => text.push_str(&t),
                ModelStreamChunk::Finish {
                    reason: crate::ir::FinishReason::Stop,
                    ..
                } => saw_finish = true,
                _ => {}
            }
        }
        assert_eq!(text, "Hello");
        assert!(saw_finish);
    }

    #[tokio::test]
    async fn stream_warnings_surface_before_decoded_chunks() {
        // Send a request with an unsupported setting (top_k on Anthropic)
        // and assert the warning lands on the stream before any text.
        let body = "data: {\"type\":\"message_stop\"}\n\n";
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&mock)
            .await;

        let codec = Arc::new(AnthropicMessagesCodec::new());
        let transport = Arc::new(DirectTransport::new(
            mock.uri(),
            DirectAuth::XApiKey(SecretString::from("k")),
        ));
        let client = ProviderClient::new(codec, transport).unwrap();
        let mut req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("hi")]);
        req.settings.seed = Some(42); // unsupported on anthropic-messages
        let mut stream = client
            .send_stream(&req, tokio_util::sync::CancellationToken::new())
            .await
            .unwrap();

        let first = futures::StreamExt::next(&mut stream)
            .await
            .unwrap()
            .unwrap();
        match first {
            ModelStreamChunk::Warning(crate::ir::ModelWarning::UnsupportedSetting {
                setting,
                ..
            }) => {
                assert_eq!(setting, "seed");
            }
            other => panic!("expected leading Warning chunk, got {other:?}"),
        }
    }

    /// `ModelRequest::has_cache_markers()` returns true when any
    /// provider-specific cache marker is set: Anthropic
    /// `cache_control`, Gemini `cached_content`, per-block
    /// `cache_marker`, or structural `Boundary` role.
    #[test]
    fn has_cache_markers_anthropic_top_level() {
        use crate::ir::provider_options::{AnthropicOptions, CacheControl};

        let mut req = ModelRequest::new("m", vec![Message::user("hi")]);
        assert!(!req.has_cache_markers());

        req.provider_options.anthropic = Some(AnthropicOptions {
            cache_control: Some(CacheControl {
                system: true,
                ..Default::default()
            }),
            ..Default::default()
        });
        assert!(req.has_cache_markers());
    }

    #[test]
    fn has_cache_markers_per_block() {
        use crate::ir::model::{SystemBlock, SystemPrompt};
        use crate::ir::provider_options::CacheMarker;

        let mut req = ModelRequest::new("m", vec![Message::user("hi")]);
        req.system = Some(SystemPrompt::Blocks(vec![SystemBlock {
            text: "sys".into(),
            role: crate::ir::SystemBlockRole::Static,
            cache_marker: Some(CacheMarker::ephemeral()),
        }]));
        assert!(req.has_cache_markers());
    }

    #[test]
    fn has_cache_markers_plain_text_system_is_not_a_marker() {
        use crate::ir::SystemPrompt;
        let mut req = ModelRequest::new("m", vec![Message::user("hi")]);
        req.system = Some(SystemPrompt::Text("plain".into()));
        assert!(!req.has_cache_markers());
    }

    #[test]
    fn has_cache_markers_gemini_cached_content() {
        use crate::ir::provider_options::GeminiOptions;
        let mut req = ModelRequest::new("m", vec![Message::user("hi")]);
        req.provider_options.gemini = Some(GeminiOptions {
            cached_content: Some("cachedContents/abc".into()),
            ..Default::default()
        });
        assert!(req.has_cache_markers());
    }

    #[test]
    fn has_cache_markers_structural_boundary() {
        use crate::ir::{SystemBlock, SystemPrompt};
        let mut req = ModelRequest::new("m", vec![Message::user("hi")]);
        req.system = Some(SystemPrompt::Blocks(vec![
            SystemBlock::uncached("static"),
            SystemBlock::boundary(),
            SystemBlock::dynamic("dynamic"),
        ]));
        assert!(req.has_cache_markers());
    }

    /// End-to-end: request declares a cache marker, provider returns
    /// zero cached_input_tokens, `ProviderClient::send` must attach
    /// a `cache.unexpected_break` warning to the response.
    #[tokio::test]
    async fn unexpected_cache_break_surfaces_warning() {
        use crate::ir::provider_options::{AnthropicOptions, CacheControl};
        use crate::ir::{ModelWarning, ProviderOptions};
        use serde_json::json;

        // Anthropic response with zero cache hits.
        let body = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 10,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0
            }
        });
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(body),
            )
            .mount(&mock)
            .await;

        let codec = Arc::new(AnthropicMessagesCodec::new());
        let transport = Arc::new(DirectTransport::new(
            mock.uri(),
            DirectAuth::XApiKey(SecretString::from("k")),
        ));
        let client = ProviderClient::new(codec, transport).unwrap();

        let mut req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("hi")]);
        req.provider_options = ProviderOptions {
            anthropic: Some(AnthropicOptions {
                cache_control: Some(CacheControl {
                    system: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resp = client.send(&req).await.unwrap();
        let saw_warning = resp.warnings.iter().any(|w| {
            matches!(
                w,
                ModelWarning::LossyEncode { field, .. } if field == "cache.unexpected_break"
            )
        });
        assert!(
            saw_warning,
            "expected cache.unexpected_break warning, got {:?}",
            resp.warnings
        );
    }

    /// Post-decode runtime validation: when a strict
    /// `ResponseFormat::JsonSchema` is set and the provider emits a
    /// non-conforming body, `ProviderClient::send` must fail with
    /// [`Error::StructuredOutputInvalid`] before the response ever
    /// reaches application code.
    #[tokio::test]
    async fn strict_json_schema_violation_errors_post_decode() {
        use crate::ir::{JsonSchemaSpec, ResponseFormat};
        use serde_json::json;

        // Anthropic-shaped response whose text content is valid JSON
        // but does NOT match the declared schema (age is a string).
        let body = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": r#"{"name":"Ada","age":"thirty"}"#}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 10}
        });
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(body),
            )
            .mount(&mock)
            .await;

        let codec = Arc::new(AnthropicMessagesCodec::new());
        let transport = Arc::new(DirectTransport::new(
            mock.uri(),
            DirectAuth::XApiKey(SecretString::from("k")),
        ));
        let client = ProviderClient::new(codec, transport).unwrap();
        let req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("hi")])
            .with_response_format(ResponseFormat::JsonSchema(JsonSchemaSpec {
                schema: json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "age": {"type": "integer"}
                    },
                    "required": ["name", "age"]
                }),
                name: None,
                description: None,
                strict: true,
            }));

        let err = client.send(&req).await.unwrap_err();
        match err {
            Error::StructuredOutputInvalid { pointer, reason } => {
                assert_eq!(pointer, "/age");
                assert!(reason.contains("expected type"), "got {reason}");
            }
            other => panic!("expected StructuredOutputInvalid, got {other:?}"),
        }
    }

    /// Non-strict mode attaches a warning instead of failing.
    #[tokio::test]
    async fn nonstrict_json_schema_violation_warns_not_errors() {
        use crate::ir::{JsonSchemaSpec, ModelWarning, ResponseFormat};
        use serde_json::json;

        let body = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": r#"{"name":"Ada","age":"thirty"}"#}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 10}
        });
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(body),
            )
            .mount(&mock)
            .await;

        let codec = Arc::new(AnthropicMessagesCodec::new());
        let transport = Arc::new(DirectTransport::new(
            mock.uri(),
            DirectAuth::XApiKey(SecretString::from("k")),
        ));
        let client = ProviderClient::new(codec, transport).unwrap();
        let req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("hi")])
            .with_response_format(ResponseFormat::JsonSchema(JsonSchemaSpec {
                schema: json!({
                    "type": "object",
                    "properties": {"age": {"type": "integer"}},
                }),
                name: None,
                description: None,
                strict: false,
            }));

        let resp = client
            .send(&req)
            .await
            .expect("should not error in non-strict mode");
        let has_warning = resp.warnings.iter().any(|w| matches!(
            w,
            ModelWarning::LossyEncode { field, .. } if field == "response_format.runtime_validation"
        ));
        assert!(
            has_warning,
            "expected runtime_validation warning, got {:?}",
            resp.warnings
        );
    }

    /// `build_chunk_stream` must abort the byte-stream loop when
    /// `cancel_token` fires. A byte stream that hangs forever should
    /// still yield a terminal `Error::Stream` within a short bound after
    /// cancel is triggered — not wait for the stream to drain.
    #[tokio::test]
    async fn build_chunk_stream_cancels_mid_read() {
        use futures::stream::{self, StreamExt};
        use std::time::Duration;

        // A byte stream that never yields another chunk.
        let hanging = stream::unfold((), |()| async move {
            // Park forever — the outer select! is what must abort us.
            futures::future::pending::<()>().await;
            Some((Ok::<bytes::Bytes, reqwest::Error>(bytes::Bytes::new()), ()))
        });

        let codec: Arc<dyn ModelCodec> = Arc::new(AnthropicMessagesCodec::new());
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut stream = build_chunk_stream(
            codec,
            StreamFraming::Sse,
            hanging,
            Vec::new(),
            cancel.clone(),
        );

        // Fire cancel after a tiny delay so the stream has entered the
        // select loop.
        let cancel_cloned = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel_cloned.cancel();
        });

        let next = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("stream should yield a terminal item within 2s of cancel");
        match next {
            Some(Err(Error::Stream(msg))) => {
                assert!(
                    msg.contains("cancelled"),
                    "expected cancellation message, got {msg:?}"
                );
            }
            other => panic!("expected Stream cancellation error, got {other:?}"),
        }
    }
}
