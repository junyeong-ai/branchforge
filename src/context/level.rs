//! Memory provider for aggregating content from multiple resource levels.
//!
//! Provides leveled resource loading with fixed override order:
//! Enterprise → User → Project → Local (later levels override earlier).

#![allow(missing_docs)]

use std::path::PathBuf;

use async_trait::async_trait;

use super::{ContextResult, MemoryContent, MemoryProvider, RuleIndex};

/// Memory provider that aggregates content from multiple resource levels.
///
/// Contents are merged in the order they are added, with later additions
/// taking precedence. When used with the CLI resource loading methods,
/// the order is always: Enterprise → User → Project → Local.
#[derive(Debug, Default)]
pub struct LeveledMemoryProvider {
    contents: Vec<MemoryContent>,
}

impl LeveledMemoryProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds raw CLAUDE.md content.
    pub fn add_content(&mut self, content: impl Into<String>) {
        let mut mc = MemoryContent::default();
        mc.claude_md.push(content.into());
        self.contents.push(mc);
    }

    /// Adds raw CLAUDE.local.md content.
    pub fn add_local_content(&mut self, content: impl Into<String>) {
        let mut mc = MemoryContent::default();
        mc.local_md.push(content.into());
        self.contents.push(mc);
    }

    /// Adds a rule index for progressive disclosure.
    pub fn add_rule(&mut self, rule: RuleIndex) {
        let mut mc = MemoryContent::default();
        mc.rule_indices.push(rule);
        self.contents.push(mc);
    }

    /// Adds pre-loaded memory content from a MemoryLoader.
    pub fn add_memory_content(&mut self, content: MemoryContent) {
        self.contents.push(content);
    }
}

#[async_trait]
impl MemoryProvider for LeveledMemoryProvider {
    fn name(&self) -> &str {
        "leveled"
    }

    async fn load(&self) -> ContextResult<MemoryContent> {
        let mut combined = MemoryContent::default();
        for content in &self.contents {
            combined.merge(content.clone());
        }
        Ok(combined)
    }
}

pub fn enterprise_base_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let path = PathBuf::from("/Library/Application Support/ClaudeCode");
        if path.exists() {
            return Some(path);
        }
    }
    #[cfg(target_os = "linux")]
    {
        let path = PathBuf::from("/etc/claude-code");
        if path.exists() {
            return Some(path);
        }
    }
    None
}

pub fn user_base_path() -> Option<PathBuf> {
    crate::common::home_dir().map(|h| h.join(".claude"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_leveled_memory_provider() {
        let mut provider = LeveledMemoryProvider::new();
        provider.add_content("# Enterprise Rules");
        provider.add_content("# User Preferences");
        provider.add_content("# Project Guidelines");

        let content = provider.load().await.unwrap();
        assert_eq!(content.claude_md.len(), 3);
    }

    #[tokio::test]
    async fn test_leveled_with_local() {
        let mut provider = LeveledMemoryProvider::new();
        provider.add_content("Main content");
        provider.add_local_content("Local content");

        let content = provider.load().await.unwrap();
        assert_eq!(content.claude_md.len(), 1);
        assert_eq!(content.local_md.len(), 1);

        let combined = content.combined_claude_md();
        assert!(combined.contains("Main content"));
        assert!(combined.contains("Local content"));
    }
}
