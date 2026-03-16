//! Google Gemini API adapter.
//!
//! Supports Google's Gemini `generateContent` API format, enabling
//! Gemini 2.0 Flash, Gemini 2.5 Pro, and other Gemini models.

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::config::ProviderConfig;
use super::traits::ProviderAdapter;
use crate::client::messages::{ApiTool, CreateMessageRequest, ErrorResponse};
use crate::types::{
    ApiResponse, ContentBlock, Message, Role, StopReason, ToolResultBlock, ToolResultContent,
    ToolUseBlock, Usage,
};
use crate::{Error, Result};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

// -- Gemini request types -------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiSystemInstruction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiToolDeclaration>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiSystemInstruction {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum GeminiPart {
    Text {
        text: String,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: GeminiFunctionResponse,
    },
    InlineData {
        #[serde(rename = "inlineData")]
        inline_data: GeminiInlineData,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiFunctionResponse {
    name: String,
    response: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiInlineData {
    mime_type: String,
    data: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    /// Set to "application/json" for structured output.
    #[serde(skip_serializing_if = "Option::is_none")]
    response_mime_type: Option<String>,
    /// JSON schema for structured output (requires response_mime_type = "application/json").
    #[serde(skip_serializing_if = "Option::is_none")]
    response_schema: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiToolDeclaration {
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Clone, Serialize)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    parameters: Value,
}

// -- Gemini response types ------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    usage_metadata: Option<GeminiUsageMetadata>,
    #[allow(dead_code)]
    model_version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    prompt_token_count: Option<u32>,
    candidates_token_count: Option<u32>,
    #[allow(dead_code)]
    total_token_count: Option<u32>,
}

// -- Gemini error response ------------------------------------------------

#[derive(Debug, Deserialize)]
struct GeminiErrorResponse {
    error: GeminiErrorDetail,
}

#[derive(Debug, Deserialize)]
struct GeminiErrorDetail {
    message: String,
    #[allow(dead_code)]
    status: Option<String>,
    #[allow(dead_code)]
    code: Option<u32>,
}

// -- Adapter implementation -----------------------------------------------

pub struct GeminiAdapter {
    config: ProviderConfig,
    api_key: SecretString,
    base_url: String,
    /// When true, the api_key is treated as an OAuth Bearer token instead of
    /// an API key query parameter.
    use_oauth: bool,
}

impl std::fmt::Debug for GeminiAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiAdapter")
            .field("config", &self.config)
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl GeminiAdapter {
    pub fn new(config: ProviderConfig) -> Self {
        let use_oauth = std::env::var("GEMINI_USE_OAUTH").is_ok();
        Self {
            config,
            api_key: Self::api_key_from_env(),
            base_url: Self::base_url_from_env(),
            use_oauth,
        }
    }

    pub fn from_api_key(config: ProviderConfig, api_key: impl Into<String>) -> Self {
        Self {
            config,
            api_key: SecretString::from(api_key.into()),
            base_url: Self::base_url_from_env(),
            use_oauth: false,
        }
    }

    /// Configure this adapter to use OAuth Bearer tokens instead of API key
    /// query parameters for authentication.
    pub fn oauth(mut self) -> Self {
        self.use_oauth = true;
        self
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
        let key = std::env::var("GEMINI_API_KEY")
            .or_else(|_| std::env::var("GOOGLE_API_KEY"))
            .unwrap_or_default();
        SecretString::from(key)
    }

    fn base_url_from_env() -> String {
        std::env::var("GEMINI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.into())
    }

    // -- Format conversions ---------------------------------------------------

    fn convert_messages(request: &CreateMessageRequest) -> Vec<GeminiContent> {
        let mut contents = Vec::new();

        for msg in &request.messages {
            match msg.role {
                Role::User => {
                    Self::convert_user_message(msg, &mut contents);
                }
                Role::Assistant => {
                    Self::convert_assistant_message(msg, &mut contents);
                }
            }
        }

        contents
    }

    fn convert_user_message(msg: &Message, out: &mut Vec<GeminiContent>) {
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
            // Emit function responses for tool results
            let mut parts = Vec::new();
            for tr in &tool_results {
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

                let content_text = if tr.is_error == Some(true) {
                    format!("[Error] {}", content_text)
                } else {
                    content_text
                };

                parts.push(GeminiPart::FunctionResponse {
                    function_response: GeminiFunctionResponse {
                        name: tr.tool_use_id.clone(),
                        response: json!({ "result": content_text }),
                    },
                });
            }

            // Also add any non-tool-result text content
            for block in &msg.content {
                if let Some(text) = block.as_text() {
                    parts.push(GeminiPart::Text {
                        text: text.to_string(),
                    });
                }
            }

            if !parts.is_empty() {
                out.push(GeminiContent {
                    role: "user".into(),
                    parts,
                });
            }
        } else {
            // Regular user message
            let mut parts = Vec::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text, .. } => {
                        parts.push(GeminiPart::Text { text: text.clone() });
                    }
                    ContentBlock::Image {
                        source: crate::types::ImageSource::Base64 { media_type, data },
                    } => {
                        parts.push(GeminiPart::InlineData {
                            inline_data: GeminiInlineData {
                                mime_type: media_type.clone(),
                                data: data.clone(),
                            },
                        });
                    }
                    _ => {}
                }
            }

            if !parts.is_empty() {
                out.push(GeminiContent {
                    role: "user".into(),
                    parts,
                });
            }
        }
    }

    fn convert_assistant_message(msg: &Message, out: &mut Vec<GeminiContent>) {
        let mut parts = Vec::new();

        for block in &msg.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    parts.push(GeminiPart::Text { text: text.clone() });
                }
                ContentBlock::ToolUse(tu) => {
                    parts.push(GeminiPart::FunctionCall {
                        function_call: GeminiFunctionCall {
                            name: tu.name.clone(),
                            args: tu.input.clone(),
                        },
                    });
                }
                // Skip thinking blocks, redacted thinking, etc.
                _ => {}
            }
        }

        if !parts.is_empty() {
            out.push(GeminiContent {
                role: "model".into(),
                parts,
            });
        }
    }

    fn convert_system_instruction(
        request: &CreateMessageRequest,
    ) -> Option<GeminiSystemInstruction> {
        request.system.as_ref().and_then(|system| {
            let text = system.as_text();
            if text.is_empty() {
                None
            } else {
                Some(GeminiSystemInstruction {
                    parts: vec![GeminiPart::Text { text }],
                })
            }
        })
    }

    fn convert_tools(tools: &[ApiTool]) -> Vec<GeminiToolDeclaration> {
        let declarations: Vec<GeminiFunctionDeclaration> = tools
            .iter()
            .filter_map(|tool| match tool {
                ApiTool::Custom(def) => Some(GeminiFunctionDeclaration {
                    name: def.name.clone(),
                    description: def.description.clone(),
                    parameters: def.input_schema.clone(),
                }),
                // Skip server-side tools as they are Anthropic-specific
                _ => None,
            })
            .collect();

        if declarations.is_empty() {
            Vec::new()
        } else {
            vec![GeminiToolDeclaration {
                function_declarations: declarations,
            }]
        }
    }

    fn build_gemini_request(request: &CreateMessageRequest) -> GeminiRequest {
        let contents = Self::convert_messages(request);
        let system_instruction = Self::convert_system_instruction(request);
        let tools = request
            .tools
            .as_ref()
            .map(|t| Self::convert_tools(t))
            .filter(|t| !t.is_empty());

        // Map structured output to Gemini's responseMimeType + responseSchema
        let (response_mime_type, response_schema) =
            if let Some(crate::client::messages::OutputFormat::JsonSchema { ref schema, .. }) =
                request.output_format
            {
                (Some("application/json".to_string()), Some(schema.clone()))
            } else {
                (None, None)
            };

        let generation_config = Some(GeminiGenerationConfig {
            max_output_tokens: Some(request.max_tokens),
            temperature: request.temperature,
            top_p: request.top_p,
            stop_sequences: request.stop_sequences.clone(),
            response_mime_type,
            response_schema,
        });

        GeminiRequest {
            contents,
            system_instruction,
            tools,
            generation_config,
        }
    }

    fn convert_finish_reason(reason: &str, has_function_calls: bool) -> StopReason {
        if has_function_calls {
            return StopReason::ToolUse;
        }
        match reason {
            "STOP" => StopReason::EndTurn,
            "MAX_TOKENS" => StopReason::MaxTokens,
            "SAFETY" => StopReason::Refusal,
            "RECITATION" => StopReason::Refusal,
            _ => StopReason::EndTurn,
        }
    }

    fn convert_response(gemini_resp: GeminiResponse, model: &str) -> Result<ApiResponse> {
        let candidate = gemini_resp
            .candidates
            .and_then(|mut c| {
                if c.is_empty() {
                    None
                } else {
                    Some(c.remove(0))
                }
            })
            .ok_or_else(|| Error::Parse("Gemini response has no candidates".into()))?;

        let mut content = Vec::new();
        let mut has_function_calls = false;

        if let Some(candidate_content) = candidate.content {
            for part in candidate_content.parts {
                match part {
                    GeminiPart::Text { text } => {
                        if !text.is_empty() {
                            content.push(ContentBlock::text(text));
                        }
                    }
                    GeminiPart::FunctionCall { function_call } => {
                        has_function_calls = true;
                        content.push(ContentBlock::ToolUse(ToolUseBlock {
                            id: format!("call_{}", uuid::Uuid::new_v4().simple()),
                            name: function_call.name,
                            input: function_call.args,
                        }));
                    }
                    // Function responses and inline data are not expected in responses
                    _ => {}
                }
            }
        }

        let stop_reason = candidate
            .finish_reason
            .as_deref()
            .map(|r| Self::convert_finish_reason(r, has_function_calls));

        let usage = gemini_resp
            .usage_metadata
            .map_or(Usage::default(), |u| Usage {
                input_tokens: u.prompt_token_count.unwrap_or(0),
                output_tokens: u.candidates_token_count.unwrap_or(0),
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
                server_tool_use: None,
            });

        // Gemini doesn't return an ID, so we generate one
        let id = format!("gemini-{}", uuid::Uuid::new_v4().simple());

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

    async fn parse_non_streaming_response(
        response: reqwest::Response,
        model: &str,
    ) -> Result<ApiResponse> {
        let json: Value = response.json().await?;
        let gemini_resp: GeminiResponse =
            serde_json::from_value(json).map_err(|e| Error::Parse(e.to_string()))?;
        Self::convert_response(gemini_resp, model)
    }

    async fn check_error_response(response: reqwest::Response) -> Result<reqwest::Response> {
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body: Value = response.json().await.unwrap_or_else(|_| json!({}));

            // Try to parse Gemini error format first
            if let Ok(gemini_err) = serde_json::from_value::<GeminiErrorResponse>(body.clone()) {
                return Err(Error::Api {
                    message: gemini_err.error.message,
                    status: Some(status),
                    error_type: gemini_err.error.status,
                });
            }

            // Try Anthropic error format as fallback
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

    fn build_url_with_auth(&self, model: &str, stream: bool) -> String {
        let action = if stream {
            "streamGenerateContent?alt=sse"
        } else {
            "generateContent"
        };

        let api_key = self.api_key.expose_secret();
        if api_key.is_empty() || self.use_oauth {
            // OAuth mode or no key: auth via header, no key in URL
            format!("{}/models/{}:{}", self.base_url, model, action)
        } else {
            // API key mode: append as query parameter
            let separator = if stream { "&" } else { "?" };
            format!(
                "{}/models/{}:{}{}key={}",
                self.base_url, model, action, separator, api_key
            )
        }
    }
}

#[async_trait]
impl ProviderAdapter for GeminiAdapter {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn name(&self) -> &'static str {
        "gemini"
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn build_url(&self, model: &str, stream: bool) -> String {
        // Return URL without API key for safe logging/display.
        // The key is added internally by build_url_with_auth in send/send_stream.
        let action = if stream {
            "streamGenerateContent?alt=sse"
        } else {
            "generateContent"
        };
        format!("{}/models/{}:{}", self.base_url, model, action)
    }

    async fn transform_request(&self, request: CreateMessageRequest) -> Result<Value> {
        let gemini_request = Self::build_gemini_request(&request);
        serde_json::to_value(&gemini_request).map_err(|e| Error::InvalidRequest(e.to_string()))
    }

    fn transform_response(&self, response: Value) -> Result<ApiResponse> {
        let gemini_resp: GeminiResponse =
            serde_json::from_value(response).map_err(|e| Error::Parse(e.to_string()))?;
        Self::convert_response(gemini_resp, "gemini")
    }

    fn parse_stream_event(&self, json: &str) -> Option<crate::client::StreamItem> {
        use crate::client::StreamItem;

        let v: Value = serde_json::from_str(json).ok()?;

        // Gemini streaming sends candidates array
        let candidate = v.get("candidates")?.get(0)?;
        let content = candidate.get("content")?;
        let parts = content.get("parts")?.as_array()?;

        for part in parts {
            if let Some(text) = part.get("text").and_then(|t| t.as_str())
                && !text.is_empty()
            {
                return Some(StreamItem::Text(text.to_string()));
            }
            if let Some(fc) = part.get("functionCall") {
                let name = fc.get("name")?.as_str()?;
                let args = fc.get("args").cloned().unwrap_or(json!({}));
                return Some(StreamItem::ToolUseComplete(ToolUseBlock {
                    id: format!("call_{}", uuid::Uuid::new_v4().simple()),
                    name: name.to_string(),
                    input: args,
                }));
            }
        }

        None
    }

    async fn apply_auth_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let api_key = self.api_key.expose_secret();
        if self.use_oauth && !api_key.is_empty() {
            // OAuth mode: send as Bearer token header
            req.header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
        } else {
            // API key mode: auth is in the URL query parameter (added by build_url_with_auth)
            req.header("Content-Type", "application/json")
        }
    }

    async fn send(
        &self,
        http: &reqwest::Client,
        request: CreateMessageRequest,
    ) -> Result<ApiResponse> {
        let model = request.model.clone();
        let url = self.build_url_with_auth(&model, false);
        let gemini_request = Self::build_gemini_request(&request);

        let req = self.apply_auth_headers(http.post(&url)).await;
        let response = req.json(&gemini_request).send().await?;
        let response = Self::check_error_response(response).await?;

        Self::parse_non_streaming_response(response, &model).await
    }

    async fn send_stream(
        &self,
        http: &reqwest::Client,
        request: CreateMessageRequest,
    ) -> Result<reqwest::Response> {
        let model = request.model.clone();
        let url = self.build_url_with_auth(&model, true);
        let gemini_request = Self::build_gemini_request(&request);

        let req = self.apply_auth_headers(http.post(&url)).await;
        let response = req.json(&gemini_request).send().await?;
        Self::check_error_response(response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::adapter::{ModelConfig, ProviderConfig};
    use crate::types::{Message, ToolDefinition};

    fn test_config() -> ProviderConfig {
        ProviderConfig::new(ModelConfig::new(
            "gemini-2.0-flash",
            "gemini-2.0-flash-lite",
        ))
    }

    #[test]
    fn test_convert_simple_user_message() {
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")]);
        let contents = GeminiAdapter::convert_messages(&request);

        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
        assert_eq!(contents[0].parts.len(), 1);
        match &contents[0].parts[0] {
            GeminiPart::Text { text } => assert_eq!(text, "Hello"),
            other => panic!("Expected text part, got {:?}", other),
        }
    }

    #[test]
    fn test_convert_system_prompt() {
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hi")])
            .system("You are helpful");
        let system = GeminiAdapter::convert_system_instruction(&request);

        assert!(system.is_some());
        let system = system.unwrap();
        assert_eq!(system.parts.len(), 1);
        match &system.parts[0] {
            GeminiPart::Text { text } => assert_eq!(text, "You are helpful"),
            other => panic!("Expected text part, got {:?}", other),
        }

        // System prompt should NOT be in contents
        let contents = GeminiAdapter::convert_messages(&request);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
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

        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![assistant_msg]);
        let contents = GeminiAdapter::convert_messages(&request);

        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "model");
        assert_eq!(contents[0].parts.len(), 2);

        match &contents[0].parts[0] {
            GeminiPart::Text { text } => assert_eq!(text, "Let me check the weather."),
            other => panic!("Expected text part, got {:?}", other),
        }
        match &contents[0].parts[1] {
            GeminiPart::FunctionCall { function_call } => {
                assert_eq!(function_call.name, "get_weather");
                assert_eq!(function_call.args["location"], "London");
            }
            other => panic!("Expected function call part, got {:?}", other),
        }
    }

    #[test]
    fn test_convert_tool_result() {
        let tool_result_msg =
            Message::tool_results(vec![ToolResultBlock::success("call_123", "Sunny, 22C")]);

        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![tool_result_msg]);
        let contents = GeminiAdapter::convert_messages(&request);

        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
        assert_eq!(contents[0].parts.len(), 1);
        match &contents[0].parts[0] {
            GeminiPart::FunctionResponse { function_response } => {
                assert_eq!(function_response.name, "call_123");
                assert_eq!(function_response.response["result"], "Sunny, 22C");
            }
            other => panic!("Expected function response part, got {:?}", other),
        }
    }

    #[test]
    fn test_convert_tool_result_error() {
        let tool_result =
            Message::tool_results(vec![ToolResultBlock::error("call_err", "File not found")]);
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![tool_result]);
        let contents = GeminiAdapter::convert_messages(&request);

        assert_eq!(contents.len(), 1);
        match &contents[0].parts[0] {
            GeminiPart::FunctionResponse { function_response } => {
                let result = function_response.response["result"].as_str().unwrap();
                assert!(result.contains("[Error]"));
                assert!(result.contains("File not found"));
            }
            other => panic!("Expected function response part, got {:?}", other),
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
        let gemini_tools = GeminiAdapter::convert_tools(&api_tools);

        assert_eq!(gemini_tools.len(), 1);
        assert_eq!(gemini_tools[0].function_declarations.len(), 1);
        assert_eq!(gemini_tools[0].function_declarations[0].name, "get_weather");
        assert_eq!(
            gemini_tools[0].function_declarations[0].description,
            "Get weather for a location"
        );
    }

    #[test]
    fn test_convert_finish_reason() {
        assert_eq!(
            GeminiAdapter::convert_finish_reason("STOP", false),
            StopReason::EndTurn
        );
        assert_eq!(
            GeminiAdapter::convert_finish_reason("MAX_TOKENS", false),
            StopReason::MaxTokens
        );
        assert_eq!(
            GeminiAdapter::convert_finish_reason("SAFETY", false),
            StopReason::Refusal
        );
        assert_eq!(
            GeminiAdapter::convert_finish_reason("RECITATION", false),
            StopReason::Refusal
        );
        // When function calls are present, always return ToolUse
        assert_eq!(
            GeminiAdapter::convert_finish_reason("STOP", true),
            StopReason::ToolUse
        );
        assert_eq!(
            GeminiAdapter::convert_finish_reason("unknown", false),
            StopReason::EndTurn
        );
    }

    #[test]
    fn test_convert_response_text_only() {
        let gemini_resp = GeminiResponse {
            candidates: Some(vec![GeminiCandidate {
                content: Some(GeminiContent {
                    role: "model".into(),
                    parts: vec![GeminiPart::Text {
                        text: "Hello there!".into(),
                    }],
                }),
                finish_reason: Some("STOP".into()),
            }]),
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: Some(10),
                candidates_token_count: Some(5),
                total_token_count: Some(15),
            }),
            model_version: None,
        };

        let response = GeminiAdapter::convert_response(gemini_resp, "gemini-2.0-flash").unwrap();
        assert_eq!(response.model, "gemini-2.0-flash");
        assert_eq!(response.text(), "Hello there!");
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(response.usage.output_tokens, 5);
    }

    #[test]
    fn test_convert_response_with_function_calls() {
        let gemini_resp = GeminiResponse {
            candidates: Some(vec![GeminiCandidate {
                content: Some(GeminiContent {
                    role: "model".into(),
                    parts: vec![GeminiPart::FunctionCall {
                        function_call: GeminiFunctionCall {
                            name: "get_weather".into(),
                            args: json!({"location": "Paris"}),
                        },
                    }],
                }),
                finish_reason: Some("STOP".into()),
            }]),
            usage_metadata: Some(GeminiUsageMetadata {
                prompt_token_count: Some(20),
                candidates_token_count: Some(15),
                total_token_count: Some(35),
            }),
            model_version: None,
        };

        let response = GeminiAdapter::convert_response(gemini_resp, "gemini-2.0-flash").unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        let tool_uses = response.tool_uses();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "get_weather");
        assert_eq!(tool_uses[0].input["location"], "Paris");
    }

    #[test]
    fn test_build_gemini_request() {
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")])
            .max_tokens(1000)
            .temperature(0.7);

        let gemini_req = GeminiAdapter::build_gemini_request(&request);
        assert_eq!(gemini_req.contents.len(), 1);
        assert!(gemini_req.system_instruction.is_none());
        assert!(gemini_req.tools.is_none());

        let gen_config = gemini_req.generation_config.unwrap();
        assert_eq!(gen_config.max_output_tokens, Some(1000));
        assert_eq!(gen_config.temperature, Some(0.7));
    }

    #[test]
    fn test_build_gemini_request_with_system() {
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")])
            .system("Be helpful");

        let gemini_req = GeminiAdapter::build_gemini_request(&request);
        assert!(gemini_req.system_instruction.is_some());
        let system = gemini_req.system_instruction.unwrap();
        match &system.parts[0] {
            GeminiPart::Text { text } => assert_eq!(text, "Be helpful"),
            other => panic!("Expected text part, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_build_url() {
        let adapter = GeminiAdapter::new(test_config());
        let url = adapter.build_url("gemini-2.0-flash", false).await;
        assert!(url.contains("/models/gemini-2.0-flash:generateContent"));
    }

    #[tokio::test]
    async fn test_build_url_streaming() {
        let adapter = GeminiAdapter::new(test_config());
        let url = adapter.build_url("gemini-2.0-flash", true).await;
        assert!(url.contains("/models/gemini-2.0-flash:streamGenerateContent"));
        assert!(url.contains("alt=sse"));
    }

    #[tokio::test]
    async fn test_transform_request() {
        let adapter = GeminiAdapter::new(test_config());
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")]);
        let body = adapter.transform_request(request).await.unwrap();
        assert!(body["contents"].is_array());
        assert!(body["generationConfig"].is_object());
    }

    #[test]
    fn test_transform_response() {
        let adapter = GeminiAdapter::new(test_config());
        let json = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "Hi!"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 2,
                "totalTokenCount": 7
            }
        });
        let response = adapter.transform_response(json).unwrap();
        assert_eq!(response.text(), "Hi!");
    }

    #[test]
    fn test_convert_no_candidates_error() {
        let gemini_resp = GeminiResponse {
            candidates: Some(vec![]),
            usage_metadata: None,
            model_version: None,
        };

        let result = GeminiAdapter::convert_response(gemini_resp, "gemini-2.0-flash");
        assert!(result.is_err());
    }

    #[test]
    fn test_assistant_role_mapped_to_model() {
        let msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::text("Response")],
        };
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![msg]);
        let contents = GeminiAdapter::convert_messages(&request);

        assert_eq!(contents[0].role, "model");
    }

    #[test]
    fn test_custom_base_url() {
        let adapter = GeminiAdapter::new(test_config()).base_url("http://localhost:8080/v1beta");
        assert_eq!(
            ProviderAdapter::base_url(&adapter),
            "http://localhost:8080/v1beta"
        );
    }
}
