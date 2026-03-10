//! Message request and response types.

use serde::{Deserialize, Serialize};

use super::config::{
    DEFAULT_MAX_TOKENS, EffortLevel, MAX_TOKENS_128K, MIN_MAX_TOKENS, OutputConfig, OutputFormat,
    ThinkingConfig, TokenValidationError, ToolChoice,
};
use super::context::ContextManagement;
use super::types::{ApiTool, RequestMetadata};
use crate::types::{
    Message, SystemPrompt, ToolDefinition, ToolSearchTool, WebFetchTool, WebSearchTool,
};

#[derive(Debug, Clone, Serialize)]
pub struct CreateMessageRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ApiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<RequestMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_management: Option<ContextManagement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
}

impl CreateMessageRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
            messages,
            system: None,
            tools: None,
            tool_choice: None,
            stream: None,
            stop_sequences: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            thinking: None,
            output_format: None,
            context_management: None,
            output_config: None,
        }
    }

    pub fn validate(&self) -> Result<(), TokenValidationError> {
        if self.max_tokens < MIN_MAX_TOKENS {
            return Err(TokenValidationError::MaxTokensTooLow {
                min: MIN_MAX_TOKENS,
                actual: self.max_tokens,
            });
        }
        if self.max_tokens > MAX_TOKENS_128K {
            return Err(TokenValidationError::MaxTokensTooHigh {
                max: MAX_TOKENS_128K,
                actual: self.max_tokens,
            });
        }
        if let Some(thinking) = &self.thinking {
            thinking.validate_against_max_tokens(self.max_tokens)?;
        }
        Ok(())
    }

    pub fn requires_128k_beta(&self) -> bool {
        self.max_tokens > DEFAULT_MAX_TOKENS
    }

    pub fn system(mut self, system: impl Into<SystemPrompt>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        let api_tools: Vec<ApiTool> = tools.into_iter().map(ApiTool::Custom).collect();
        self.tools = Some(api_tools);
        self
    }

    pub fn web_search(mut self, config: WebSearchTool) -> Self {
        let mut tools = self.tools.unwrap_or_default();
        tools.push(ApiTool::WebSearch(config));
        self.tools = Some(tools);
        self
    }

    pub fn web_fetch(mut self, config: WebFetchTool) -> Self {
        let mut tools = self.tools.unwrap_or_default();
        tools.push(ApiTool::WebFetch(config));
        self.tools = Some(tools);
        self
    }

    pub fn tool_search(mut self, config: ToolSearchTool) -> Self {
        let mut tools = self.tools.unwrap_or_default();
        tools.push(ApiTool::ToolSearch(config));
        self.tools = Some(tools);
        self
    }

    pub fn api_tools(mut self, tools: Vec<ApiTool>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn tool_choice(mut self, choice: ToolChoice) -> Self {
        self.tool_choice = Some(choice);
        self
    }

    pub fn tool_choice_auto(mut self) -> Self {
        self.tool_choice = Some(ToolChoice::Auto);
        self
    }

    pub fn tool_choice_any(mut self) -> Self {
        self.tool_choice = Some(ToolChoice::Any);
        self
    }

    pub fn tool_choice_none(mut self) -> Self {
        self.tool_choice = Some(ToolChoice::None);
        self
    }

    pub fn required_tool(mut self, name: impl Into<String>) -> Self {
        self.tool_choice = Some(ToolChoice::tool(name));
        self
    }

    pub fn stream(mut self) -> Self {
        self.stream = Some(true);
        self
    }

    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn top_p(mut self, top_p: f32) -> Self {
        self.top_p = Some(top_p);
        self
    }

    pub fn top_k(mut self, top_k: u32) -> Self {
        self.top_k = Some(top_k);
        self
    }

    pub fn stop_sequences(mut self, sequences: Vec<String>) -> Self {
        self.stop_sequences = Some(sequences);
        self
    }

    pub fn thinking(mut self, config: ThinkingConfig) -> Self {
        self.thinking = Some(config);
        self
    }

    pub fn extended_thinking(mut self, budget_tokens: u32) -> Self {
        self.thinking = Some(ThinkingConfig::enabled(budget_tokens));
        self
    }

    pub fn output_format(mut self, format: OutputFormat) -> Self {
        self.output_format = Some(format);
        self
    }

    /// Set JSON schema for structured output.
    ///
    /// Automatically transforms the schema for strict mode compatibility:
    /// - Adds `additionalProperties: false` to all objects
    /// - Removes unsupported constraints (minimum, maximum, minLength, maxLength, etc.)
    /// - Ensures `required` fields are present
    pub fn json_schema(mut self, schema: serde_json::Value) -> Self {
        let strict_schema = crate::client::schema::transform_for_strict(schema);
        self.output_format = Some(OutputFormat::json_schema(strict_schema));
        self
    }

    pub fn context_management(mut self, management: ContextManagement) -> Self {
        self.context_management = Some(management);
        self
    }

    pub fn effort(mut self, level: EffortLevel) -> Self {
        self.output_config = Some(OutputConfig::effort(level));
        self
    }

    pub fn output_config(mut self, config: OutputConfig) -> Self {
        self.output_config = Some(config);
        self
    }

    pub fn metadata(mut self, metadata: RequestMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

impl From<String> for SystemPrompt {
    fn from(s: String) -> Self {
        SystemPrompt::Text(s)
    }
}

impl From<&str> for SystemPrompt {
    fn from(s: &str) -> Self {
        SystemPrompt::Text(s.to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CountTokensRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemPrompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ApiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
}

impl CountTokensRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            system: None,
            tools: None,
            thinking: None,
        }
    }

    pub fn system(mut self, system: impl Into<SystemPrompt>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = Some(tools.into_iter().map(ApiTool::Custom).collect());
        self
    }

    pub fn api_tools(mut self, tools: Vec<ApiTool>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn thinking(mut self, config: ThinkingConfig) -> Self {
        self.thinking = Some(config);
        self
    }

    pub fn from_message_request(request: &CreateMessageRequest) -> Self {
        Self {
            model: request.model.clone(),
            messages: request.messages.clone(),
            system: request.system.clone(),
            tools: request.tools.clone(),
            thinking: request.thinking.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CountTokensResponse {
    pub input_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_management: Option<CountTokensContextManagement>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct CountTokensContextManagement {
    #[serde(default)]
    pub original_input_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::super::config::MIN_THINKING_BUDGET;
    use super::*;

    #[test]
    fn test_create_request_default_max_tokens() {
        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hello")]);
        assert_eq!(request.max_tokens, DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn test_create_request() {
        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hello")])
            .max_tokens(1000)
            .temperature(0.7);

        assert_eq!(request.model, "claude-sonnet-4-5");
        assert_eq!(request.max_tokens, 1000);
        assert_eq!(request.temperature, Some(0.7));
    }

    #[test]
    fn test_request_validate_valid() {
        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .max_tokens(4000)
            .extended_thinking(2000);
        assert!(request.validate().is_ok());
    }

    #[test]
    fn test_request_validate_max_tokens_too_high() {
        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .max_tokens(MAX_TOKENS_128K + 1);
        let err = request.validate().unwrap_err();
        assert!(matches!(err, TokenValidationError::MaxTokensTooHigh { .. }));
    }

    #[test]
    fn test_request_validate_thinking_auto_clamp() {
        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .extended_thinking(500);
        assert_eq!(
            request.thinking.as_ref().unwrap().budget(),
            Some(MIN_THINKING_BUDGET)
        );
        assert!(request.validate().is_ok());
    }

    #[test]
    fn test_request_validate_thinking_exceeds_max() {
        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .max_tokens(2000)
            .extended_thinking(MIN_THINKING_BUDGET);
        assert!(request.validate().is_ok());

        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .max_tokens(MIN_THINKING_BUDGET)
            .extended_thinking(MIN_THINKING_BUDGET);
        let err = request.validate().unwrap_err();
        assert!(matches!(
            err,
            TokenValidationError::ThinkingBudgetExceedsMaxTokens { .. }
        ));
    }

    #[test]
    fn test_request_requires_128k_beta() {
        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")]);
        assert!(!request.requires_128k_beta());

        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .max_tokens(DEFAULT_MAX_TOKENS + 1);
        assert!(request.requires_128k_beta());

        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .max_tokens(MAX_TOKENS_128K);
        assert!(request.requires_128k_beta());
    }

    #[test]
    fn test_count_tokens_request() {
        let request = CountTokensRequest::new("claude-sonnet-4-5", vec![Message::user("Hello")])
            .system("You are a helpful assistant");

        assert_eq!(request.model, "claude-sonnet-4-5");
        assert!(request.system.is_some());
    }

    #[test]
    fn test_count_tokens_from_message_request() {
        let msg_request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .system("System prompt")
            .extended_thinking(10000);

        let count_request = CountTokensRequest::from_message_request(&msg_request);

        assert_eq!(count_request.model, msg_request.model);
        assert_eq!(count_request.messages.len(), msg_request.messages.len());
        assert!(count_request.system.is_some());
        assert!(count_request.thinking.is_some());
    }

    #[test]
    fn test_request_with_effort() {
        let request = CreateMessageRequest::new("claude-opus-4-6", vec![Message::user("Hi")])
            .effort(EffortLevel::Medium);
        assert!(request.output_config.is_some());
        assert_eq!(
            request.output_config.unwrap().effort,
            Some(EffortLevel::Medium)
        );
    }

    #[test]
    fn test_request_with_context_management() {
        let mgmt = ContextManagement::new().edit(ContextManagement::clear_thinking(2));
        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .context_management(mgmt);
        assert!(request.context_management.is_some());
    }

    #[test]
    fn test_request_with_tool_choice() {
        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .tool_choice_any();
        assert_eq!(request.tool_choice, Some(ToolChoice::Any));

        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hi")])
            .required_tool("Grep");
        assert_eq!(request.tool_choice, Some(ToolChoice::tool("Grep")));
    }
}
