//! AWS Bedrock adapter using the Converse API.
//!
//! Uses the Bedrock Converse API format with SigV4 signing.
//! Supports global and regional endpoints as documented at:
//! <https://platform.claude.com/docs/en/build-with-claude/claude-on-amazon-bedrock>

use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_credential_types::provider::ProvideCredentials;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningSettings, sign};
use aws_sigv4::sign::v4::SigningParams;
use aws_smithy_runtime_api::client::identity::Identity;
use secrecy::ExposeSecret;
use serde_json::{Value, json};

use super::base::RequestExecutor;
use super::config::ProviderConfig;
use super::token_cache::{AwsCredentialsCache, CachedAwsCredentials, new_aws_credentials_cache};
use super::traits::ProviderAdapter;
use crate::client::messages::{ApiTool, CreateMessageRequest};
use crate::client::streaming::StreamItem;
use crate::types::{
    ApiResponse, ContentBlock, Role, StopReason, ToolResultContent, ToolUseBlock, Usage,
};
use crate::{Error, Result};

#[derive(Debug)]
pub struct BedrockAdapter {
    config: ProviderConfig,
    region: String,
    small_model_region: Option<String>,
    use_global_endpoint: bool,
    enable_1m_context: bool,
    auth: BedrockAuth,
    credentials_cache: AwsCredentialsCache,
}

#[derive(Debug)]
enum BedrockAuth {
    SigV4(Arc<dyn ProvideCredentials>),
    BearerToken(String),
}

impl BedrockAdapter {
    pub async fn from_env(config: ProviderConfig) -> Result<Self> {
        let bedrock_config = crate::config::BedrockConfig::from_env();
        Self::from_config(config, bedrock_config).await
    }

    pub async fn from_config(
        config: ProviderConfig,
        bedrock: crate::config::BedrockConfig,
    ) -> Result<Self> {
        let region = bedrock.region.unwrap_or_else(|| "us-east-1".into());

        let auth = if let Some(token) = bedrock.bearer_token {
            BedrockAuth::BearerToken(token)
        } else {
            let aws_config = aws_config::load_defaults(BehaviorVersion::latest()).await;
            let credentials = aws_config
                .credentials_provider()
                .ok_or_else(|| Error::auth("No AWS credentials found"))?;
            BedrockAuth::SigV4(Arc::from(credentials))
        };

        Ok(Self {
            config,
            region,
            small_model_region: bedrock.small_model_region,
            use_global_endpoint: bedrock.use_global_endpoint,
            enable_1m_context: bedrock.enable_1m_context,
            auth,
            credentials_cache: new_aws_credentials_cache(),
        })
    }

    pub fn region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    pub fn small_model_region(mut self, region: impl Into<String>) -> Self {
        self.small_model_region = Some(region.into());
        self
    }

    pub fn global_endpoint(mut self, enable: bool) -> Self {
        self.use_global_endpoint = enable;
        self
    }

    pub fn use_1m_context(mut self, enable: bool) -> Self {
        self.enable_1m_context = enable;
        self
    }

    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        self.auth = BedrockAuth::BearerToken(token.into());
        self
    }

    fn region_for_model(&self, model: &str) -> &str {
        if let Some(ref small_region) = self.small_model_region
            && model.contains("haiku")
        {
            return small_region;
        }
        &self.region
    }

    fn build_converse_url(&self, model: &str, stream: bool) -> String {
        let region = self.region_for_model(model);
        let endpoint = if stream {
            "converse-stream"
        } else {
            "converse"
        };
        let encoded_model = urlencoding::encode(model);

        format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/{}",
            region, encoded_model, endpoint
        )
    }

    fn build_converse_body(&self, request: &CreateMessageRequest) -> Value {
        let mut body = json!({});

        // Convert messages
        let messages = Self::convert_messages(request);
        body["messages"] = json!(messages);

        // System prompt
        if let Some(ref system) = request.system {
            let text = system.as_text();
            if !text.is_empty() {
                body["system"] = json!([{"text": text}]);
            }
        }

        // InferenceConfig
        let mut inference_config = json!({
            "maxTokens": request.max_tokens,
        });
        if let Some(temp) = request.temperature {
            inference_config["temperature"] = json!(temp);
        }
        if let Some(top_p) = request.top_p {
            inference_config["topP"] = json!(top_p);
        }
        if let Some(ref stop) = request.stop_sequences {
            inference_config["stopSequences"] = json!(stop);
        }
        body["inferenceConfig"] = inference_config;

        // Tools
        if let Some(ref tools) = request.tools {
            let tool_specs = Self::convert_tools(tools);
            if !tool_specs.is_empty() {
                body["toolConfig"] = json!({ "tools": tool_specs });
            }
        }

        // Thinking / beta / structured output go in additionalModelRequestFields
        // (Anthropic-specific fields passed through to the underlying model)
        let mut additional = json!({});
        if let Some(ref thinking) = request.thinking {
            additional["thinking"] = json!(thinking);
        } else if let Some(budget) = self.config.thinking_budget {
            additional["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": budget
            });
        }
        // Structured output — pass as Anthropic-native output_format
        if let Some(ref fmt) = request.output_format {
            additional["output_format"] = json!(fmt);
        }
        // Beta features
        let mut beta_features = Vec::new();
        if self.enable_1m_context {
            beta_features.push(super::BetaFeature::Context1M.header_value());
        }
        if request.output_format.is_some()
            || request
                .tools
                .as_ref()
                .is_some_and(|t| t.iter().any(|tool| tool.is_strict()))
        {
            beta_features.push(super::BetaFeature::StructuredOutputs.header_value());
        }
        if !beta_features.is_empty() {
            additional["anthropic_beta"] = json!(beta_features);
        }
        if additional.as_object().is_some_and(|o| !o.is_empty()) {
            body["additionalModelRequestFields"] = additional;
        }

        body
    }

    fn convert_content_block(block: &ContentBlock) -> Option<Value> {
        match block {
            ContentBlock::Text { text, .. } => Some(json!({"text": text})),
            ContentBlock::ToolUse(tu) => Some(json!({
                "toolUse": {
                    "toolUseId": tu.id,
                    "name": tu.name,
                    "input": tu.input,
                }
            })),
            ContentBlock::ToolResult(tr) => {
                let content_text = match &tr.content {
                    Some(ToolResultContent::Text(t)) => t.clone(),
                    Some(ToolResultContent::Blocks(blocks)) => blocks
                        .iter()
                        .filter_map(|b| match b {
                            crate::types::ToolResultContentBlock::Text { text } => {
                                Some(text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    None => String::new(),
                };
                let status = if tr.is_error == Some(true) {
                    "error"
                } else {
                    "success"
                };
                Some(json!({
                    "toolResult": {
                        "toolUseId": tr.tool_use_id,
                        "content": [{"text": content_text}],
                        "status": status,
                    }
                }))
            }
            ContentBlock::Image {
                source: crate::types::ImageSource::Base64 { media_type, data },
            } => {
                let format = match media_type.as_str() {
                    "image/jpeg" => "jpeg",
                    "image/png" => "png",
                    "image/gif" => "gif",
                    "image/webp" => "webp",
                    other => other,
                };
                Some(json!({
                    "image": {
                        "format": format,
                        "source": {"bytes": data},
                    }
                }))
            }
            // Skip thinking, redacted thinking, and other internal blocks
            _ => None,
        }
    }

    fn convert_messages(request: &CreateMessageRequest) -> Vec<Value> {
        let mut messages = Vec::new();
        for msg in &request.messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let content: Vec<Value> = msg
                .content
                .iter()
                .filter_map(Self::convert_content_block)
                .collect();
            if !content.is_empty() {
                messages.push(json!({
                    "role": role,
                    "content": content,
                }));
            }
        }
        messages
    }

    fn convert_tools(tools: &[ApiTool]) -> Vec<Value> {
        tools
            .iter()
            .filter_map(|tool| match tool {
                ApiTool::Custom(def) => {
                    let mut spec = json!({
                        "name": def.name,
                        "description": def.description,
                        "inputSchema": {
                            "json": def.input_schema,
                        },
                    });
                    if def.strict == Some(true) {
                        spec["strict"] = json!(true);
                    }
                    Some(json!({ "toolSpec": spec }))
                }
                // Skip server-side tools as they are Anthropic-specific
                _ => None,
            })
            .collect()
    }

    fn parse_converse_response(json: Value, model: &str) -> Result<ApiResponse> {
        let mut content = Vec::new();

        // Parse output.message.content
        if let Some(output) = json.get("output")
            && let Some(message) = output.get("message")
            && let Some(blocks) = message.get("content").and_then(|c| c.as_array())
        {
            for block in blocks {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    content.push(ContentBlock::text(text));
                }
                if let Some(tu) = block.get("toolUse") {
                    let id = tu
                        .get("toolUseId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = tu
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = tu.get("input").cloned().unwrap_or(json!({}));
                    content.push(ContentBlock::ToolUse(ToolUseBlock { id, name, input }));
                }
            }
        }

        // Parse stopReason
        let stop_reason =
            json.get("stopReason")
                .and_then(|v| v.as_str())
                .map(|reason| match reason {
                    "end_turn" => StopReason::EndTurn,
                    "tool_use" => StopReason::ToolUse,
                    "max_tokens" => StopReason::MaxTokens,
                    "content_filtered" => StopReason::Refusal,
                    "stop_sequence" => StopReason::StopSequence,
                    _ => StopReason::EndTurn,
                });

        // Parse usage
        let usage = if let Some(u) = json.get("usage") {
            Usage {
                input_tokens: u.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                output_tokens: u.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                ..Default::default()
            }
        } else {
            Usage::default()
        };

        // Generate an ID (Converse doesn't return one in the same way)
        let id = format!("bedrock-{}", uuid::Uuid::new_v4().simple());

        Ok(ApiResponse {
            id,
            response_type: "message".into(),
            role: "assistant".into(),
            content,
            model: model.to_string(),
            stop_reason,
            stop_sequence: None,
            usage,
            context_management: None,
        })
    }

    async fn get_credentials(&self) -> Result<CachedAwsCredentials> {
        let provider = match &self.auth {
            BedrockAuth::SigV4(p) => p,
            BedrockAuth::BearerToken(_) => {
                return Err(Error::auth("Bearer token mode does not use credentials"));
            }
        };

        {
            let cache = self.credentials_cache.read().await;
            if let Some(ref creds) = *cache
                && !creds.is_expired()
            {
                return Ok(creds.clone());
            }
        }

        let creds = provider
            .provide_credentials()
            .await
            .map_err(|e| Error::auth(e.to_string()))?;

        let cached = CachedAwsCredentials::new(
            creds.access_key_id().to_string(),
            creds.secret_access_key().to_string(),
            creds.session_token().map(|s| s.to_string()),
            creds.expiry(),
        );

        *self.credentials_cache.write().await = Some(cached.clone());
        Ok(cached)
    }

    async fn get_auth_headers(
        &self,
        method: &str,
        url: &str,
        body: &[u8],
        region: &str,
    ) -> Result<Vec<(String, String)>> {
        match &self.auth {
            BedrockAuth::BearerToken(token) => {
                Ok(vec![("Authorization".into(), format!("Bearer {}", token))])
            }
            BedrockAuth::SigV4(_) => self.sign_request(method, url, body, region).await,
        }
    }

    async fn sign_request(
        &self,
        method: &str,
        url: &str,
        body: &[u8],
        region: &str,
    ) -> Result<Vec<(String, String)>> {
        let creds = self.get_credentials().await?;

        let aws_creds = aws_credential_types::Credentials::new(
            &creds.access_key_id,
            creds.secret_access_key.expose_secret(),
            creds
                .session_token
                .as_ref()
                .map(|s| s.expose_secret().to_string()),
            creds.expiry(),
            "bedrock-adapter",
        );

        let identity = Identity::new(aws_creds, creds.expiry());

        let signing_params = SigningParams::builder()
            .identity(&identity)
            .region(region)
            .name("bedrock")
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .map_err(|e| Error::auth(e.to_string()))?;

        let signable_request = SignableRequest::new(
            method,
            url,
            std::iter::empty::<(&str, &str)>(),
            SignableBody::Bytes(body),
        )
        .map_err(|e| Error::auth(e.to_string()))?;

        let (signing_instructions, _) = sign(signable_request, &signing_params.into())
            .map_err(|e| Error::auth(e.to_string()))?
            .into_parts();

        Ok(signing_instructions
            .headers()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect())
    }

    async fn execute_request(
        &self,
        http: &reqwest::Client,
        url: &str,
        body_bytes: Vec<u8>,
        region: &str,
    ) -> Result<reqwest::Response> {
        let headers = self
            .get_auth_headers("POST", url, &body_bytes, region)
            .await?;
        RequestExecutor::post_bytes(http, url, body_bytes, headers).await
    }

    async fn execute_stream_request(
        &self,
        http: &reqwest::Client,
        url: &str,
        body_bytes: Vec<u8>,
        region: &str,
    ) -> Result<reqwest::Response> {
        let auth_headers = self
            .get_auth_headers("POST", url, &body_bytes, region)
            .await?;

        let mut req = http
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/vnd.amazon.eventstream")
            .body(body_bytes);

        for (name, value) in auth_headers {
            req = req.header(&name, &value);
        }

        let response = req.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Api {
                message: text,
                status: Some(status),
                error_type: None,
            });
        }
        Ok(response)
    }

    /// Parse a Bedrock Converse Stream event.
    ///
    /// AWS EventStream frames carry the event type in the `:event-type` header,
    /// and the JSON payload is flat (no wrapper key).  The caller
    /// (`AwsEventStreamParser`) prepends `__event_type=<type>\n` to the JSON
    /// string so we can dispatch without modifying the streaming infrastructure.
    ///
    /// Event types: `contentBlockDelta`, `contentBlockStart`, `contentBlockStop`,
    /// `messageStart`, `messageStop`, `metadata`.
    pub(crate) fn parse_converse_stream_event(raw: &str) -> Option<StreamItem> {
        use crate::types::{ContentDelta, MessageDeltaData, StreamEvent};

        // Extract event type prefix if present: "__event_type=<type>\n<json>"
        let (event_type, json) = if let Some(rest) = raw.strip_prefix("__event_type=") {
            if let Some(nl_pos) = rest.find('\n') {
                (&rest[..nl_pos], &rest[nl_pos + 1..])
            } else {
                ("", raw)
            }
        } else {
            ("", raw)
        };

        let v: Value = serde_json::from_str(json)
            .inspect_err(|e| {
                tracing::warn!(
                    "Failed to parse Bedrock stream event: {} - data: {}",
                    e,
                    json
                )
            })
            .ok()?;

        match event_type {
            "contentBlockDelta" => {
                let index = v
                    .get("contentBlockIndex")
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0) as usize;

                if let Some(delta) = v.get("delta") {
                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                        return Some(StreamItem::Text(text.to_string()));
                    }
                    if let Some(partial_json) = delta
                        .get("toolUse")
                        .and_then(|tu| tu.get("input"))
                        .and_then(|i| i.as_str())
                    {
                        return Some(StreamItem::Event(StreamEvent::ContentBlockDelta {
                            index,
                            delta: ContentDelta::InputJsonDelta {
                                partial_json: partial_json.to_string(),
                            },
                        }));
                    }
                }
                None
            }
            "contentBlockStart" => {
                let index = v
                    .get("contentBlockIndex")
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0) as usize;

                if let Some(start) = v.get("start")
                    && let Some(tu) = start.get("toolUse")
                {
                    let id = tu
                        .get("toolUseId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = tu
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(StreamItem::Event(StreamEvent::ContentBlockStart {
                        index,
                        content_block: ContentBlock::ToolUse(ToolUseBlock {
                            id,
                            name,
                            input: json!({}),
                        }),
                    }))
                } else {
                    None
                }
            }
            "contentBlockStop" => {
                let index = v
                    .get("contentBlockIndex")
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0) as usize;
                Some(StreamItem::Event(StreamEvent::ContentBlockStop { index }))
            }
            "messageStop" => {
                let _stop_reason = v
                    .get("stopReason")
                    .and_then(|r| r.as_str())
                    .map(|reason| match reason {
                        "end_turn" => StopReason::EndTurn,
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" => StopReason::MaxTokens,
                        "content_filtered" => StopReason::Refusal,
                        "stop_sequence" => StopReason::StopSequence,
                        _ => StopReason::EndTurn,
                    });
                Some(StreamItem::Event(StreamEvent::MessageStop))
            }
            "messageStart" => None,
            "metadata" => {
                let u = v.get("usage")?;
                let usage = Usage {
                    input_tokens: u.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    output_tokens: u.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    ..Default::default()
                };
                Some(StreamItem::Event(StreamEvent::MessageDelta {
                    delta: MessageDeltaData {
                        stop_reason: None,
                        stop_sequence: None,
                    },
                    usage,
                }))
            }
            _ => None,
        }
    }
}

#[async_trait]
impl ProviderAdapter for BedrockAdapter {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn name(&self) -> &'static str {
        "bedrock"
    }

    fn stream_format(&self) -> super::traits::StreamFormat {
        super::traits::StreamFormat::AwsEventStream
    }

    fn parse_stream_event(&self, _json: &str) -> Option<StreamItem> {
        // Bedrock stream events are parsed through the binary Event Stream
        // decoder in the custom StreamParser path, not via SSE JSON events.
        // This method is unused for AwsEventStream format.
        None
    }

    async fn build_url(&self, model: &str, stream: bool) -> String {
        self.build_converse_url(model, stream)
    }

    async fn transform_request(&self, request: CreateMessageRequest) -> Result<serde_json::Value> {
        Ok(self.build_converse_body(&request))
    }

    fn transform_response(&self, response: serde_json::Value) -> Result<ApiResponse> {
        Self::parse_converse_response(response, &self.config.models.primary)
    }

    async fn send(
        &self,
        http: &reqwest::Client,
        request: CreateMessageRequest,
    ) -> Result<ApiResponse> {
        let model = request.model.clone();
        let region = self.region_for_model(&model);
        let url = self.build_converse_url(&model, false);
        let body = self.build_converse_body(&request);
        let body_bytes = serde_json::to_vec(&body)?;

        let response = self.execute_request(http, &url, body_bytes, region).await?;
        let json: serde_json::Value = response.json().await?;
        Self::parse_converse_response(json, &model)
    }

    async fn send_stream(
        &self,
        http: &reqwest::Client,
        request: CreateMessageRequest,
    ) -> Result<reqwest::Response> {
        let model = request.model.clone();
        let region = self.region_for_model(&model);
        let url = self.build_converse_url(&model, true);
        let body = self.build_converse_body(&request);
        let body_bytes = serde_json::to_vec(&body)?;

        self.execute_stream_request(http, &url, body_bytes, region)
            .await
    }

    async fn refresh_credentials(&self) -> Result<()> {
        if matches!(self.auth, BedrockAuth::SigV4(_)) {
            *self.credentials_cache.write().await = None;
            self.get_credentials().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::adapter::{BetaFeature, ModelConfig};
    use crate::types::{Message, ToolResultBlock};

    #[test]
    fn test_url_encoding() {
        let model = "global.anthropic.claude-sonnet-4-5-20250929-v1:0";
        let encoded = urlencoding::encode(model);
        assert!(encoded.contains("%3A"));
        assert!(encoded.contains("global.anthropic"));
    }

    #[test]
    fn test_converse_url_format() {
        let model = "global.anthropic.claude-sonnet-4-5-20250929-v1:0";
        let encoded = urlencoding::encode(model);
        let url = format!(
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/{}/converse",
            encoded
        );
        assert!(url.contains("bedrock-runtime"));
        assert!(url.contains("/model/"));
        assert!(url.contains("/converse"));
        assert!(url.contains("%3A"));
    }

    #[test]
    fn test_converse_stream_url_format() {
        let model = "global.anthropic.claude-sonnet-4-5-20250929-v1:0";
        let encoded = urlencoding::encode(model);
        let url = format!(
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/{}/converse-stream",
            encoded
        );
        assert!(url.contains("/converse-stream"));
    }

    #[test]
    fn test_model_config() {
        let config = ModelConfig::bedrock();
        assert!(config.primary.contains("anthropic"));
        assert!(config.primary.contains("global"));
    }

    #[test]
    fn test_converse_request_body() {
        let request = CreateMessageRequest::new(
            "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
            vec![Message::user("Hello")],
        )
        .max_tokens(1024);

        let body = BedrockAdapter::convert_messages(&request);
        assert_eq!(body.len(), 1);
        assert_eq!(body[0]["role"], "user");
        let content = body[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["text"], "Hello");
    }

    #[test]
    fn test_converse_response_parsing() {
        let response = json!({
            "output": {
                "message": {
                    "role": "assistant",
                    "content": [
                        {"text": "Hello there!"},
                    ]
                }
            },
            "stopReason": "end_turn",
            "usage": {
                "inputTokens": 100,
                "outputTokens": 50,
                "totalTokens": 150
            }
        });

        let api_response = BedrockAdapter::parse_converse_response(response, "test-model").unwrap();
        assert_eq!(api_response.text(), "Hello there!");
        assert_eq!(api_response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(api_response.usage.input_tokens, 100);
        assert_eq!(api_response.usage.output_tokens, 50);
    }

    #[test]
    fn test_converse_response_with_tool_use() {
        let response = json!({
            "output": {
                "message": {
                    "role": "assistant",
                    "content": [
                        {"toolUse": {
                            "toolUseId": "tool_123",
                            "name": "get_weather",
                            "input": {"location": "NYC"}
                        }}
                    ]
                }
            },
            "stopReason": "tool_use",
            "usage": {
                "inputTokens": 50,
                "outputTokens": 30
            }
        });

        let api_response = BedrockAdapter::parse_converse_response(response, "test-model").unwrap();
        assert_eq!(api_response.stop_reason, Some(StopReason::ToolUse));
        let tool_uses = api_response.tool_uses();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "get_weather");
        assert_eq!(tool_uses[0].id, "tool_123");
        assert_eq!(tool_uses[0].input["location"], "NYC");
    }

    #[test]
    fn test_converse_tool_conversion() {
        use crate::types::ToolDefinition;

        let tool = ToolDefinition::new(
            "get_weather",
            "Get weather for a location",
            json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                },
                "required": ["location"]
            }),
        );
        let api_tools = vec![ApiTool::Custom(tool)];
        let specs = BedrockAdapter::convert_tools(&api_tools);

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0]["toolSpec"]["name"], "get_weather");
        assert_eq!(
            specs[0]["toolSpec"]["description"],
            "Get weather for a location"
        );
        assert!(specs[0]["toolSpec"]["inputSchema"]["json"].is_object());
    }

    #[test]
    fn test_beta_feature_in_additional_fields() {
        let beta_value = BetaFeature::Context1M.header_value();
        let additional = json!({
            "anthropic_beta": [beta_value],
        });
        assert_eq!(additional["anthropic_beta"][0], beta_value);
    }

    #[test]
    fn test_convert_tool_result_message() {
        let tool_result_msg =
            Message::tool_results(vec![ToolResultBlock::success("call_123", "Sunny, 22C")]);
        let request = CreateMessageRequest::new(
            "global.anthropic.claude-sonnet-4-5-20250929-v1:0",
            vec![tool_result_msg],
        );
        let messages = BedrockAdapter::convert_messages(&request);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert!(content[0].get("toolResult").is_some());
        assert_eq!(content[0]["toolResult"]["toolUseId"], "call_123");
        assert_eq!(content[0]["toolResult"]["status"], "success");
    }

    /// Helper: build the `__event_type=<type>\n<json>` format that
    /// `AwsEventStreamParser` produces for `parse_converse_stream_event`.
    fn prefixed(event_type: &str, json: &str) -> String {
        format!("__event_type={event_type}\n{json}")
    }

    #[test]
    fn test_parse_stream_text_delta() {
        let input = prefixed(
            "contentBlockDelta",
            r#"{"contentBlockIndex":0,"delta":{"text":"Hello world"}}"#,
        );
        let item = BedrockAdapter::parse_converse_stream_event(&input);
        assert!(matches!(item, Some(StreamItem::Text(ref t)) if t == "Hello world"));
    }

    #[test]
    fn test_parse_stream_tool_use_start() {
        let input = prefixed(
            "contentBlockStart",
            r#"{"contentBlockIndex":1,"start":{"toolUse":{"toolUseId":"tool_abc","name":"get_weather"}}}"#,
        );
        let item = BedrockAdapter::parse_converse_stream_event(&input);
        match item {
            Some(StreamItem::Event(crate::types::StreamEvent::ContentBlockStart {
                index,
                content_block: ContentBlock::ToolUse(tu),
            })) => {
                assert_eq!(index, 1);
                assert_eq!(tu.id, "tool_abc");
                assert_eq!(tu.name, "get_weather");
            }
            other => panic!("Expected ContentBlockStart with ToolUse, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_stream_tool_input_delta() {
        let input = prefixed(
            "contentBlockDelta",
            r#"{"contentBlockIndex":1,"delta":{"toolUse":{"input":"{\"loc\":\"NYC\"}"}}}"#,
        );
        let item = BedrockAdapter::parse_converse_stream_event(&input);
        match item {
            Some(StreamItem::Event(crate::types::StreamEvent::ContentBlockDelta {
                index,
                delta: crate::types::ContentDelta::InputJsonDelta { partial_json },
            })) => {
                assert_eq!(index, 1);
                assert_eq!(partial_json, r#"{"loc":"NYC"}"#);
            }
            other => panic!("Expected InputJsonDelta, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_stream_content_block_stop() {
        let input = prefixed(
            "contentBlockStop",
            r#"{"contentBlockIndex":0}"#,
        );
        let item = BedrockAdapter::parse_converse_stream_event(&input);
        assert!(matches!(
            item,
            Some(StreamItem::Event(
                crate::types::StreamEvent::ContentBlockStop { index: 0 }
            ))
        ));
    }

    #[test]
    fn test_parse_stream_message_stop() {
        let input = prefixed("messageStop", r#"{"stopReason":"end_turn"}"#);
        let item = BedrockAdapter::parse_converse_stream_event(&input);
        assert!(
            matches!(item, Some(StreamItem::Event(crate::types::StreamEvent::MessageStop))),
            "Expected MessageStop, got {:?}",
            item
        );
    }

    #[test]
    fn test_parse_stream_message_stop_tool_use() {
        let input = prefixed("messageStop", r#"{"stopReason":"tool_use"}"#);
        let item = BedrockAdapter::parse_converse_stream_event(&input);
        assert!(
            matches!(item, Some(StreamItem::Event(crate::types::StreamEvent::MessageStop))),
            "Expected MessageStop, got {:?}",
            item
        );
    }

    #[test]
    fn test_parse_stream_metadata_usage() {
        let input = prefixed(
            "metadata",
            r#"{"usage":{"inputTokens":100,"outputTokens":50},"metrics":{"latencyMs":123}}"#,
        );
        let item = BedrockAdapter::parse_converse_stream_event(&input);
        match item {
            Some(StreamItem::Event(crate::types::StreamEvent::MessageDelta { usage, .. })) => {
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 50);
            }
            other => panic!("Expected MessageDelta with usage, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_stream_message_start_ignored() {
        let input = prefixed("messageStart", r#"{"role":"assistant"}"#);
        let item = BedrockAdapter::parse_converse_stream_event(&input);
        assert!(item.is_none());
    }

    #[test]
    fn test_parse_stream_unknown_event_ignored() {
        let input = prefixed("unknownEvent", r#"{"data":"something"}"#);
        let item = BedrockAdapter::parse_converse_stream_event(&input);
        assert!(item.is_none());
    }

    #[test]
    fn test_parse_stream_without_prefix_returns_none() {
        // Raw JSON without __event_type= prefix should return None
        let json = r#"{"contentBlockDelta":{"delta":{"text":"hi"}}}"#;
        let item = BedrockAdapter::parse_converse_stream_event(json);
        assert!(item.is_none());
    }
}
