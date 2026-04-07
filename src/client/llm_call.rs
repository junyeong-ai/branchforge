//! `LlmCall` — the consumer-facing trait for sending model requests.
//!
//! This is the single entry point that the agent runtime calls for both
//! unary and streaming model invocations. Implementations include:
//!
//! - [`ProviderClient`](super::provider_client::ProviderClient) — bare codec + transport.
//! - [`RetryingClient`] — exponential-backoff retry around any `LlmCall`.
//! - [`FallingBackClient`] — try a primary `LlmCall`, then a fallback.
//! - [`CircuitBrokenClient`] — circuit-breaker pattern around any `LlmCall`.

use async_trait::async_trait;
use std::sync::Arc;

use super::RetryPolicy;
use super::provider_client::ChunkStream;
use crate::Result;
use crate::ir::{ModelRequest, ModelResponse};

/// The consumer surface for model invocations.
///
/// Agent runtime, compaction service, and any other component that needs
/// to call an LLM should take `Arc<dyn LlmCall>`.
#[async_trait]
pub trait LlmCall: Send + Sync + std::fmt::Debug {
    /// Unary request → response.
    async fn send(&self, request: &ModelRequest) -> Result<ModelResponse>;

    /// Streaming request → chunk stream.
    async fn send_stream(&self, request: &ModelRequest) -> Result<ChunkStream>;
}

// ---------------------------------------------------------------------------
// ProviderClient → LlmCall
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmCall for super::provider_client::ProviderClient {
    async fn send(&self, request: &ModelRequest) -> Result<ModelResponse> {
        self.send(request).await
    }

    async fn send_stream(&self, request: &ModelRequest) -> Result<ChunkStream> {
        self.send_stream(request).await
    }
}

// ---------------------------------------------------------------------------
// RetryingClient
// ---------------------------------------------------------------------------

/// Exponential-backoff retry around any `LlmCall`.
#[derive(Debug)]
pub struct RetryingClient {
    inner: Arc<dyn LlmCall>,
    policy: RetryPolicy,
}

impl RetryingClient {
    pub fn new(inner: Arc<dyn LlmCall>, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }

    pub fn wrap(inner: Arc<dyn LlmCall>) -> Self {
        Self::new(inner, RetryPolicy::default())
    }
}

#[async_trait]
impl LlmCall for RetryingClient {
    async fn send(&self, request: &ModelRequest) -> Result<ModelResponse> {
        let mut last_err = None;
        for attempt in 0..=self.policy.max_retries {
            match self.inner.send(request).await {
                Ok(resp) => return Ok(resp),
                Err(e) if e.is_retryable() && attempt < self.policy.max_retries => {
                    let delay = self.policy.delay_for(attempt + 1, e.retry_after());
                    tracing::warn!(
                        error = %e,
                        attempt = attempt + 1,
                        max_retries = self.policy.max_retries,
                        delay_ms = delay.as_millis() as u64,
                        "Retrying after transient error"
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.expect("retry loop ended without error"))
    }

    async fn send_stream(&self, request: &ModelRequest) -> Result<ChunkStream> {
        let mut last_err = None;
        for attempt in 0..=self.policy.max_retries {
            match self.inner.send_stream(request).await {
                Ok(stream) => return Ok(stream),
                Err(e) if e.is_retryable() && attempt < self.policy.max_retries => {
                    let delay = self.policy.delay_for(attempt + 1, e.retry_after());
                    tracing::warn!(
                        error = %e,
                        attempt = attempt + 1,
                        max_retries = self.policy.max_retries,
                        delay_ms = delay.as_millis() as u64,
                        "Retrying stream connection after transient error"
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.expect("retry loop ended without error"))
    }
}

// ---------------------------------------------------------------------------
// FallingBackClient
// ---------------------------------------------------------------------------

/// Try a primary `LlmCall`, fall back to an alternative on certain errors.
#[derive(Debug)]
pub struct FallingBackClient {
    primary: Arc<dyn LlmCall>,
    fallback: Arc<dyn LlmCall>,
    /// Model to substitute in the request when falling back.
    fallback_model: String,
    triggers: Vec<super::FallbackTrigger>,
}

impl FallingBackClient {
    pub fn new(
        primary: Arc<dyn LlmCall>,
        fallback: Arc<dyn LlmCall>,
        fallback_model: impl Into<String>,
        triggers: Vec<super::FallbackTrigger>,
    ) -> Self {
        Self {
            primary,
            fallback,
            fallback_model: fallback_model.into(),
            triggers,
        }
    }

    fn should_fallback(&self, err: &crate::Error) -> bool {
        self.triggers.iter().any(|t| t.matches(err))
    }
}

#[async_trait]
impl LlmCall for FallingBackClient {
    async fn send(&self, request: &ModelRequest) -> Result<ModelResponse> {
        match self.primary.send(request).await {
            Ok(resp) => Ok(resp),
            Err(e) if self.should_fallback(&e) => {
                tracing::warn!(
                    error = %e,
                    fallback_model = %self.fallback_model,
                    "Primary failed, falling back"
                );
                let mut fb_request = request.clone();
                fb_request.model = self.fallback_model.clone();
                self.fallback.send(&fb_request).await
            }
            Err(e) => Err(e),
        }
    }

    async fn send_stream(&self, request: &ModelRequest) -> Result<ChunkStream> {
        match self.primary.send_stream(request).await {
            Ok(stream) => Ok(stream),
            Err(e) if self.should_fallback(&e) => {
                tracing::warn!(
                    error = %e,
                    fallback_model = %self.fallback_model,
                    "Primary stream failed, falling back"
                );
                let mut fb_request = request.clone();
                fb_request.model = self.fallback_model.clone();
                self.fallback.send_stream(&fb_request).await
            }
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// CircuitBrokenClient
// ---------------------------------------------------------------------------

/// Circuit-breaker pattern around any `LlmCall`.
pub struct CircuitBrokenClient {
    inner: Arc<dyn LlmCall>,
    breaker: Arc<crate::common::circuit::CircuitBreaker>,
}

impl std::fmt::Debug for CircuitBrokenClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircuitBrokenClient")
            .field("inner", &self.inner)
            .finish()
    }
}

impl CircuitBrokenClient {
    pub fn new(inner: Arc<dyn LlmCall>, config: crate::common::circuit::CircuitConfig) -> Self {
        Self {
            inner,
            breaker: Arc::new(crate::common::circuit::CircuitBreaker::new(config)),
        }
    }
}

#[async_trait]
impl LlmCall for CircuitBrokenClient {
    async fn send(&self, request: &ModelRequest) -> Result<ModelResponse> {
        if !self.breaker.allow_request() {
            return Err(crate::Error::Config(
                "Circuit breaker is open — too many recent failures".into(),
            ));
        }
        match self.inner.send(request).await {
            Ok(resp) => {
                self.breaker.record_success();
                Ok(resp)
            }
            Err(e) => {
                self.breaker.record_failure();
                Err(e)
            }
        }
    }

    async fn send_stream(&self, request: &ModelRequest) -> Result<ChunkStream> {
        if !self.breaker.allow_request() {
            return Err(crate::Error::Config(
                "Circuit breaker is open — too many recent failures".into(),
            ));
        }
        match self.inner.send_stream(request).await {
            Ok(stream) => {
                self.breaker.record_success();
                Ok(stream)
            }
            Err(e) => {
                self.breaker.record_failure();
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience: LegacyBridgeClient
// ---------------------------------------------------------------------------

/// Bridges the old `Client` to the `LlmCall` trait so existing code can
/// incrementally migrate. Converts `ModelRequest → CreateMessageRequest`,
/// dispatches through `Client::send_with_auth_retry`, and converts back.
///
/// Deleted once all consumers use `LlmCall` directly.
#[derive(Debug)]
pub struct LegacyBridgeClient {
    client: Arc<super::Client>,
}

impl LegacyBridgeClient {
    pub fn new(client: Arc<super::Client>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl LlmCall for LegacyBridgeClient {
    async fn send(&self, request: &ModelRequest) -> Result<ModelResponse> {
        // Convert IR request → legacy CreateMessageRequest via compat
        let legacy_req = legacy_request_from_ir(request);
        let legacy_resp = self.client.send_with_auth_retry(legacy_req).await?;
        // Convert legacy ApiResponse → IR ModelResponse
        Ok(ir_response_from_legacy(legacy_resp, request))
    }

    async fn send_stream(&self, _request: &ModelRequest) -> Result<ChunkStream> {
        // Streaming through legacy bridge is not supported — the agent
        // streaming path will be migrated to use ProviderClient directly.
        Err(crate::Error::Config(
            "Streaming not supported through LegacyBridgeClient".into(),
        ))
    }
}

fn legacy_request_from_ir(req: &ModelRequest) -> super::CreateMessageRequest {
    use crate::ir::compat::ir_message_to_legacy;

    let messages: Vec<crate::types::Message> =
        req.messages.iter().map(ir_message_to_legacy).collect();

    let mut legacy = super::CreateMessageRequest::new(&req.model, messages)
        .max_tokens(req.settings.max_output_tokens.unwrap_or(8192));

    if let Some(ref system) = req.system {
        let legacy_system = match system {
            crate::ir::SystemPrompt::Text(s) => crate::types::SystemPrompt::Text(s.clone()),
            crate::ir::SystemPrompt::Blocks(blocks) => crate::types::SystemPrompt::Blocks(
                blocks
                    .iter()
                    .map(|b| crate::types::SystemBlock::uncached(&b.text))
                    .collect(),
            ),
        };
        legacy = legacy.system(legacy_system);
    }

    if let Some(temp) = req.settings.temperature {
        legacy.temperature = Some(temp);
    }

    legacy
}

fn ir_response_from_legacy(
    resp: crate::types::ApiResponse,
    _original_request: &ModelRequest,
) -> ModelResponse {
    let content: Vec<crate::ir::ContentPart> = resp
        .content
        .iter()
        .map(crate::ir::compat::legacy_block_to_ir)
        .collect();

    let finish_reason = resp
        .stop_reason
        .map(crate::ir::FinishReason::from)
        .unwrap_or(crate::ir::FinishReason::Stop);

    let usage = crate::ir::Usage::from(&resp.usage);

    ModelResponse {
        id: resp.id,
        model: resp.model,
        content,
        finish_reason,
        usage,
        continuation: None,
        warnings: Vec::new(),
        raw: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_defaults() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 2);
    }
}
