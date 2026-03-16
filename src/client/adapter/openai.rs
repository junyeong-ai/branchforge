//! OpenAI-compatible API adapter.
//!
//! Supports OpenAI's Chat Completion API format, enabling GPT-4o, o3,
//! and OpenAI-compatible endpoints (Together, Groq, Ollama, vLLM).

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::config::ProviderConfig;
use super::traits::ProviderAdapter;
use crate::client::messages::{ApiTool, CreateMessageRequest, ErrorResponse, OutputFormat};
use crate::types::{
    ApiResponse, ContentBlock, Message, Role, StopReason, ToolResultBlock, ToolResultContent,
    ToolUseBlock, Usage,
};
use crate::{Error, Result};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

// -- OpenAI request types ------------------------------------------------

#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<OpenAiResponseFormat>,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<OpenAiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum OpenAiContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrlDetail },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImageUrlDetail {
    url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAiFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiFunctionDef,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiFunctionDef {
    name: String,
    description: String,
    parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    strict: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiResponseFormat {
    JsonSchema { json_schema: OpenAiJsonSchema },
}

#[derive(Debug, Clone, Serialize)]
struct OpenAiJsonSchema {
    name: String,
    schema: Value,
    strict: bool,
}

// -- OpenAI response types ------------------------------------------------

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    id: String,
    model: String,
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponseMessage {
    #[allow(dead_code)]
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

// -- OpenAI error response ------------------------------------------------

#[derive(Debug, Deserialize)]
struct OpenAiErrorResponse {
    error: OpenAiErrorDetail,
}

#[derive(Debug, Deserialize)]
struct OpenAiErrorDetail {
    message: String,
    #[serde(rename = "type")]
    error_type: Option<String>,
}

// -- Adapter implementation -----------------------------------------------

pub struct OpenAiAdapter {
    config: ProviderConfig,
    api_key: SecretString,
    base_url: String,
}

impl std::fmt::Debug for OpenAiAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiAdapter")
            .field("config", &self.config)
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl OpenAiAdapter {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            config,
            api_key: Self::api_key_from_env(),
            base_url: Self::base_url_from_env(),
        }
    }

    pub fn from_api_key(config: ProviderConfig, api_key: impl Into<String>) -> Self {
        Self {
            config,
            api_key: SecretString::from(api_key.into()),
            base_url: Self::base_url_from_env(),
        }
    }

    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn api_key(self, key: impl Into<String>) -> Self {
        Self {
            api_key: SecretString::from(key.into()),
            ..self
        }
    }

    fn api_key_from_env() -> SecretString {
        SecretString::from(std::env::var("OPENAI_API_KEY").unwrap_or_default())
    }

    fn base_url_from_env() -> String {
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.into())
    }

    // -- Format conversions ---------------------------------------------------

    fn convert_messages(request: &CreateMessageRequest) -> Vec<OpenAiMessage> {
        let mut oai_messages = Vec::new();

        // System prompt -> system message
        if let Some(ref system) = request.system {
            let text = system.as_text();
            if !text.is_empty() {
                oai_messages.push(OpenAiMessage {
                    role: "system".into(),
                    content: Some(OpenAiContent::Text(text)),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
        }

        for msg in &request.messages {
            match msg.role {
                Role::User => {
                    Self::convert_user_message(msg, &mut oai_messages);
                }
                Role::Assistant => {
                    Self::convert_assistant_message(msg, &mut oai_messages);
                }
            }
        }

        oai_messages
    }

    fn convert_user_message(msg: &Message, out: &mut Vec<OpenAiMessage>) {
        // Check if message contains tool results
        let tool_results: Vec<&ToolResultBlock> = msg
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult(tr) => Some(tr),
                _ => None,
            })
            .collect();

        if !tool_results.is_empty() {
            // Emit tool result messages
            for tr in &tool_results {
                let content_text = match &tr.content {
                    Some(ToolResultContent::Text(t)) => t.clone(),
                    Some(ToolResultContent::Blocks(blocks)) => {
                        // Concatenate text blocks
                        blocks
                            .iter()
                            .filter_map(|b| match b {
                                crate::types::ToolResultContentBlock::Text { text } => {
                                    Some(text.as_str())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                    None => String::new(),
                };

                // If tool returned an error, prefix it
                let content_text = if tr.is_error == Some(true) {
                    format!("[Error] {}", content_text)
                } else {
                    content_text
                };

                out.push(OpenAiMessage {
                    role: "tool".into(),
                    content: Some(OpenAiContent::Text(content_text)),
                    tool_calls: None,
                    tool_call_id: Some(tr.tool_use_id.clone()),
                    name: None,
                });
            }

            // Also emit any non-tool-result content as a user message
            let text_parts: Vec<String> = msg
                .content
                .iter()
                .filter_map(|b| b.as_text().map(String::from))
                .collect();
            if !text_parts.is_empty() {
                out.push(OpenAiMessage {
                    role: "user".into(),
                    content: Some(OpenAiContent::Text(text_parts.join("\n"))),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
        } else {
            // Regular user message
            let parts: Vec<OpenAiContentPart> = msg
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text, .. } => {
                        Some(OpenAiContentPart::Text { text: text.clone() })
                    }
                    ContentBlock::Image { source } => match source {
                        crate::types::ImageSource::Url { url } => {
                            Some(OpenAiContentPart::ImageUrl {
                                image_url: ImageUrlDetail { url: url.clone() },
                            })
                        }
                        crate::types::ImageSource::Base64 { media_type, data } => {
                            Some(OpenAiContentPart::ImageUrl {
                                image_url: ImageUrlDetail {
                                    url: format!("data:{};base64,{}", media_type, data),
                                },
                            })
                        }
                        // File references can't be directly converted
                        crate::types::ImageSource::File { .. } => None,
                    },
                    _ => None,
                })
                .collect();

            if parts.len() == 1
                && let OpenAiContentPart::Text { ref text } = parts[0]
            {
                out.push(OpenAiMessage {
                    role: "user".into(),
                    content: Some(OpenAiContent::Text(text.clone())),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
                return;
            }

            if !parts.is_empty() {
                out.push(OpenAiMessage {
                    role: "user".into(),
                    content: Some(OpenAiContent::Parts(parts)),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
        }
    }

    fn convert_assistant_message(msg: &Message, out: &mut Vec<OpenAiMessage>) {
        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for block in &msg.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    text_parts.push(text.clone());
                }
                ContentBlock::ToolUse(tu) => {
                    let arguments = serde_json::to_string(&tu.input).unwrap_or_default();
                    tool_calls.push(OpenAiToolCall {
                        id: tu.id.clone(),
                        call_type: "function".into(),
                        function: OpenAiFunctionCall {
                            name: tu.name.clone(),
                            arguments,
                        },
                    });
                }
                // Skip thinking blocks, redacted thinking, etc.
                _ => {}
            }
        }

        let content = if text_parts.is_empty() {
            None
        } else {
            Some(OpenAiContent::Text(text_parts.join("")))
        };

        let tool_calls_field = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        };

        out.push(OpenAiMessage {
            role: "assistant".into(),
            content,
            tool_calls: tool_calls_field,
            tool_call_id: None,
            name: None,
        });
    }

    fn convert_tools(tools: &[ApiTool]) -> Vec<OpenAiTool> {
        tools
            .iter()
            .filter_map(|tool| match tool {
                ApiTool::Custom(def) => Some(OpenAiTool {
                    tool_type: "function".into(),
                    function: OpenAiFunctionDef {
                        name: def.name.clone(),
                        description: def.description.clone(),
                        parameters: def.input_schema.clone(),
                        strict: def.strict,
                    },
                }),
                // Skip server-side tools (WebSearch, WebFetch, ToolSearch)
                // as they are Anthropic-specific and have no OpenAI equivalent
                _ => None,
            })
            .collect()
    }

    fn build_openai_request(request: &CreateMessageRequest, stream: bool) -> OpenAiRequest {
        let messages = Self::convert_messages(request);
        let tools = request
            .tools
            .as_ref()
            .map(|t| Self::convert_tools(t))
            .filter(|t| !t.is_empty());

        // Map Anthropic OutputFormat to OpenAI response_format
        let response_format = request.output_format.as_ref().map(|fmt| {
            let OutputFormat::JsonSchema { name, schema, .. } = fmt;
            OpenAiResponseFormat::JsonSchema {
                json_schema: OpenAiJsonSchema {
                    name: name.clone().unwrap_or_else(|| "output".into()),
                    schema: schema.clone(),
                    strict: true,
                },
            }
        });

        OpenAiRequest {
            model: request.model.clone(),
            messages,
            max_completion_tokens: Some(request.max_tokens),
            temperature: request.temperature,
            top_p: request.top_p,
            stop: request.stop_sequences.clone(),
            tools,
            stream: if stream { Some(true) } else { None },
            response_format,
        }
    }

    fn convert_finish_reason(reason: &str) -> StopReason {
        match reason {
            "stop" => StopReason::EndTurn,
            "length" => StopReason::MaxTokens,
            "tool_calls" => StopReason::ToolUse,
            "content_filter" => StopReason::Refusal,
            _ => StopReason::EndTurn,
        }
    }

    fn convert_response(oai_resp: OpenAiResponse) -> Result<ApiResponse> {
        let choice = oai_resp
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Error::Parse("OpenAI response has no choices".into()))?;

        let mut content = Vec::new();

        if let Some(text) = choice.message.content
            && !text.is_empty()
        {
            content.push(ContentBlock::text(text));
        }

        if let Some(tool_calls) = choice.message.tool_calls {
            for tc in tool_calls {
                let input: Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
                content.push(ContentBlock::ToolUse(ToolUseBlock {
                    id: tc.id,
                    name: tc.function.name,
                    input,
                }));
            }
        }

        let stop_reason = choice
            .finish_reason
            .as_deref()
            .map(Self::convert_finish_reason);

        let usage = oai_resp.usage.map_or(Usage::default(), |u| Usage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            server_tool_use: None,
        });

        Ok(ApiResponse {
            id: oai_resp.id,
            response_type: "message".into(),
            role: "assistant".into(),
            content,
            model: oai_resp.model,
            stop_reason,
            stop_sequence: None,
            usage,
            context_management: None,
        })
    }

    /// Parse streaming chunks and assemble a full `ApiResponse`.
    ///
    /// This is used by `send()` when the adapter needs to make a non-streaming
    /// request, but we still need to parse a non-streaming response. The actual
    /// streaming path returns the raw `reqwest::Response` for the `StreamParser`
    /// to consume.
    async fn parse_non_streaming_response(response: reqwest::Response) -> Result<ApiResponse> {
        let json: Value = response.json().await?;
        let oai_resp: OpenAiResponse =
            serde_json::from_value(json).map_err(|e| Error::Parse(e.to_string()))?;
        Self::convert_response(oai_resp)
    }

    async fn check_error_response(response: reqwest::Response) -> Result<reqwest::Response> {
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body: Value = response.json().await.unwrap_or_else(|_| json!({}));

            // Try to parse OpenAI error format first
            if let Ok(oai_err) = serde_json::from_value::<OpenAiErrorResponse>(body.clone()) {
                return Err(Error::Api {
                    message: oai_err.error.message,
                    status: Some(status),
                    error_type: oai_err.error.error_type,
                });
            }

            // Try Anthropic error format as fallback (for compatible endpoints)
            if let Ok(anthropic_err) = serde_json::from_value::<ErrorResponse>(body.clone()) {
                return Err(anthropic_err.into_error(status));
            }

            // Generic error
            return Err(Error::Api {
                message: body.to_string(),
                status: Some(status),
                error_type: None,
            });
        }
        Ok(response)
    }
}

#[async_trait]
impl ProviderAdapter for OpenAiAdapter {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn name(&self) -> &'static str {
        "openai"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn build_url(&self, _model: &str, _stream: bool) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    async fn transform_request(&self, request: CreateMessageRequest) -> Result<Value> {
        let oai_request = Self::build_openai_request(&request, request.stream == Some(true));
        serde_json::to_value(&oai_request).map_err(|e| Error::InvalidRequest(e.to_string()))
    }

    fn transform_response(&self, response: Value) -> Result<ApiResponse> {
        let oai_resp: OpenAiResponse =
            serde_json::from_value(response).map_err(|e| Error::Parse(e.to_string()))?;
        Self::convert_response(oai_resp)
    }

    fn parse_stream_event(&self, json: &str) -> Option<crate::client::StreamItem> {
        use crate::client::StreamItem;
        use crate::types::{MessageDeltaData, StreamEvent, Usage as StreamUsage};

        let v: Value = serde_json::from_str(json).ok()?;
        let choice = v.get("choices")?.get(0)?;

        // Handle text delta
        if let Some(delta) = choice.get("delta")
            && let Some(content) = delta.get("content").and_then(|c| c.as_str())
            && !content.is_empty()
        {
            return Some(StreamItem::Text(content.to_string()));
        }

        // Handle finish_reason as a stop event
        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            let stop = Self::convert_finish_reason(reason);
            return Some(StreamItem::Event(StreamEvent::MessageDelta {
                delta: MessageDeltaData {
                    stop_reason: Some(stop),
                    stop_sequence: None,
                },
                usage: StreamUsage::default(),
            }));
        }

        None
    }

    async fn apply_auth_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header(
            "Authorization",
            format!("Bearer {}", self.api_key.expose_secret()),
        )
        .header("Content-Type", "application/json")
    }

    async fn send(
        &self,
        http: &reqwest::Client,
        request: CreateMessageRequest,
    ) -> Result<ApiResponse> {
        let url = format!("{}/chat/completions", self.base_url);
        let oai_request = Self::build_openai_request(&request, false);

        let req = self.apply_auth_headers(http.post(&url)).await;
        let response = req.json(&oai_request).send().await?;
        let response = Self::check_error_response(response).await?;

        Self::parse_non_streaming_response(response).await
    }

    async fn send_stream(
        &self,
        http: &reqwest::Client,
        request: CreateMessageRequest,
    ) -> Result<reqwest::Response> {
        let url = format!("{}/chat/completions", self.base_url);
        let oai_request = Self::build_openai_request(&request, true);

        let req = self.apply_auth_headers(http.post(&url)).await;
        let response = req.json(&oai_request).send().await?;
        Self::check_error_response(response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::adapter::{ModelConfig, ProviderConfig};
    use crate::types::{Message, ToolDefinition};

    fn test_config() -> ProviderConfig {
        ProviderConfig::new(ModelConfig::new("gpt-4o", "gpt-4o-mini"))
    }

    #[test]
    fn test_convert_simple_user_message() {
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Hello")]);
        let messages = OpenAiAdapter::convert_messages(&request);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        match &messages[0].content {
            Some(OpenAiContent::Text(t)) => assert_eq!(t, "Hello"),
            other => panic!("Expected text content, got {:?}", other),
        }
    }

    #[test]
    fn test_convert_system_prompt() {
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Hi")])
            .system("You are helpful");
        let messages = OpenAiAdapter::convert_messages(&request);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        match &messages[0].content {
            Some(OpenAiContent::Text(t)) => assert_eq!(t, "You are helpful"),
            other => panic!("Expected system text, got {:?}", other),
        }
        assert_eq!(messages[1].role, "user");
    }

    #[test]
    fn test_convert_assistant_with_tool_use() {
        let tool_use = ToolUseBlock {
            id: "call_123".into(),
            name: "get_weather".into(),
            input: json!({"location": "London"}),
        };
        let assistant_msg = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::text("Let me check the weather."),
                ContentBlock::ToolUse(tool_use),
            ],
        };
        let tool_result_msg =
            Message::tool_results(vec![ToolResultBlock::success("call_123", "Sunny, 22C")]);

        let request = CreateMessageRequest::new("gpt-4o", vec![assistant_msg, tool_result_msg]);
        let messages = OpenAiAdapter::convert_messages(&request);

        // assistant message with text + tool_calls
        assert_eq!(messages[0].role, "assistant");
        assert!(messages[0].content.is_some());
        assert!(messages[0].tool_calls.is_some());
        let tc = messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "call_123");
        assert_eq!(tc[0].function.name, "get_weather");

        // tool result message
        assert_eq!(messages[1].role, "tool");
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call_123"));
        match &messages[1].content {
            Some(OpenAiContent::Text(t)) => assert_eq!(t, "Sunny, 22C"),
            other => panic!("Expected tool result text, got {:?}", other),
        }
    }

    #[test]
    fn test_convert_tools() {
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
        let oai_tools = OpenAiAdapter::convert_tools(&api_tools);

        assert_eq!(oai_tools.len(), 1);
        assert_eq!(oai_tools[0].tool_type, "function");
        assert_eq!(oai_tools[0].function.name, "get_weather");
        assert_eq!(
            oai_tools[0].function.description,
            "Get weather for a location"
        );
    }

    #[test]
    fn test_convert_finish_reason() {
        assert_eq!(
            OpenAiAdapter::convert_finish_reason("stop"),
            StopReason::EndTurn
        );
        assert_eq!(
            OpenAiAdapter::convert_finish_reason("length"),
            StopReason::MaxTokens
        );
        assert_eq!(
            OpenAiAdapter::convert_finish_reason("tool_calls"),
            StopReason::ToolUse
        );
        assert_eq!(
            OpenAiAdapter::convert_finish_reason("content_filter"),
            StopReason::Refusal
        );
        assert_eq!(
            OpenAiAdapter::convert_finish_reason("unknown"),
            StopReason::EndTurn
        );
    }

    #[test]
    fn test_convert_response_text_only() {
        let oai_resp = OpenAiResponse {
            id: "chatcmpl-123".into(),
            model: "gpt-4o".into(),
            choices: vec![OpenAiChoice {
                message: OpenAiResponseMessage {
                    role: "assistant".into(),
                    content: Some("Hello there!".into()),
                    tool_calls: None,
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(OpenAiUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
            }),
        };

        let response = OpenAiAdapter::convert_response(oai_resp).unwrap();
        assert_eq!(response.id, "chatcmpl-123");
        assert_eq!(response.model, "gpt-4o");
        assert_eq!(response.text(), "Hello there!");
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(response.usage.output_tokens, 5);
    }

    #[test]
    fn test_convert_response_with_tool_calls() {
        let oai_resp = OpenAiResponse {
            id: "chatcmpl-456".into(),
            model: "gpt-4o".into(),
            choices: vec![OpenAiChoice {
                message: OpenAiResponseMessage {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![OpenAiToolCall {
                        id: "call_abc".into(),
                        call_type: "function".into(),
                        function: OpenAiFunctionCall {
                            name: "get_weather".into(),
                            arguments: r#"{"location":"Paris"}"#.into(),
                        },
                    }]),
                },
                finish_reason: Some("tool_calls".into()),
            }],
            usage: Some(OpenAiUsage {
                prompt_tokens: 20,
                completion_tokens: 15,
            }),
        };

        let response = OpenAiAdapter::convert_response(oai_resp).unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        let tool_uses = response.tool_uses();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "get_weather");
        assert_eq!(tool_uses[0].id, "call_abc");
        assert_eq!(tool_uses[0].input["location"], "Paris");
    }

    #[test]
    fn test_build_openai_request() {
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Hello")])
            .max_tokens(1000)
            .temperature(0.7);

        let oai_req = OpenAiAdapter::build_openai_request(&request, false);
        assert_eq!(oai_req.model, "gpt-4o");
        assert_eq!(oai_req.max_completion_tokens, Some(1000));
        assert_eq!(oai_req.temperature, Some(0.7));
        assert!(oai_req.stream.is_none());
        assert!(oai_req.tools.is_none());
    }

    #[test]
    fn test_build_openai_request_streaming() {
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Hello")]);
        let oai_req = OpenAiAdapter::build_openai_request(&request, true);
        assert_eq!(oai_req.stream, Some(true));
    }

    #[tokio::test]
    async fn test_build_url() {
        let adapter = OpenAiAdapter::new(test_config());
        let url = adapter.build_url("gpt-4o", false).await;
        assert!(url.ends_with("/chat/completions"));
    }

    #[tokio::test]
    async fn test_transform_request() {
        let adapter = OpenAiAdapter::new(test_config());
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Hello")]);
        let body = adapter.transform_request(request).await.unwrap();
        assert_eq!(body["model"], "gpt-4o");
        assert!(body["messages"].is_array());
    }

    #[test]
    fn test_transform_response() {
        let adapter = OpenAiAdapter::new(test_config());
        let json = json!({
            "id": "chatcmpl-test",
            "model": "gpt-4o",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hi!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 2
            }
        });
        let response = adapter.transform_response(json).unwrap();
        assert_eq!(response.text(), "Hi!");
    }

    #[test]
    fn test_convert_tool_result_error() {
        let tool_result =
            Message::tool_results(vec![ToolResultBlock::error("call_err", "File not found")]);
        let request = CreateMessageRequest::new("gpt-4o", vec![tool_result]);
        let messages = OpenAiAdapter::convert_messages(&request);

        assert_eq!(messages[0].role, "tool");
        match &messages[0].content {
            Some(OpenAiContent::Text(t)) => assert!(t.contains("[Error]")),
            other => panic!("Expected error text, got {:?}", other),
        }
    }

    #[test]
    fn test_custom_base_url() {
        let adapter = OpenAiAdapter::new(test_config()).base_url("http://localhost:11434/v1");
        assert_eq!(
            ProviderAdapter::base_url(&adapter),
            "http://localhost:11434/v1"
        );
    }
}
