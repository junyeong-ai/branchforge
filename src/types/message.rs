//! Message types for the Claude API.

use serde::{Deserialize, Serialize};

use super::ContentBlock;
use super::document::DocumentBlock;
use super::search::SearchResultBlock;

/// Role of a message participant
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// User message
    User,
    /// Assistant (Claude) message
    Assistant,
}

/// A message in a conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Role of the message sender
    pub role: Role,
    /// Content of the message
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn tool_results(results: Vec<super::ToolResultBlock>) -> Self {
        Self {
            role: Role::User,
            content: results.into_iter().map(ContentBlock::ToolResult).collect(),
        }
    }

    pub fn user_with_content(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::User,
            content,
        }
    }

    pub fn user_with_document(text: impl Into<String>, doc: DocumentBlock) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Document(doc), ContentBlock::text(text)],
        }
    }

    pub fn user_with_documents(text: impl Into<String>, docs: Vec<DocumentBlock>) -> Self {
        let mut content: Vec<ContentBlock> = docs.into_iter().map(ContentBlock::Document).collect();
        content.push(ContentBlock::text(text));
        Self {
            role: Role::User,
            content,
        }
    }

    pub fn user_with_search_results(
        text: impl Into<String>,
        results: Vec<SearchResultBlock>,
    ) -> Self {
        let mut content: Vec<ContentBlock> = results
            .into_iter()
            .map(ContentBlock::SearchResult)
            .collect();
        content.push(ContentBlock::text(text));
        Self {
            role: Role::User,
            content,
        }
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| block.as_text())
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn has_tool_use(&self) -> bool {
        self.content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
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

    pub fn documents(&self) -> Vec<&DocumentBlock> {
        self.content
            .iter()
            .filter_map(|block| block.as_document())
            .collect()
    }

    pub fn search_results(&self) -> Vec<&SearchResultBlock> {
        self.content
            .iter()
            .filter_map(|block| block.as_search_result())
            .collect()
    }

    pub fn cache_on_last_block(mut self) -> Self {
        if let Some(last) = self.content.pop() {
            self.content
                .push(last.with_cache_control(CacheControl::ephemeral()));
        }
        self
    }

    pub fn set_cache_on_last_block(&mut self, cache: CacheControl) {
        if let Some(last) = self.content.last_mut() {
            last.set_cache_control(Some(cache));
        }
    }

    pub fn clear_cache_control(&mut self) {
        for block in &mut self.content {
            block.set_cache_control(None);
        }
    }

    pub fn has_cache_control(&self) -> bool {
        self.content.iter().any(|b| b.is_cached())
    }
}

/// System prompt configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    /// Simple text system prompt
    Text(String),
    /// Structured system prompt with cache control
    Blocks(Vec<SystemBlock>),
}

impl Default for SystemPrompt {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl SystemPrompt {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(s) => s.is_empty(),
            Self::Blocks(b) => b.is_empty(),
        }
    }

    pub fn as_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Blocks(b) => b
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }
}

impl std::fmt::Display for SystemPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_text())
    }
}

/// A block in a structured system prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    /// Type of the block (always "text" for now).
    #[serde(rename = "type")]
    pub block_type: String,
    /// Text content.
    pub text: String,
    /// Optional cache control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl SystemBlock {
    /// Create a new system block with caching enabled (default TTL).
    pub fn cached(text: impl Into<String>) -> Self {
        Self {
            block_type: "text".to_string(),
            text: text.into(),
            cache_control: Some(CacheControl::ephemeral()),
        }
    }

    /// Create a new system block with caching and specific TTL.
    ///
    /// Per Anthropic best practices:
    /// - Use OneHour for static content (system prompts, tools)
    /// - Use FiveMinutes for dynamic content (messages)
    pub fn cached_with_ttl(text: impl Into<String>, ttl: CacheTtl) -> Self {
        Self {
            block_type: "text".to_string(),
            text: text.into(),
            cache_control: Some(CacheControl::ephemeral().ttl(ttl)),
        }
    }

    /// Create a new system block without caching.
    pub fn uncached(text: impl Into<String>) -> Self {
        Self {
            block_type: "text".to_string(),
            text: text.into(),
            cache_control: None,
        }
    }
}

/// Cache control for prompt caching.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: CacheType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<CacheTtl>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheType {
    Ephemeral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheTtl {
    FiveMinutes,
    OneHour,
}

impl Serialize for CacheTtl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            CacheTtl::FiveMinutes => serializer.serialize_str("5m"),
            CacheTtl::OneHour => serializer.serialize_str("1h"),
        }
    }
}

impl<'de> Deserialize<'de> for CacheTtl {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "5m" => Ok(CacheTtl::FiveMinutes),
            "1h" => Ok(CacheTtl::OneHour),
            _ => Err(serde::de::Error::custom(format!("Unknown TTL: {}", s))),
        }
    }
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self {
            cache_type: CacheType::Ephemeral,
            ttl: None,
        }
    }

    pub fn ephemeral_5m() -> Self {
        Self {
            cache_type: CacheType::Ephemeral,
            ttl: Some(CacheTtl::FiveMinutes),
        }
    }

    pub fn ephemeral_1h() -> Self {
        Self {
            cache_type: CacheType::Ephemeral,
            ttl: Some(CacheTtl::OneHour),
        }
    }

    pub fn ttl(mut self, ttl: CacheTtl) -> Self {
        self.ttl = Some(ttl);
        self
    }
}

impl SystemPrompt {
    /// Create a simple text system prompt
    pub fn text(prompt: impl Into<String>) -> Self {
        Self::Text(prompt.into())
    }

    /// Create a system prompt with caching enabled
    pub fn cached(prompt: impl Into<String>) -> Self {
        Self::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: prompt.into(),
            cache_control: Some(CacheControl {
                cache_type: CacheType::Ephemeral,
                ttl: None,
            }),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_message() {
        let msg = Message::user("Hello");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.text(), "Hello");
    }

    #[test]
    fn test_assistant_message() {
        let msg = Message::assistant("Hi there!");
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.text(), "Hi there!");
    }
}
