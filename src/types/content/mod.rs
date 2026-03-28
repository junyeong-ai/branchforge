//! Content block types for messages.

mod image;
mod server_tools;
mod tool_blocks;

use std::path::Path;

use serde::{Deserialize, Serialize};

pub use image::ImageSource;
pub use server_tools::{
    ServerToolError, ServerToolUseBlock, WebFetchResultItem, WebFetchToolResultBlock,
    WebFetchToolResultContent, WebSearchResultItem, WebSearchToolResultBlock,
    WebSearchToolResultContent,
};
pub use tool_blocks::{ToolResultBlock, ToolResultContent, ToolResultContentBlock, ToolUseBlock};

use super::citations::Citation;
use super::document::DocumentBlock;
use super::message::CacheControl;
use super::search::SearchResultBlock;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        citations: Option<Vec<Citation>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
    Image {
        source: ImageSource,
    },
    Document(DocumentBlock),
    SearchResult(SearchResultBlock),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
    Thinking(ThinkingBlock),
    RedactedThinking {
        data: String,
    },
    ServerToolUse(ServerToolUseBlock),
    WebSearchToolResult(WebSearchToolResultBlock),
    WebFetchToolResult(WebFetchToolResultBlock),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    pub thinking: String,
    pub signature: String,
}

impl From<&str> for ContentBlock {
    fn from(text: &str) -> Self {
        ContentBlock::Text {
            text: text.to_string(),
            citations: None,
            cache_control: None,
        }
    }
}

impl From<String> for ContentBlock {
    fn from(text: String) -> Self {
        ContentBlock::Text {
            text,
            citations: None,
            cache_control: None,
        }
    }
}

impl From<DocumentBlock> for ContentBlock {
    fn from(doc: DocumentBlock) -> Self {
        ContentBlock::Document(doc)
    }
}

impl From<SearchResultBlock> for ContentBlock {
    fn from(result: SearchResultBlock) -> Self {
        ContentBlock::SearchResult(result)
    }
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock::Text {
            text: text.into(),
            citations: None,
            cache_control: None,
        }
    }

    pub fn text_cached(text: impl Into<String>) -> Self {
        ContentBlock::Text {
            text: text.into(),
            citations: None,
            cache_control: Some(CacheControl::ephemeral()),
        }
    }

    pub fn text_with_cache(text: impl Into<String>, cache: CacheControl) -> Self {
        ContentBlock::Text {
            text: text.into(),
            citations: None,
            cache_control: Some(cache),
        }
    }

    pub fn text_with_citations(text: impl Into<String>, citations: Vec<Citation>) -> Self {
        ContentBlock::Text {
            text: text.into(),
            citations: if citations.is_empty() {
                None
            } else {
                Some(citations)
            },
            cache_control: None,
        }
    }

    pub fn document(doc: DocumentBlock) -> Self {
        ContentBlock::Document(doc)
    }

    pub fn search_result(result: SearchResultBlock) -> Self {
        ContentBlock::SearchResult(result)
    }

    pub fn image(source: ImageSource) -> Self {
        ContentBlock::Image { source }
    }

    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        ContentBlock::Image {
            source: ImageSource::base64(media_type, data),
        }
    }

    pub fn image_url(url: impl Into<String>) -> Self {
        ContentBlock::Image {
            source: ImageSource::from_url(url),
        }
    }

    pub fn image_file(file_id: impl Into<String>) -> Self {
        ContentBlock::Image {
            source: ImageSource::from_file(file_id),
        }
    }

    pub async fn image_from_path(path: impl AsRef<Path>) -> crate::Result<Self> {
        Ok(ContentBlock::Image {
            source: ImageSource::from_path(path).await?,
        })
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    pub fn citations(&self) -> Option<&[Citation]> {
        match self {
            ContentBlock::Text { citations, .. } => citations.as_deref(),
            _ => None,
        }
    }

    pub fn has_citations(&self) -> bool {
        matches!(self, ContentBlock::Text { citations: Some(c), .. } if !c.is_empty())
    }

    pub fn cache_control(&self) -> Option<&CacheControl> {
        match self {
            ContentBlock::Text { cache_control, .. } => cache_control.as_ref(),
            ContentBlock::Document(doc) => doc.cache_control.as_ref(),
            ContentBlock::SearchResult(sr) => sr.cache_control.as_ref(),
            _ => None,
        }
    }

    pub fn is_cached(&self) -> bool {
        self.cache_control().is_some()
    }

    /// Set cache control in-place (only for Text blocks).
    pub fn set_cache_control(&mut self, cache: Option<CacheControl>) {
        if let ContentBlock::Text { cache_control, .. } = self {
            *cache_control = cache;
        }
    }

    pub fn with_cache_control(self, cache: CacheControl) -> Self {
        match self {
            ContentBlock::Text {
                text, citations, ..
            } => ContentBlock::Text {
                text,
                citations,
                cache_control: Some(cache),
            },
            other => other,
        }
    }

    pub fn without_cache_control(self) -> Self {
        match self {
            ContentBlock::Text {
                text, citations, ..
            } => ContentBlock::Text {
                text,
                citations,
                cache_control: None,
            },
            other => other,
        }
    }

    pub fn as_document(&self) -> Option<&DocumentBlock> {
        match self {
            ContentBlock::Document(doc) => Some(doc),
            _ => None,
        }
    }

    pub fn as_search_result(&self) -> Option<&SearchResultBlock> {
        match self {
            ContentBlock::SearchResult(sr) => Some(sr),
            _ => None,
        }
    }

    pub fn is_document(&self) -> bool {
        matches!(self, ContentBlock::Document(_))
    }

    pub fn is_search_result(&self) -> bool {
        matches!(self, ContentBlock::SearchResult(_))
    }

    pub fn is_image(&self) -> bool {
        matches!(self, ContentBlock::Image { .. })
    }

    pub fn as_image(&self) -> Option<&ImageSource> {
        match self {
            ContentBlock::Image { source } => Some(source),
            _ => None,
        }
    }

    pub fn as_thinking(&self) -> Option<&ThinkingBlock> {
        match self {
            ContentBlock::Thinking(block) => Some(block),
            _ => None,
        }
    }

    pub fn is_thinking(&self) -> bool {
        matches!(
            self,
            ContentBlock::Thinking(_) | ContentBlock::RedactedThinking { .. }
        )
    }

    pub fn is_server_tool_use(&self) -> bool {
        matches!(self, ContentBlock::ServerToolUse(_))
    }

    pub fn as_server_tool_use(&self) -> Option<&ServerToolUseBlock> {
        match self {
            ContentBlock::ServerToolUse(block) => Some(block),
            _ => None,
        }
    }

    pub fn is_web_search_result(&self) -> bool {
        matches!(self, ContentBlock::WebSearchToolResult(_))
    }

    pub fn as_web_search_result(&self) -> Option<&WebSearchToolResultBlock> {
        match self {
            ContentBlock::WebSearchToolResult(block) => Some(block),
            _ => None,
        }
    }

    pub fn is_web_fetch_result(&self) -> bool {
        matches!(self, ContentBlock::WebFetchToolResult(_))
    }

    pub fn as_web_fetch_result(&self) -> Option<&WebFetchToolResultBlock> {
        match self {
            ContentBlock::WebFetchToolResult(block) => Some(block),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_block_text() {
        let block = ContentBlock::text("Hello");
        assert_eq!(block.as_text(), Some("Hello"));
        assert!(!block.has_citations());
        assert!(!block.is_cached());
    }

    #[test]
    fn test_content_block_cached() {
        let block = ContentBlock::text_cached("Hello");
        assert_eq!(block.as_text(), Some("Hello"));
        assert!(block.is_cached());
        assert!(block.cache_control().is_some());
    }

    #[test]
    fn test_content_block_cache_control() {
        use crate::types::{CacheControl, CacheTtl};

        let block = ContentBlock::text("Hello").with_cache_control(CacheControl::ephemeral_1h());
        assert!(block.is_cached());
        assert_eq!(block.cache_control().unwrap().ttl, Some(CacheTtl::OneHour));

        let block = block.without_cache_control();
        assert!(!block.is_cached());
    }

    #[test]
    fn test_content_block_from_document() {
        let doc = DocumentBlock::text("content");
        let block: ContentBlock = doc.into();
        assert!(block.is_document());
    }

    #[test]
    fn test_content_block_image() {
        let block = ContentBlock::image_file("file_123");
        assert!(block.is_image());
        assert!(block.as_image().is_some());
        assert_eq!(block.as_image().unwrap().file_id(), Some("file_123"));

        let block = ContentBlock::image_url("https://example.com/img.png");
        assert!(block.is_image());
    }

    #[tokio::test]
    async fn test_content_block_image_from_path() {
        let dir = tempfile::tempdir().unwrap();
        let jpeg_path = dir.path().join("test.jpg");

        let jpeg_data: [u8; 4] = [0xFF, 0xD8, 0xFF, 0xE0];
        tokio::fs::write(&jpeg_path, &jpeg_data).await.unwrap();

        let block = ContentBlock::image_from_path(&jpeg_path).await.unwrap();
        assert!(block.is_image());

        let source = block.as_image().unwrap();
        assert!(source.is_base64());
        assert_eq!(source.media_type(), Some("image/jpeg"));
    }

    #[test]
    fn test_thinking_block_serialization() {
        let block = ThinkingBlock {
            thinking: "Let me analyze this...".to_string(),
            signature: "sig_abc123".to_string(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"thinking\":\"Let me analyze this...\""));
        assert!(json.contains("\"signature\":\"sig_abc123\""));
    }

    #[test]
    fn test_thinking_block_deserialization() {
        let json = r#"{"thinking":"Step by step reasoning","signature":"sig_xyz"}"#;
        let block: ThinkingBlock = serde_json::from_str(json).unwrap();
        assert_eq!(block.thinking, "Step by step reasoning");
        assert_eq!(block.signature, "sig_xyz");
    }

    #[test]
    fn test_content_block_thinking_variant() {
        let thinking = ThinkingBlock {
            thinking: "Analysis".to_string(),
            signature: "sig".to_string(),
        };
        let block = ContentBlock::Thinking(thinking);
        assert!(block.is_thinking());
        assert!(block.as_thinking().is_some());
        assert_eq!(block.as_thinking().unwrap().thinking, "Analysis");
    }

    #[test]
    fn test_content_block_redacted_thinking() {
        let block = ContentBlock::RedactedThinking {
            data: "encrypted_data".to_string(),
        };
        assert!(block.is_thinking());
        assert!(block.as_thinking().is_none());
    }

    #[test]
    fn test_thinking_content_block_serialization() {
        let block = ContentBlock::Thinking(ThinkingBlock {
            thinking: "Reasoning here".to_string(),
            signature: "sig123".to_string(),
        });
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"thinking\""));
        assert!(json.contains("\"thinking\":\"Reasoning here\""));
    }

    #[test]
    fn test_redacted_thinking_serialization() {
        let block = ContentBlock::RedactedThinking {
            data: "redacted_content".to_string(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"redacted_thinking\""));
        assert!(json.contains("\"data\":\"redacted_content\""));
    }

    #[test]
    fn test_content_block_server_tool_helpers() {
        let text_block = ContentBlock::text("Hello");
        assert!(!text_block.is_server_tool_use());
        assert!(!text_block.is_web_search_result());
        assert!(!text_block.is_web_fetch_result());
        assert!(text_block.as_server_tool_use().is_none());
        assert!(text_block.as_web_search_result().is_none());
        assert!(text_block.as_web_fetch_result().is_none());
    }
}
