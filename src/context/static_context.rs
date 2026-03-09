//! Static Context for Prompt Caching
//!
//! Content that is always loaded and cached for the entire session.
//! Per Anthropic best practices, static content uses 1-hour TTL.

use crate::mcp::make_mcp_name;
use crate::types::{CacheTtl, SystemBlock, ToolDefinition};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default)]
pub struct StaticContext {
    pub system_prompt: String,
    pub claude_md: String,
    pub skill_summary: String,
    pub rules_summary: String,
    pub tool_definitions: Vec<ToolDefinition>,
    pub mcp_tool_metadata: Vec<McpToolMeta>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpToolMeta {
    pub server: String,
    pub name: String,
    pub description: String,
}

impl StaticContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn claude_md(mut self, content: impl Into<String>) -> Self {
        self.claude_md = content.into();
        self
    }

    pub fn skill_summary(mut self, summary: impl Into<String>) -> Self {
        self.skill_summary = summary.into();
        self
    }

    pub fn rules_summary(mut self, summary: impl Into<String>) -> Self {
        self.rules_summary = summary.into();
        self
    }

    pub fn tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tool_definitions = tools;
        self
    }

    pub fn mcp_tools(mut self, tools: Vec<McpToolMeta>) -> Self {
        self.mcp_tool_metadata = tools;
        self
    }

    /// Convert static context to system blocks.
    ///
    /// Static blocks are cached only when the caller enables static-context caching.
    pub fn to_system_blocks(&self, cache_static: bool, ttl: CacheTtl) -> Vec<SystemBlock> {
        let mut blocks = Vec::new();

        if !self.system_prompt.is_empty() {
            blocks.push(self.make_block(&self.system_prompt, cache_static, ttl));
        }

        if !self.claude_md.is_empty() {
            blocks.push(self.make_block(&self.claude_md, cache_static, ttl));
        }

        if !self.skill_summary.is_empty() {
            blocks.push(self.make_block(&self.skill_summary, cache_static, ttl));
        }

        if !self.rules_summary.is_empty() {
            blocks.push(self.make_block(&self.rules_summary, cache_static, ttl));
        }

        blocks
    }

    pub fn tool_summary(&self) -> Option<String> {
        let mut lines = Vec::new();

        if !self.tool_definitions.is_empty() {
            lines.push("# Built-in Tools".to_string());
            for tool in &self.tool_definitions {
                lines.push(format!("- {}: {}", tool.name, tool.description));
            }
        }

        if !self.mcp_tool_metadata.is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(self.build_mcp_summary());
        }

        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    fn make_block(&self, text: &str, cached: bool, ttl: CacheTtl) -> SystemBlock {
        if cached {
            SystemBlock::cached_with_ttl(text, ttl)
        } else {
            SystemBlock::uncached(text)
        }
    }

    fn build_mcp_summary(&self) -> String {
        let mut lines = vec!["# MCP Server Tools".to_string()];
        for tool in &self.mcp_tool_metadata {
            lines.push(format!(
                "- {}:  {}",
                make_mcp_name(&tool.server, &tool.name),
                tool.description
            ));
        }
        lines.join("\n")
    }

    pub fn content_hash(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        self.system_prompt.hash(&mut hasher);
        self.claude_md.hash(&mut hasher);
        self.skill_summary.hash(&mut hasher);
        self.rules_summary.hash(&mut hasher);

        for tool in &self.tool_definitions {
            tool.name.hash(&mut hasher);
            tool.description.hash(&mut hasher);
            tool.input_schema.to_string().hash(&mut hasher);
        }

        for mcp in &self.mcp_tool_metadata {
            mcp.server.hash(&mut hasher);
            mcp.name.hash(&mut hasher);
        }

        format!("{:016x}", hasher.finish())
    }

    pub fn estimate_tokens(&self) -> u64 {
        let total_chars = self.system_prompt.len()
            + self.claude_md.len()
            + self.skill_summary.len()
            + self.rules_summary.len()
            + self
                .mcp_tool_metadata
                .iter()
                .map(|t| t.description.len())
                .sum::<usize>();

        (total_chars / 4) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CacheType;

    #[test]
    fn test_system_block_cached_with_ttl() {
        let block = SystemBlock::cached_with_ttl("Hello", CacheTtl::OneHour);
        assert!(block.cache_control.is_some());
        let cache_ctrl = block.cache_control.unwrap();
        assert_eq!(cache_ctrl.cache_type, CacheType::Ephemeral);
        assert_eq!(cache_ctrl.ttl, Some(CacheTtl::OneHour));
        assert_eq!(block.block_type, "text");
    }

    #[test]
    fn test_static_context_blocks() {
        let static_context = StaticContext::new()
            .system_prompt("You are a helpful assistant")
            .claude_md("# Project\nThis is a Rust project");

        let blocks = static_context.to_system_blocks(true, CacheTtl::OneHour);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].text.contains("helpful assistant"));
        assert!(blocks[1].text.contains("Rust project"));
    }

    #[test]
    fn test_content_hash_consistency() {
        let ctx1 = StaticContext::new()
            .system_prompt("Same prompt")
            .claude_md("Same content");

        let ctx2 = StaticContext::new()
            .system_prompt("Same prompt")
            .claude_md("Same content");

        assert_eq!(ctx1.content_hash(), ctx2.content_hash());
    }

    #[test]
    fn test_content_hash_different() {
        let ctx1 = StaticContext::new().system_prompt("Prompt A");
        let ctx2 = StaticContext::new().system_prompt("Prompt B");

        assert_ne!(ctx1.content_hash(), ctx2.content_hash());
    }
}
