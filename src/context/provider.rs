//! Memory provider trait and implementations.
//!
//! Providers abstract the source of memory content, allowing for both
//! in-memory testing and file-based production use.

use std::path::PathBuf;

use async_trait::async_trait;

use super::{ContextResult, MemoryContent, MemoryLoader};

/// Trait for providing memory content from various sources.
///
/// Implementations can load CLAUDE.md content from:
/// - In-memory strings (for testing/programmatic use)
/// - File system (for CLI-compatible behavior)
/// - Remote sources (for enterprise deployment)
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    /// Returns the provider name for debugging/logging.
    fn name(&self) -> &str;

    /// Loads memory content from this provider.
    async fn load(&self) -> ContextResult<MemoryContent>;
}

/// In-memory provider for testing and programmatic use.
///
/// # Example
/// ```
/// use branchforge::context::MemoryContextProvider;
///
/// let provider = MemoryContextProvider::new()
///     .claude_md("# Project Rules\nUse async/await for all I/O.");
/// ```
#[derive(Debug, Clone, Default)]
pub struct MemoryContextProvider {
    /// Content to include as CLAUDE.md.
    pub claude_md: Vec<String>,
    /// Content to include as CLAUDE.local.md.
    pub local_md: Vec<String>,
}

impl MemoryContextProvider {
    /// Creates a new empty MemoryContextProvider.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds content to the CLAUDE.md section.
    pub fn claude_md(mut self, content: impl Into<String>) -> Self {
        self.claude_md.push(content.into());
        self
    }

    /// Adds content to the CLAUDE.local.md section.
    pub fn local_md(mut self, content: impl Into<String>) -> Self {
        self.local_md.push(content.into());
        self
    }
}

#[async_trait]
impl MemoryProvider for MemoryContextProvider {
    fn name(&self) -> &str {
        "in-memory"
    }

    async fn load(&self) -> ContextResult<MemoryContent> {
        Ok(MemoryContent {
            claude_md: self.claude_md.clone(),
            local_md: self.local_md.clone(),
            rule_indices: Vec::new(),
        })
    }
}

/// File-based memory provider for CLI-compatible behavior.
///
/// Loads CLAUDE.md and CLAUDE.local.md files from the file system
/// with full @import support.
///
/// # Example
/// ```no_run
/// use branchforge::context::FileMemoryProvider;
///
/// let provider = FileMemoryProvider::new("/path/to/project");
/// ```
#[derive(Debug, Clone)]
pub struct FileMemoryProvider {
    /// Root path to load memory files from.
    pub path: PathBuf,
}

impl FileMemoryProvider {
    /// Creates a new FileMemoryProvider for the given directory.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[async_trait]
impl MemoryProvider for FileMemoryProvider {
    fn name(&self) -> &str {
        "file"
    }

    async fn load(&self) -> ContextResult<MemoryContent> {
        let loader = MemoryLoader::new();
        loader.load(&self.path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_provider() {
        let provider = MemoryContextProvider::new()
            .claude_md("# Project Rules")
            .claude_md("Use async/await.");

        let content = provider.load().await.unwrap();
        assert_eq!(content.claude_md.len(), 2);
        assert!(content.claude_md[0].contains("Project Rules"));
    }

    #[tokio::test]
    async fn test_in_memory_provider_with_local() {
        let provider = MemoryContextProvider::new()
            .claude_md("Shared rules")
            .local_md("Local settings");

        let content = provider.load().await.unwrap();
        assert_eq!(content.claude_md.len(), 1);
        assert_eq!(content.local_md.len(), 1);
    }

    #[tokio::test]
    async fn test_empty_provider() {
        let provider = MemoryContextProvider::new();
        let content = provider.load().await.unwrap();
        assert!(content.is_empty());
    }
}
