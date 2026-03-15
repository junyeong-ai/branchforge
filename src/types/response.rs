//! API response types.

use std::collections::HashMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::ContentBlock;
use super::citations::{Citation, SearchResultLocationCitation};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn context_usage(&self) -> u64 {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }

    pub fn add(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
    }

    pub fn add_usage(&mut self, usage: &Usage) {
        self.input_tokens += usage.input_tokens as u64;
        self.output_tokens += usage.output_tokens as u64;
        self.cache_read_input_tokens += usage.cache_read_input_tokens.unwrap_or(0) as u64;
        self.cache_creation_input_tokens += usage.cache_creation_input_tokens.unwrap_or(0) as u64;
    }

    pub fn cache_hit_rate(&self) -> f64 {
        if self.input_tokens == 0 {
            return 0.0;
        }
        (self.cache_read_input_tokens as f64 / self.input_tokens as f64).clamp(0.0, 1.0)
    }
}

impl From<&Usage> for TokenUsage {
    fn from(usage: &Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens as u64,
            output_tokens: usage.output_tokens as u64,
            cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0) as u64,
            cache_creation_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0) as u64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub response_type: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<StopReason>,
    pub stop_sequence: Option<String>,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_management: Option<ContextManagementResponse>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextManagementResponse {
    #[serde(default)]
    pub applied_edits: Vec<AppliedEdit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedEdit {
    #[serde(rename = "type")]
    pub edit_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_tool_uses: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_thinking_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    /// Model refused to generate output due to safety reasons.
    /// When using structured outputs, the response may not match the schema.
    Refusal,
}

/// Server-side tool usage from API response.
///
/// This is returned in the `usage.server_tool_use` field when server-side
/// tools (web search, web fetch) are used by the API.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ServerToolUseUsage {
    /// Number of server-side web search requests.
    #[serde(default)]
    pub web_search_requests: u32,
    /// Number of server-side web fetch requests.
    #[serde(default)]
    pub web_fetch_requests: u32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u32>,
    /// Server-side tool usage (web search, web fetch).
    #[serde(default)]
    pub server_tool_use: Option<ServerToolUseUsage>,
}

impl Usage {
    pub fn total(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }

    pub fn context_usage(&self) -> u32 {
        self.input_tokens
            + self.cache_read_input_tokens.unwrap_or(0)
            + self.cache_creation_input_tokens.unwrap_or(0)
    }

    pub fn estimated_cost(&self, model: &str) -> Decimal {
        crate::budget::pricing::global_pricing_table().calculate(model, self)
    }

    /// Get server-side web search request count.
    pub fn server_web_search_requests(&self) -> u32 {
        self.server_tool_use
            .as_ref()
            .map(|s| s.web_search_requests)
            .unwrap_or(0)
    }

    /// Get server-side web fetch request count.
    pub fn server_web_fetch_requests(&self) -> u32 {
        self.server_tool_use
            .as_ref()
            .map(|s| s.web_fetch_requests)
            .unwrap_or(0)
    }

    /// Check if any server-side tools were used.
    pub fn has_server_tool_use(&self) -> bool {
        self.server_tool_use
            .as_ref()
            .map(|s| s.web_search_requests > 0 || s.web_fetch_requests > 0)
            .unwrap_or(false)
    }
}

impl ApiResponse {
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| block.as_text())
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn wants_tool_use(&self) -> bool {
        self.stop_reason == Some(StopReason::ToolUse)
    }

    pub fn tool_uses(&self) -> Vec<&super::ToolUseBlock> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse(tool_use) => Some(tool_use),
                _ => None,
            })
            .collect()
    }

    pub fn thinking_blocks(&self) -> Vec<&super::ThinkingBlock> {
        self.content
            .iter()
            .filter_map(|block| block.as_thinking())
            .collect()
    }

    pub fn has_thinking(&self) -> bool {
        self.content.iter().any(|block| block.is_thinking())
    }

    pub fn all_citations(&self) -> Vec<&Citation> {
        self.content
            .iter()
            .filter_map(|block| block.citations())
            .flatten()
            .collect()
    }

    pub fn has_citations(&self) -> bool {
        self.content.iter().any(|block| block.has_citations())
    }

    pub fn citations_by_document(&self) -> HashMap<usize, Vec<&Citation>> {
        let mut map: HashMap<usize, Vec<&Citation>> = HashMap::new();
        for citation in self.all_citations() {
            if let Some(doc_idx) = citation.document_index() {
                map.entry(doc_idx).or_default().push(citation);
            }
        }
        map
    }

    pub fn search_citations(&self) -> Vec<&SearchResultLocationCitation> {
        self.all_citations()
            .into_iter()
            .filter_map(|c| match c {
                Citation::SearchResultLocation(src) => Some(src),
                _ => None,
            })
            .collect()
    }

    pub fn applied_edits(&self) -> &[AppliedEdit] {
        self.context_management
            .as_ref()
            .map(|cm| cm.applied_edits.as_slice())
            .unwrap_or_default()
    }

    pub fn cleared_tokens(&self) -> u64 {
        self.applied_edits()
            .iter()
            .filter_map(|e| e.cleared_input_tokens)
            .sum()
    }

    /// Get server-side tool uses from the response content.
    pub fn server_tool_uses(&self) -> Vec<&super::ServerToolUseBlock> {
        self.content
            .iter()
            .filter_map(|block| block.as_server_tool_use())
            .collect()
    }

    /// Check if the response contains any server-side tool use.
    pub fn has_server_tool_use(&self) -> bool {
        self.content.iter().any(|block| block.is_server_tool_use())
    }

    /// Get server-side web search request count from usage.
    pub fn server_web_search_requests(&self) -> u32 {
        self.usage.server_web_search_requests()
    }

    /// Get server-side web fetch request count from usage.
    pub fn server_web_fetch_requests(&self) -> u32 {
        self.usage.server_web_fetch_requests()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: MessageStartData,
    },
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: ContentDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: MessageDeltaData,
        usage: Usage,
    },
    MessageStop,
    Ping,
    Error {
        error: StreamError,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageStartData {
    pub id: String,
    #[serde(rename = "type")]
    pub message_type: String,
    pub role: String,
    pub model: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
    CitationsDelta { citation: Citation },
}

impl ContentDelta {
    pub fn is_citation(&self) -> bool {
        matches!(self, Self::CitationsDelta { .. })
    }

    pub fn as_citation(&self) -> Option<&Citation> {
        match self {
            Self::CitationsDelta { citation } => Some(citation),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDeltaData {
    pub stop_reason: Option<StopReason>,
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum CompactResult {
    NotNeeded,
    Compacted {
        original_count: usize,
        new_count: usize,
        saved_tokens: usize,
        summary: String,
    },
    Skipped {
        reason: String,
    },
}

/// Per-model usage statistics tracking.
///
/// This tracks API usage for each model separately, enabling accurate
/// cost attribution when multiple models are used (e.g., Haiku for
/// tool summarization, Opus for main agent).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    /// Input tokens consumed by this model.
    pub input_tokens: u32,
    /// Output tokens generated by this model.
    pub output_tokens: u32,
    /// Cache read tokens.
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    /// Cache creation tokens.
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    /// Number of local web search requests made by this model.
    #[serde(default)]
    pub web_search_requests: u32,
    /// Number of local web fetch requests made by this model.
    #[serde(default)]
    pub web_fetch_requests: u32,
    /// Estimated cost in USD for this model's usage.
    #[serde(default)]
    pub cost_usd: Decimal,
    /// Context window size for this model.
    #[serde(default)]
    pub context_window: u64,
}

impl ModelUsage {
    pub fn from_usage(usage: &Usage, model: &str) -> Self {
        let cost = usage.estimated_cost(model);
        let context_window = crate::models::context_window::for_model(model);
        let (web_search, web_fetch) = usage
            .server_tool_use
            .as_ref()
            .map(|s| (s.web_search_requests, s.web_fetch_requests))
            .unwrap_or((0, 0));
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0),
            cache_creation_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
            web_search_requests: web_search,
            web_fetch_requests: web_fetch,
            cost_usd: cost,
            context_window,
        }
    }

    pub fn add(&mut self, other: &ModelUsage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .saturating_add(other.cache_read_input_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .saturating_add(other.cache_creation_input_tokens);
        self.web_search_requests = self
            .web_search_requests
            .saturating_add(other.web_search_requests);
        self.web_fetch_requests = self
            .web_fetch_requests
            .saturating_add(other.web_fetch_requests);
        self.cost_usd += other.cost_usd;
    }

    pub fn add_usage(&mut self, usage: &Usage, model: &str) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens.unwrap_or(0));
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens.unwrap_or(0));
        self.cost_usd += usage.estimated_cost(model);
        if let Some(ref server_usage) = usage.server_tool_use {
            self.web_search_requests = self
                .web_search_requests
                .saturating_add(server_usage.web_search_requests);
            self.web_fetch_requests = self
                .web_fetch_requests
                .saturating_add(server_usage.web_fetch_requests);
        }
    }

    pub fn total_tokens(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }
}

/// Server-side tool usage statistics.
///
/// Tracks usage of server-side tools that are executed by the Anthropic API
/// rather than locally. This is parsed from the API response's `server_tool_use`
/// field when available.
///
/// **Important distinction:**
/// - `server_tool_use`: Tools executed by Anthropic's servers (e.g., server-side RAG)
/// - `tool_stats["WebSearch"]`: Local WebSearch tool calls (via DuckDuckGo etc.)
/// - `modelUsage.*.webSearchRequests`: Per-model local web search counts
///
/// Currently, Anthropic's API may return this field for certain features like
/// built-in web search or retrieval. If the API doesn't return this field,
/// values remain at 0.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerToolUse {
    /// Number of server-side web search requests (from API response).
    pub web_search_requests: u32,
    /// Number of server-side web fetch requests (from API response).
    pub web_fetch_requests: u32,
}

impl ServerToolUse {
    /// Record a web search request.
    pub fn record_web_search(&mut self) {
        self.web_search_requests += 1;
    }

    /// Record a web fetch request.
    pub fn record_web_fetch(&mut self) {
        self.web_fetch_requests += 1;
    }

    /// Check if any server tools were used.
    pub fn has_usage(&self) -> bool {
        self.web_search_requests > 0 || self.web_fetch_requests > 0
    }

    /// Add counts from API response's server_tool_use usage.
    pub fn add_from_usage(&mut self, usage: &ServerToolUseUsage) {
        self.web_search_requests += usage.web_search_requests;
        self.web_fetch_requests += usage.web_fetch_requests;
    }
}

/// Record of a authorization denial for a tool request.
///
/// Tracks when and why a tool execution was blocked, useful for
/// debugging and audit logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizationDenied {
    /// Name of the tool that was denied.
    pub tool_name: String,
    /// The tool use ID from the API request.
    pub tool_use_id: String,
    /// The input that was provided to the tool.
    pub tool_input: serde_json::Value,
    /// Reason for the denial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Timestamp when the denial occurred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

impl AuthorizationDenied {
    /// Create a new authorization denial record.
    pub fn new(
        tool_name: impl Into<String>,
        tool_use_id: impl Into<String>,
        tool_input: serde_json::Value,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            tool_use_id: tool_use_id.into(),
            tool_input,
            reason: None,
            timestamp: Some(chrono::Utc::now()),
        }
    }

    /// Add a reason for the denial.
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_usage_total() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };
        assert_eq!(usage.total(), 150);
    }

    #[test]
    fn test_usage_cost() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };
        // Sonnet long context (1M > 200K): $3 * 2 input + $15 output = $21
        let cost = usage.estimated_cost("claude-sonnet-4-5");
        assert_eq!(cost, dec!(21));
    }

    #[test]
    fn test_model_usage_from_usage() {
        let usage = Usage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_input_tokens: Some(100),
            cache_creation_input_tokens: Some(50),
            ..Default::default()
        };
        let model_usage = ModelUsage::from_usage(&usage, "claude-sonnet-4-5");
        assert_eq!(model_usage.input_tokens, 1000);
        assert_eq!(model_usage.output_tokens, 500);
        assert_eq!(model_usage.cache_read_input_tokens, 100);
        assert!(model_usage.cost_usd > Decimal::ZERO);
    }

    #[test]
    fn test_model_usage_add() {
        let mut usage1 = ModelUsage {
            input_tokens: 100,
            output_tokens: 50,
            cost_usd: dec!(0.01),
            ..Default::default()
        };
        let usage2 = ModelUsage {
            input_tokens: 200,
            output_tokens: 100,
            cost_usd: dec!(0.02),
            ..Default::default()
        };
        usage1.add(&usage2);
        assert_eq!(usage1.input_tokens, 300);
        assert_eq!(usage1.output_tokens, 150);
        assert_eq!(usage1.cost_usd, dec!(0.03));
    }

    #[test]
    fn test_server_tool_use() {
        let mut stu = ServerToolUse::default();
        assert!(!stu.has_usage());

        stu.record_web_search();
        assert!(stu.has_usage());
        assert_eq!(stu.web_search_requests, 1);

        stu.record_web_fetch();
        assert_eq!(stu.web_fetch_requests, 1);
    }

    #[test]
    fn test_authorization_denial() {
        let denial = AuthorizationDenied::new(
            "WebSearch",
            "tool_123",
            serde_json::json!({"query": "test"}),
        )
        .reason("User denied");

        assert_eq!(denial.tool_name, "WebSearch");
        assert_eq!(denial.reason, Some("User denied".to_string()));
        assert!(denial.timestamp.is_some());
    }

    #[test]
    fn test_server_tool_use_usage_parsing() {
        let json = r#"{
            "input_tokens": 1000,
            "output_tokens": 500,
            "server_tool_use": {
                "web_search_requests": 3,
                "web_fetch_requests": 2
            }
        }"#;
        let usage: Usage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert!(usage.has_server_tool_use());
        assert_eq!(usage.server_web_search_requests(), 3);
        assert_eq!(usage.server_web_fetch_requests(), 2);
    }

    #[test]
    fn test_server_tool_use_usage_empty() {
        let json = r#"{
            "input_tokens": 100,
            "output_tokens": 50
        }"#;
        let usage: Usage = serde_json::from_str(json).unwrap();
        assert!(!usage.has_server_tool_use());
        assert_eq!(usage.server_web_search_requests(), 0);
        assert_eq!(usage.server_web_fetch_requests(), 0);
    }

    #[test]
    fn test_server_tool_use_add_from_usage() {
        let mut stu = ServerToolUse::default();
        let usage = ServerToolUseUsage {
            web_search_requests: 2,
            web_fetch_requests: 1,
        };
        stu.add_from_usage(&usage);
        assert_eq!(stu.web_search_requests, 2);
        assert_eq!(stu.web_fetch_requests, 1);

        // Add more
        stu.add_from_usage(&usage);
        assert_eq!(stu.web_search_requests, 4);
        assert_eq!(stu.web_fetch_requests, 2);
    }
}
