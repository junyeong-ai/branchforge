//! Content source abstraction for lazy loading.
//!
//! `ContentSource` represents where content can be loaded from,
//! enabling progressive disclosure where metadata is always available
//! but full content is loaded on-demand.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Global HTTP client for content fetching.
static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Default timeout for HTTP content fetching.
const HTTP_TIMEOUT_SECS: u64 = 30;

fn get_http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .unwrap_or_default()
    })
}

/// Source location for loading content on-demand.
///
/// This enables the progressive disclosure pattern where indices contain
/// minimal metadata (name, description) while full content is loaded
/// only when needed.
#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentSource {
    /// File system path
    File {
        /// Path to the content file
        path: PathBuf,
    },

    /// In-memory content (already loaded or code-defined)
    InMemory {
        /// The actual content
        content: String,
    },

    /// HTTP endpoint (for remote content)
    Http {
        /// URL to fetch content from
        url: String,
    },
}

impl ContentSource {
    /// Create a file-based content source.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File { path: path.into() }
    }

    /// Create an in-memory content source.
    pub fn in_memory(content: impl Into<String>) -> Self {
        Self::InMemory {
            content: content.into(),
        }
    }

    /// Create an HTTP content source.
    pub fn http(url: impl Into<String>) -> Self {
        Self::Http { url: url.into() }
    }

    /// Load the content from this source.
    ///
    /// This is the core lazy-loading mechanism. Content is only fetched
    /// when this method is called, not when the index is created.
    pub async fn load(&self) -> crate::Result<String> {
        match self {
            Self::File { path } => tokio::fs::read_to_string(path).await.map_err(|e| {
                crate::Error::Config(format!("Failed to load content from {:?}: {}", path, e))
            }),
            Self::InMemory { content } => Ok(content.clone()),
            Self::Http { url } => {
                let response =
                    get_http_client().get(url).send().await.map_err(|e| {
                        crate::Error::Config(format!("Failed to fetch {}: {}", url, e))
                    })?;

                if !response.status().is_success() {
                    return Err(crate::Error::Config(format!(
                        "HTTP {} fetching {}: {}",
                        response.status().as_u16(),
                        url,
                        response.status().canonical_reason().unwrap_or("Unknown")
                    )));
                }

                response.text().await.map_err(|e| {
                    crate::Error::Config(format!("Failed to read response from {}: {}", url, e))
                })
            }
        }
    }

    /// Check if this is an in-memory source.
    pub fn is_in_memory(&self) -> bool {
        matches!(self, Self::InMemory { .. })
    }

    /// Check if this is a file source.
    pub fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }

    /// Get the file path if this is a file source.
    pub fn as_file_path(&self) -> Option<&PathBuf> {
        match self {
            Self::File { path } => Some(path),
            _ => None,
        }
    }

    /// Get the parent directory if this is a file source.
    pub fn base_dir(&self) -> Option<PathBuf> {
        match self {
            Self::File { path } => path.parent().map(|p| p.to_path_buf()),
            _ => None,
        }
    }
}

impl Default for ContentSource {
    fn default() -> Self {
        Self::InMemory {
            content: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_source_constructors() {
        let file = ContentSource::file("/path/to/file.md");
        assert!(file.is_file());
        assert_eq!(
            file.as_file_path(),
            Some(&PathBuf::from("/path/to/file.md"))
        );

        let memory = ContentSource::in_memory("content here");
        assert!(memory.is_in_memory());

        let http = ContentSource::http("https://example.com/skill.md");
        assert!(
            matches!(http, ContentSource::Http { url } if url == "https://example.com/skill.md")
        );
    }

    #[test]
    fn test_base_dir() {
        let file = ContentSource::file("/home/user/.claude/skills/commit/SKILL.md");
        assert_eq!(
            file.base_dir(),
            Some(PathBuf::from("/home/user/.claude/skills/commit"))
        );

        let memory = ContentSource::in_memory("content");
        assert_eq!(memory.base_dir(), None);
    }

    #[tokio::test]
    async fn test_load_in_memory() {
        let source = ContentSource::in_memory("test content");
        let content = source.load().await.unwrap();
        assert_eq!(content, "test content");
    }

    #[tokio::test]
    async fn test_load_file() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "file content").unwrap();

        let source = ContentSource::file(file.path());
        let content = source.load().await.unwrap();
        assert!(content.contains("file content"));
    }

    #[tokio::test]
    async fn test_load_file_not_found() {
        let source = ContentSource::file("/nonexistent/path/file.md");
        let result = source.load().await;
        assert!(result.is_err());
    }

    #[test]
    fn test_serde_roundtrip() {
        let sources = vec![
            ContentSource::file("/path/to/file.md"),
            ContentSource::in_memory("content"),
            ContentSource::http("https://example.com"),
        ];

        for source in sources {
            let json = serde_json::to_string(&source).unwrap();
            let parsed: ContentSource = serde_json::from_str(&json).unwrap();
            assert_eq!(source, parsed);
        }
    }
}
