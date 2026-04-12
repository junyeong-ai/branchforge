//! Rule Index for Progressive Disclosure
//!
//! Rules are loaded on-demand based on file path matching.
//! Only indices (metadata) are loaded at startup; full content is lazy-loaded.
//!
//! # Architecture
//!
//! RuleIndex implements both `Index` and `PathMatched` traits:
//! - `Index`: Provides lazy content loading and priority-based override
//! - `PathMatched`: Enables path-based filtering for context-sensitive rules
//!
//! Rules are stored in `IndexRegistry<RuleIndex>` and use `find_matching(path)`
//! to get all rules that apply to a specific file.

use std::path::Path;

use async_trait::async_trait;
use glob::Pattern;
use serde::{Deserialize, Serialize};

use crate::common::{
    ContentSource, Index, Named, PathMatched, SourceType, parse_frontmatter, strip_frontmatter,
};

/// Frontmatter schema for rule files.
///
/// Used with the generic `parse_frontmatter<RuleFrontmatter>()` parser.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RuleFrontmatter {
    /// Human-readable description of the rule.
    #[serde(default)]
    pub description: String,

    /// Path patterns this rule applies to (glob syntax).
    #[serde(default)]
    pub paths: Option<Vec<String>>,

    /// Explicit priority for ordering.
    #[serde(default)]
    pub priority: i32,
}

/// Rule index entry - minimal metadata for progressive disclosure.
///
/// Contains only metadata needed for system prompt injection.
/// Full rule content is loaded on-demand via `load_content()`.
///
/// # Serialization
///
/// `compiled_patterns` is a derived field rebuilt from `paths` on
/// deserialization via `#[serde(from = "RuleIndexRaw")]`. This guarantees
/// that path matching works correctly on a deserialized `RuleIndex`.
#[derive(Clone, Debug, Serialize)]
#[serde(into = "RuleIndexRaw")]
pub struct RuleIndex {
    /// Rule name (unique identifier).
    pub name: String,

    /// Human-readable description of what this rule does.
    pub description: String,

    /// Path patterns this rule applies to (glob syntax).
    /// `None` means this is a global rule that applies to all files.
    pub paths: Option<Vec<String>>,

    /// Compiled glob patterns for efficient matching.
    /// Derived from `paths`; rebuilt automatically on deserialization.
    compiled_patterns: Vec<Pattern>,

    /// Explicit priority for ordering. Higher values take precedence.
    /// This is separate from source_type-based priority in the Index trait.
    pub priority: i32,

    /// Content source for lazy loading.
    pub source: ContentSource,

    /// Source type (builtin, user, project).
    pub source_type: SourceType,
}

/// Wire format for `RuleIndex` (excludes derived `compiled_patterns`).
///
/// The `From<RuleIndexRaw> for RuleIndex` impl rebuilds `compiled_patterns`
/// from `paths`, ensuring deserialized indices match paths correctly.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct RuleIndexRaw {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    paths: Option<Vec<String>>,
    #[serde(default)]
    priority: i32,
    source: ContentSource,
    #[serde(default)]
    source_type: SourceType,
}

impl From<RuleIndexRaw> for RuleIndex {
    fn from(raw: RuleIndexRaw) -> Self {
        let compiled_patterns = compile_patterns(raw.paths.as_deref());
        Self {
            name: raw.name,
            description: raw.description,
            paths: raw.paths,
            compiled_patterns,
            priority: raw.priority,
            source: raw.source,
            source_type: raw.source_type,
        }
    }
}

impl From<RuleIndex> for RuleIndexRaw {
    fn from(idx: RuleIndex) -> Self {
        Self {
            name: idx.name,
            description: idx.description,
            paths: idx.paths,
            priority: idx.priority,
            source: idx.source,
            source_type: idx.source_type,
        }
    }
}

impl<'de> Deserialize<'de> for RuleIndex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RuleIndexRaw::deserialize(deserializer).map(Self::from)
    }
}

/// Compile glob patterns, dropping any that fail to parse.
fn compile_patterns(paths: Option<&[String]>) -> Vec<Pattern> {
    paths
        .map(|p| p.iter().filter_map(|s| Pattern::new(s).ok()).collect())
        .unwrap_or_default()
}

impl RuleIndex {
    /// Create a new rule index entry.
    ///
    /// Uses `ContentSource::default()` (empty InMemory) as placeholder.
    /// Call `source()` to set the actual content source.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            paths: None,
            compiled_patterns: Vec::new(),
            priority: 0,
            source: ContentSource::default(),
            source_type: SourceType::default(),
        }
    }

    /// Set the rule description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Set path patterns this rule applies to.
    pub fn paths(mut self, paths: Vec<String>) -> Self {
        self.compiled_patterns = compile_patterns(Some(&paths));
        self.paths = Some(paths);
        self
    }

    /// Set the explicit priority.
    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Set the content source.
    pub fn source(mut self, source: ContentSource) -> Self {
        self.source = source;
        self
    }

    /// Set the source type.
    pub fn source_type(mut self, source_type: SourceType) -> Self {
        self.source_type = source_type;
        self
    }

    /// Phase I-2: Load rule from a file path using asynchronous
    /// file I/O. Previously used `std::fs::read_to_string`, which
    /// blocked the tokio executor when called from the
    /// [`crate::context::memory_loader::MemoryLoader`] scan loop
    /// (itself built on `tokio::fs::read_dir`). The sync/async
    /// mismatch caused a real concurrency bug — any project with
    /// many rule files could stall the runtime. Converting to
    /// `tokio::fs::read_to_string` fixes this without changing
    /// the public return type (still `Option<Self>`).
    pub async fn from_file(path: &Path) -> Option<Self> {
        let content = tokio::fs::read_to_string(path).await.ok()?;
        Self::parse_with_frontmatter(&content, path)
    }

    /// Parse rule from content with frontmatter.
    ///
    /// Uses the generic `parse_frontmatter<RuleFrontmatter>()` parser.
    /// Falls back to defaults if frontmatter is missing or invalid.
    pub fn parse_with_frontmatter(content: &str, path: &Path) -> Option<Self> {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Try parsing frontmatter, use defaults if missing/invalid
        let fm = parse_frontmatter::<RuleFrontmatter>(content)
            .map(|doc| doc.frontmatter)
            .unwrap_or_default();

        let compiled_patterns = compile_patterns(fm.paths.as_deref());

        Some(Self {
            name,
            description: fm.description,
            paths: fm.paths,
            compiled_patterns,
            priority: fm.priority,
            source: ContentSource::file(path),
            source_type: SourceType::default(),
        })
    }
}

// ============================================================================
// Trait implementations
// ============================================================================

impl Named for RuleIndex {
    fn name(&self) -> &str {
        &self.name
    }
}

#[async_trait]
impl Index for RuleIndex {
    fn source(&self) -> &ContentSource {
        &self.source
    }

    fn source_type(&self) -> SourceType {
        self.source_type
    }

    /// Override priority to use explicit field instead of source_type-based.
    ///
    /// Rules need explicit ordering independent of their source type.
    fn priority(&self) -> i32 {
        self.priority
    }

    fn to_summary_line(&self) -> String {
        let scope = match &self.paths {
            Some(p) if !p.is_empty() => p.join(", "),
            _ => "all files".to_string(),
        };
        if self.description.is_empty() {
            format!("- {}: applies to {}", self.name, scope)
        } else {
            format!(
                "- {} ({}): applies to {}",
                self.name, self.description, scope
            )
        }
    }

    fn description(&self) -> &str {
        &self.description
    }

    async fn load_content(&self) -> crate::Result<String> {
        let content = self.source.load().await.map_err(|e| {
            crate::Error::Config(format!("Failed to load rule '{}': {}", self.name, e))
        })?;

        // Strip frontmatter for file sources
        if self.source.is_file() {
            Ok(strip_frontmatter(&content).to_string())
        } else {
            Ok(content)
        }
    }
}

impl PathMatched for RuleIndex {
    fn path_patterns(&self) -> Option<&[String]> {
        self.paths.as_deref()
    }

    fn matches_path(&self, file_path: &Path) -> bool {
        if self.compiled_patterns.is_empty() {
            return true; // Global rule matches all files
        }
        let path_str = file_path.to_string_lossy();
        self.compiled_patterns.iter().any(|p| p.matches(&path_str))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::fs;

    #[test]
    fn test_rule_index_creation() {
        let rule = RuleIndex::new("typescript")
            .description("TypeScript coding standards")
            .paths(vec!["**/*.ts".into(), "**/*.tsx".into()])
            .priority(10);

        assert_eq!(rule.name, "typescript");
        assert_eq!(rule.description, "TypeScript coding standards");
        assert_eq!(rule.priority, 10);
    }

    #[test]
    fn test_path_matching() {
        let rule = RuleIndex::new("rust").paths(vec!["**/*.rs".into()]);

        assert!(rule.matches_path(Path::new("src/lib.rs")));
        assert!(rule.matches_path(Path::new("src/context/mod.rs")));
        assert!(!rule.matches_path(Path::new("src/lib.ts")));
    }

    #[test]
    fn test_global_rule() {
        let rule = RuleIndex::new("security");
        assert!(rule.is_global());
        assert!(rule.matches_path(Path::new("any/file.rs")));
        assert!(rule.matches_path(Path::new("another/file.js")));
    }

    #[test]
    fn test_frontmatter_parsing() {
        let content = r#"---
description: "Rust coding standards"
paths:
  - src/**/*.rs
  - tests/**/*.rs
priority: 10
---

# Rust Guidelines
Use snake_case for variables."#;

        // Use the generic parser via parse_with_frontmatter
        let rule =
            RuleIndex::parse_with_frontmatter(content, std::path::Path::new("test.md")).unwrap();
        assert_eq!(rule.priority, 10);
        assert_eq!(rule.description, "Rust coding standards");
        assert!(rule.paths.is_some());
        let paths = rule.paths.unwrap();
        assert!(paths.contains(&"src/**/*.rs".to_string()));
        assert!(paths.contains(&"tests/**/*.rs".to_string()));
    }

    #[test]
    fn test_strip_frontmatter() {
        let content = r#"---
paths: src/**/*.rs
---

# Content"#;

        // Use the common strip_frontmatter function
        let stripped = strip_frontmatter(content);
        assert_eq!(stripped, "# Content");
    }

    #[tokio::test]
    async fn test_lazy_loading() {
        let dir = tempdir().unwrap();
        let rule_path = dir.path().join("test.md");
        fs::write(
            &rule_path,
            r#"---
description: "Test rule"
paths:
  - "**/*.rs"
priority: 5
---

# Test Rule Content"#,
        )
        .await
        .unwrap();

        let index = RuleIndex::from_file(&rule_path).await.unwrap();
        assert_eq!(index.name, "test");
        assert_eq!(index.description, "Test rule");
        assert_eq!(index.priority, 5);

        let content = index.load_content().await.expect("Should load content");
        assert_eq!(content, "# Test Rule Content");
    }

    #[test]
    fn test_summary_line_with_description() {
        let rule = RuleIndex::new("security")
            .description("Security best practices")
            .paths(vec!["**/*.rs".into()]);

        let summary = rule.to_summary_line();
        assert!(summary.contains("security"));
        assert!(summary.contains("Security best practices"));
        assert!(summary.contains("**/*.rs"));
    }

    #[test]
    fn test_summary_line_without_description() {
        let rule = RuleIndex::new("global-rule");
        let summary = rule.to_summary_line();
        assert_eq!(summary, "- global-rule: applies to all files");
    }

    #[test]
    fn test_serde_roundtrip_recompiles_patterns() {
        // Regression test for the bug where #[serde(skip)] on
        // compiled_patterns left deserialized indices unable to match paths.
        let original = RuleIndex::new("rust")
            .description("Rust style")
            .paths(vec!["**/*.rs".into()])
            .source(ContentSource::in_memory("body"))
            .source_type(SourceType::User);

        let json = serde_json::to_string(&original).expect("serialize");
        let restored: RuleIndex = serde_json::from_str(&json).expect("deserialize");

        // The deserialized index must match paths, not silently fall back
        // to the global rule (compiled_patterns.is_empty()) branch.
        assert!(restored.matches_path(Path::new("src/lib.rs")));
        assert!(restored.matches_path(Path::new("src/context/mod.rs")));
        assert!(!restored.matches_path(Path::new("src/lib.ts")));
        assert!(!restored.is_global());
        assert_eq!(restored.name, "rust");
        assert_eq!(restored.priority, 0);
    }

    #[test]
    fn test_serde_roundtrip_global_rule() {
        // Global rule (no paths) should remain global after roundtrip.
        let original = RuleIndex::new("global").source(ContentSource::in_memory("body"));
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: RuleIndex = serde_json::from_str(&json).expect("deserialize");
        assert!(restored.is_global());
        assert!(restored.matches_path(Path::new("any/file.rs")));
    }

    #[test]
    fn test_priority_override() {
        // Priority should be explicit, not source_type-based
        let rule = RuleIndex::new("test")
            .priority(100)
            .source_type(SourceType::Builtin); // Builtin would be 0 normally

        assert_eq!(rule.priority, 100); // Should use explicit priority
    }

    #[test]
    fn test_implements_index_and_path_matched() {
        use crate::common::{Index, PathMatched};

        let rule = RuleIndex::new("test")
            .description("Test")
            .paths(vec!["**/*.rs".into()])
            .source_type(SourceType::User)
            .source(ContentSource::in_memory("Rule content"));

        // Index trait
        assert_eq!(rule.name(), "test");
        assert_eq!(rule.source_type, SourceType::User);
        assert!(rule.to_summary_line().contains("test"));
        assert_eq!(rule.description, "Test");

        // PathMatched trait
        assert!(!rule.is_global());
        assert!(rule.matches_path(Path::new("src/lib.rs")));
        assert!(!rule.matches_path(Path::new("src/lib.ts")));
    }
}
