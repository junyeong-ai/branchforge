//! Provider adapter trait definition.

use std::fmt::Debug;

use async_trait::async_trait;

use super::config::{ModelType, ProviderConfig};
use crate::client::messages::{CountTokensRequest, CountTokensResponse, CreateMessageRequest};
use crate::client::streaming::{StreamItem, stream_event_to_item};
use crate::types::{ApiResponse, StreamEvent};
use crate::{Error, Result};

/// Wire format used by a provider's streaming endpoint.
///
/// The [`Client`](crate::client::Client) uses this to decide how to parse the
/// byte stream returned by [`ProviderAdapter::send_stream`].
///
/// - **Sse** — Standard SSE (`data: {json}\n\n`).  Used by Anthropic Direct,
///   OpenAI, and Gemini (`alt=sse`).  The built-in `StreamParser` handles
///   this format.
/// - **AwsEventStream** — AWS binary Event Stream framing.  Used by Bedrock's
///   `converse-stream`.  The [`AwsEventStreamParser`](crate::client::AwsEventStreamParser)
///   decodes binary frames and extracts JSON payloads for event parsing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StreamFormat {
    /// Standard Server-Sent Events (`data: {json}\n\n`).
    #[default]
    Sse,
    /// AWS binary Event Stream framing (`application/vnd.amazon.eventstream`).
    AwsEventStream,
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync + Debug {
    fn config(&self) -> &ProviderConfig;

    fn name(&self) -> &'static str;

    /// Returns the base URL for API requests (e.g. `https://api.anthropic.com`).
    fn base_url(&self) -> &str {
        "https://api.anthropic.com"
    }

    fn model(&self, model_type: ModelType) -> &str {
        self.config().models.get(model_type)
    }

    async fn build_url(&self, model: &str, stream: bool) -> String;

    async fn prepare_request(&self, request: CreateMessageRequest) -> CreateMessageRequest {
        request
    }

    async fn transform_request(&self, request: CreateMessageRequest) -> Result<serde_json::Value>;

    fn transform_response(&self, response: serde_json::Value) -> Result<ApiResponse> {
        serde_json::from_value(response).map_err(|e| Error::Parse(e.to_string()))
    }

    /// Parse a single SSE JSON event into a [`StreamItem`].
    ///
    /// The default implementation deserialises Anthropic-format `StreamEvent`s.
    /// Non-Anthropic adapters (OpenAI, Gemini) override this to parse their
    /// own SSE JSON format directly into `StreamItem`.
    ///
    /// Return `None` to skip the event (e.g. heartbeats, `[DONE]`).
    fn parse_stream_event(&self, json: &str) -> Option<StreamItem> {
        let event = serde_json::from_str::<StreamEvent>(json)
            .inspect_err(|e| tracing::warn!("Failed to parse stream event: {} - data: {}", e, json))
            .ok()?;
        Some(stream_event_to_item(event))
    }

    /// Wire format of the streaming response.
    ///
    /// Adapters whose providers use a format other than standard SSE
    /// should override this so the client can choose the right decoder.
    fn stream_format(&self) -> StreamFormat {
        StreamFormat::Sse
    }

    async fn apply_auth_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req
    }

    async fn send(
        &self,
        http: &reqwest::Client,
        request: CreateMessageRequest,
    ) -> Result<ApiResponse>;

    async fn send_stream(
        &self,
        http: &reqwest::Client,
        request: CreateMessageRequest,
    ) -> Result<reqwest::Response>;

    fn supports_credential_refresh(&self) -> bool {
        false
    }

    async fn ensure_fresh_credentials(&self) -> Result<()> {
        Ok(())
    }

    async fn refresh_credentials(&self) -> Result<()> {
        Ok(())
    }

    async fn count_tokens(
        &self,
        _http: &reqwest::Client,
        _request: CountTokensRequest,
    ) -> Result<CountTokensResponse> {
        Err(Error::NotSupported {
            provider: self.name(),
            operation: "count_tokens",
        })
    }
}
