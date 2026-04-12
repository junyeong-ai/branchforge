//! CLAUDE.md and CLAUDE.local.md loader with CLI-compatible @import processing.
//!
//! This module provides a memory loader that reads CLAUDE.md and CLAUDE.local.md files
//! with support for recursive @import directives. It implements the same import behavior
//! as Claude Code CLI 2.1.12.
//!
//! # Layering
//!
//! This module is Layer 2a (`local-fs`) — it walks a real filesystem tree
//! and reads markdown files. A local research or knowledge-management
//! agent can legitimately use it (the mechanism is "scan a directory for
//! markdown instructions"); the only Claude Code-specific aspects are the
//! hard-coded file names (`CLAUDE.md`, `CLAUDE.local.md`, `.claude/rules/`).
//! A future refinement will split this into a truly domain-neutral
//! `MarkdownMemoryLoader` (Layer 2a) that accepts a configurable glob and
//! an opinionated Claude Code-convention wrapper (Layer 2b, `coding-tools`).
//! Pure-core builds have no access to either loader; they use
//! [`super::MemoryContent`] (Layer 1) as a data carrier and populate it via
//! in-memory providers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::import_extractor::ImportExtractor;
use super::memory_content::MemoryContent;
use super::rule_index::RuleIndex;
use super::{ContextError, ContextResult};

/// Default maximum import depth for CLI-like behavior.
/// CLI uses depth 5 technically but loads ~24K tokens of memory.
/// Depth 2 gives ~31K tokens which is the closest match.
pub(crate) const DEFAULT_IMPORT_DEPTH: usize = 2;

/// Maximum import depth when full expansion is needed (CLI's technical limit).
pub(crate) const MAX_IMPORT_DEPTH: usize = 5;

/// Configuration for MemoryLoader.
#[derive(Debug, Clone)]
pub struct MemoryLoaderConfig {
    /// Maximum import depth (default: 2 for CLI-like token counts).
    /// Use MAX_IMPORT_DEPTH (5) for full expansion.
    pub max_depth: usize,
}

impl Default for MemoryLoaderConfig {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_IMPORT_DEPTH,
        }
    }
}

impl MemoryLoaderConfig {
    /// Creates config with full import expansion (depth 5).
    pub fn full_expansion() -> Self {
        Self {
            max_depth: MAX_IMPORT_DEPTH,
        }
    }

    /// Creates config with specified max depth.
    pub fn max_depth(max_depth: usize) -> Self {
        Self { max_depth }
    }
}

/// Memory loader with CLI-compatible @import processing.
///
/// # Features
/// - Loads CLAUDE.md and CLAUDE.local.md from project directories
/// - Supports recursive @import with depth limiting
/// - Circular import detection using canonical path tracking
/// - Scans .claude/rules/ directory for rule files
///
/// # CLI Compatibility
/// This implementation matches Claude Code CLI 2.1.12 behavior:
/// - Maximum import depth of 5 (configurable, default 3 for similar token counts)
/// - Same path validation rules
/// - Same circular import prevention
pub struct MemoryLoader {
    extractor: ImportExtractor,
    config: MemoryLoaderConfig,
}

impl MemoryLoader {
    /// Creates a new MemoryLoader with default configuration (depth 3).
    pub fn new() -> Self {
        Self::from_config(MemoryLoaderConfig::default())
    }

    /// Creates a new MemoryLoader with custom configuration.
    pub fn from_config(config: MemoryLoaderConfig) -> Self {
        Self {
            extractor: ImportExtractor::new(),
            config,
        }
    }

    /// Creates a new MemoryLoader with full import expansion (depth 5).
    pub fn full_expansion() -> Self {
        Self::from_config(MemoryLoaderConfig::full_expansion())
    }

    /// Loads all memory content (CLAUDE.md + CLAUDE.local.md + rules) from a directory.
    ///
    /// # Arguments
    /// * `start_dir` - The project root directory to load from
    ///
    /// # Returns
    /// Combined MemoryContent with all loaded files and rules
    pub async fn load(&self, start_dir: &Path) -> ContextResult<MemoryContent> {
        let mut content = self.load_shared(start_dir).await?;
        let local = self.load_local(start_dir).await?;
        content.merge(local);
        Ok(content)
    }

    /// Loads shared CLAUDE.md and rules (visible to all team members).
    pub async fn load_shared(&self, start_dir: &Path) -> ContextResult<MemoryContent> {
        let mut content = MemoryContent::default();
        let mut visited = HashSet::new();

        for path in Self::find_claude_files(start_dir) {
            match self
                .load_with_imports(&path, start_dir, 0, &mut visited)
                .await
            {
                Ok(text) => content.claude_md.push(text),
                Err(e) => tracing::debug!("Failed to load {}: {}", path.display(), e),
            }
        }

        let rules_dir = start_dir.join(".claude").join("rules");
        if rules_dir.exists() {
            content.rule_indices = self.scan_rules(&rules_dir).await?;
        }

        Ok(content)
    }

    /// Loads local CLAUDE.local.md (private to the user, not in version control).
    pub async fn load_local(&self, start_dir: &Path) -> ContextResult<MemoryContent> {
        let mut content = MemoryContent::default();
        let mut visited = HashSet::new();

        for path in Self::find_local_files(start_dir) {
            match self
                .load_with_imports(&path, start_dir, 0, &mut visited)
                .await
            {
                Ok(text) => content.local_md.push(text),
                Err(e) => tracing::debug!("Failed to load {}: {}", path.display(), e),
            }
        }

        Ok(content)
    }

    /// Loads a file with recursive @import expansion.
    ///
    /// # Arguments
    /// * `path` - Path to the file to load
    /// * `project_root` - Project root directory for resolving @.agents/... style imports
    /// * `depth` - Current import depth (0 = root)
    /// * `visited` - Set of canonical paths already loaded (for circular detection)
    ///
    /// # Returns
    /// File content with all imports expanded inline
    fn load_with_imports<'a>(
        &'a self,
        path: &'a Path,
        project_root: &'a Path,
        depth: usize,
        visited: &'a mut HashSet<PathBuf>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ContextResult<String>> + Send + 'a>>
    {
        Box::pin(async move {
            // Depth limit check (configurable, default 3)
            if depth > self.config.max_depth {
                tracing::warn!(
                    "Import depth limit ({}) exceeded, skipping: {}",
                    self.config.max_depth,
                    path.display()
                );
                return Ok(String::new());
            }

            // Circular import detection using canonical paths
            let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            if visited.contains(&canonical) {
                tracing::debug!("Circular import detected, skipping: {}", path.display());
                return Ok(String::new());
            }
            visited.insert(canonical);

            let content =
                tokio::fs::read_to_string(path)
                    .await
                    .map_err(|e| ContextError::Source {
                        message: format!("Failed to read {}: {}", path.display(), e),
                    })?;

            // Use current file's directory for relative path resolution
            let file_dir = path.parent().unwrap_or(Path::new("."));
            let imports = self.extractor.extract(&content, file_dir);

            // Post-process: fix duplicated .agents/ or .claude/ paths
            // e.g., /project/.agents/guides/.agents/patterns -> .agents/patterns
            let imports: Vec<PathBuf> = imports
                .into_iter()
                .map(|p| Self::normalize_project_relative_path(&p, project_root))
                .collect();

            let mut result = content;
            for import_path in imports {
                if import_path.exists() {
                    if let Ok(imported) = self
                        .load_with_imports(&import_path, project_root, depth + 1, visited)
                        .await
                        && !imported.is_empty()
                    {
                        result.push_str("\n\n");
                        result.push_str(&imported);
                    }
                } else {
                    tracing::debug!("Import not found, skipping: {}", import_path.display());
                }
            }

            Ok(result)
        })
    }

    /// Finds CLAUDE.md files in standard locations.
    fn find_claude_files(start_dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();

        // Project root CLAUDE.md
        let claude_md = start_dir.join("CLAUDE.md");
        if claude_md.exists() {
            files.push(claude_md);
        }

        // .claude/CLAUDE.md (alternative location)
        let claude_dir_md = start_dir.join(".claude").join("CLAUDE.md");
        if claude_dir_md.exists() {
            files.push(claude_dir_md);
        }

        files
    }

    /// Finds CLAUDE.local.md files in standard locations.
    fn find_local_files(start_dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();

        // Project root CLAUDE.local.md
        let local_md = start_dir.join("CLAUDE.local.md");
        if local_md.exists() {
            files.push(local_md);
        }

        // .claude/CLAUDE.local.md (alternative location)
        let local_dir_md = start_dir.join(".claude").join("CLAUDE.local.md");
        if local_dir_md.exists() {
            files.push(local_dir_md);
        }

        files
    }

    /// Normalizes paths with duplicated .agents/ or .claude/ segments.
    ///
    /// When imports are resolved from nested files (e.g., .agents/guides/workflow.md),
    /// relative paths like @.agents/patterns/... get incorrectly expanded to
    /// /project/.agents/guides/.agents/patterns/... (duplicated .agents/).
    ///
    /// This function detects such duplications and re-resolves from project root.
    fn normalize_project_relative_path(path: &Path, project_root: &Path) -> PathBuf {
        const MARKERS: [&str; 2] = ["/.agents/", "/.claude/"];

        let path_str = path.to_string_lossy();

        // Find the last occurrence of any project-relative marker
        let last_marker_pos = MARKERS
            .iter()
            .filter_map(|marker| {
                let count = path_str.matches(marker).count();
                // Only fix if duplicated (count > 1) or mixed markers exist
                if count > 1 || MARKERS.iter().filter(|m| path_str.contains(*m)).count() > 1 {
                    path_str.rfind(marker).map(|pos| (pos, *marker))
                } else {
                    None
                }
            })
            .max_by_key(|(pos, _)| *pos);

        if let Some((idx, _)) = last_marker_pos {
            let relative_part = &path_str[idx + 1..]; // Skip leading "/"
            project_root.join(relative_part)
        } else {
            path.to_path_buf()
        }
    }

    /// Scans .claude/rules/ directory recursively for rule files.
    async fn scan_rules(&self, dir: &Path) -> ContextResult<Vec<RuleIndex>> {
        let mut indices = Vec::new();
        self.scan_rules_recursive(dir, &mut indices).await?;
        indices.sort_by(|a, b| b.priority.cmp(&a.priority));
        Ok(indices)
    }

    fn scan_rules_recursive<'a>(
        &'a self,
        dir: &'a Path,
        indices: &'a mut Vec<RuleIndex>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ContextResult<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut entries = tokio::fs::read_dir(dir)
                .await
                .map_err(|e| ContextError::Source {
                    message: format!("Failed to read rules directory: {}", e),
                })?;

            while let Some(entry) =
                entries
                    .next_entry()
                    .await
                    .map_err(|e| ContextError::Source {
                        message: format!("Failed to read directory entry: {}", e),
                    })?
            {
                let path = entry.path();

                if path.is_dir() {
                    self.scan_rules_recursive(&path, indices).await?;
                } else if path.extension().is_some_and(|e| e == "md")
                    && let Some(index) = RuleIndex::from_file(&path).await
                {
                    indices.push(index);
                }
            }

            Ok(())
        })
    }
}

impl Default for MemoryLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn test_load_claude_md() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "# Project\nTest content")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        assert_eq!(content.claude_md.len(), 1);
        assert!(content.claude_md[0].contains("Test content"));
    }

    #[tokio::test]
    async fn test_load_local_md() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.local.md"), "# Local\nPrivate")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        assert_eq!(content.local_md.len(), 1);
        assert!(content.local_md[0].contains("Private"));
    }

    #[tokio::test]
    async fn test_scan_rules_recursive() {
        let dir = tempdir().unwrap();
        let rules_dir = dir.path().join(".claude").join("rules");
        let sub_dir = rules_dir.join("frontend");
        fs::create_dir_all(&sub_dir).await.unwrap();

        fs::write(
            rules_dir.join("rust.md"),
            "---\npaths: **/*.rs\npriority: 10\n---\n\n# Rust Rules",
        )
        .await
        .unwrap();

        fs::write(
            sub_dir.join("react.md"),
            "---\npaths: **/*.tsx\npriority: 5\n---\n\n# React Rules",
        )
        .await
        .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        assert_eq!(content.rule_indices.len(), 2);
        assert!(content.rule_indices.iter().any(|r| r.name == "rust"));
        assert!(content.rule_indices.iter().any(|r| r.name == "react"));
    }

    #[tokio::test]
    async fn test_import_syntax() {
        let dir = tempdir().unwrap();

        fs::write(
            dir.path().join("CLAUDE.md"),
            "# Main\n@docs/guidelines.md\nEnd",
        )
        .await
        .unwrap();

        let docs_dir = dir.path().join("docs");
        fs::create_dir_all(&docs_dir).await.unwrap();
        fs::write(docs_dir.join("guidelines.md"), "Imported content")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        assert!(content.combined_claude_md().contains("Imported content"));
    }

    #[tokio::test]
    async fn test_combined_includes_local() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "Main content")
            .await
            .unwrap();
        fs::write(dir.path().join("CLAUDE.local.md"), "Local content")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        let combined = content.combined_claude_md();
        assert!(combined.contains("Main content"));
        assert!(combined.contains("Local content"));
    }

    #[tokio::test]
    async fn test_recursive_import() {
        let dir = tempdir().unwrap();

        // CLAUDE.md → docs/guide.md → docs/detail.md
        fs::write(dir.path().join("CLAUDE.md"), "Root content @docs/guide.md")
            .await
            .unwrap();

        let docs_dir = dir.path().join("docs");
        fs::create_dir_all(&docs_dir).await.unwrap();
        fs::write(docs_dir.join("guide.md"), "Guide content @detail.md")
            .await
            .unwrap();
        fs::write(docs_dir.join("detail.md"), "Detail content")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();
        let combined = content.combined_claude_md();

        assert!(combined.contains("Root content"));
        assert!(combined.contains("Guide content"));
        assert!(combined.contains("Detail content"));
    }

    #[tokio::test]
    async fn test_depth_limit_default() {
        let dir = tempdir().unwrap();

        // Create chain: CLAUDE.md → level1.md → level2.md → level3.md
        // With default depth 2, should stop at level 2 (0,1,2 loaded, 3 not)
        fs::write(dir.path().join("CLAUDE.md"), "Level 0 @level1.md")
            .await
            .unwrap();

        for i in 1..=3 {
            let content = if i < 3 {
                format!("Level {} @level{}.md", i, i + 1)
            } else {
                format!("Level {}", i)
            };
            fs::write(dir.path().join(format!("level{}.md", i)), content)
                .await
                .unwrap();
        }

        let loader = MemoryLoader::new(); // Default depth 2
        let content = loader.load(dir.path()).await.unwrap();
        let combined = content.combined_claude_md();

        // Should have levels 0-2 but NOT level 3 (default depth limit = 2)
        assert!(combined.contains("Level 0"));
        assert!(combined.contains("Level 2"));
        assert!(!combined.contains("Level 3"));
    }

    #[tokio::test]
    async fn test_depth_limit_full_expansion() {
        let dir = tempdir().unwrap();

        // Create chain: CLAUDE.md → level1.md → level2.md → ... → level6.md
        // With full expansion (depth 5), should stop at level 5 (0-5 loaded, 6 not)
        fs::write(dir.path().join("CLAUDE.md"), "Level 0 @level1.md")
            .await
            .unwrap();

        for i in 1..=6 {
            let content = if i < 6 {
                format!("Level {} @level{}.md", i, i + 1)
            } else {
                format!("Level {}", i)
            };
            fs::write(dir.path().join(format!("level{}.md", i)), content)
                .await
                .unwrap();
        }

        let loader = MemoryLoader::full_expansion(); // Depth 5
        let content = loader.load(dir.path()).await.unwrap();
        let combined = content.combined_claude_md();

        // Should have levels 0-5 but NOT level 6 (max depth limit = 5)
        assert!(combined.contains("Level 0"));
        assert!(combined.contains("Level 5"));
        assert!(!combined.contains("Level 6"));
    }

    #[tokio::test]
    async fn test_depth_limit_custom() {
        let dir = tempdir().unwrap();

        // Create chain up to level 3
        fs::write(dir.path().join("CLAUDE.md"), "Level 0 @level1.md")
            .await
            .unwrap();

        for i in 1..=3 {
            let content = if i < 3 {
                format!("Level {} @level{}.md", i, i + 1)
            } else {
                format!("Level {}", i)
            };
            fs::write(dir.path().join(format!("level{}.md", i)), content)
                .await
                .unwrap();
        }

        // Custom depth 1: should only load levels 0-1
        let loader = MemoryLoader::from_config(MemoryLoaderConfig::max_depth(1));
        let content = loader.load(dir.path()).await.unwrap();
        let combined = content.combined_claude_md();

        assert!(combined.contains("Level 0"));
        assert!(combined.contains("Level 1"));
        assert!(!combined.contains("Level 2"));
    }

    #[tokio::test]
    async fn test_circular_import() {
        let dir = tempdir().unwrap();

        // CLAUDE.md → a.md → b.md → a.md (circular)
        fs::write(dir.path().join("CLAUDE.md"), "Root @a.md")
            .await
            .unwrap();
        fs::write(dir.path().join("a.md"), "A content @b.md")
            .await
            .unwrap();
        fs::write(dir.path().join("b.md"), "B content @a.md")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let result = loader.load(dir.path()).await;

        // Should not infinite loop and should succeed
        assert!(result.is_ok());
        let combined = result.unwrap().combined_claude_md();
        assert!(combined.contains("A content"));
        assert!(combined.contains("B content"));
    }

    #[tokio::test]
    async fn test_import_in_code_block_ignored() {
        let dir = tempdir().unwrap();

        fs::write(
            dir.path().join("CLAUDE.md"),
            "# Example\n```\n@should/not/import.md\n```\n@should/import.md",
        )
        .await
        .unwrap();

        fs::write(
            dir.path().join("should").join("import.md"),
            "This is imported",
        )
        .await
        .ok();
        let should_dir = dir.path().join("should");
        fs::create_dir_all(&should_dir).await.unwrap();
        fs::write(should_dir.join("import.md"), "Imported content")
            .await
            .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();
        let combined = content.combined_claude_md();

        assert!(combined.contains("Imported content"));
        // The @should/not/import.md in code block should remain as-is, not be processed
        assert!(combined.contains("@should/not/import.md"));
    }

    #[tokio::test]
    async fn test_missing_import_ignored() {
        let dir = tempdir().unwrap();

        fs::write(
            dir.path().join("CLAUDE.md"),
            "# Main\n@nonexistent/file.md\nRest of content",
        )
        .await
        .unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();
        let combined = content.combined_claude_md();

        // Should still load the main content even if import doesn't exist
        assert!(combined.contains("# Main"));
        assert!(combined.contains("Rest of content"));
    }

    #[tokio::test]
    async fn test_empty_content() {
        let dir = tempdir().unwrap();

        let loader = MemoryLoader::new();
        let content = loader.load(dir.path()).await.unwrap();

        assert!(content.is_empty());
        assert!(content.combined_claude_md().is_empty());
    }

    #[tokio::test]
    async fn test_memory_content_merge() {
        let mut content1 = MemoryContent {
            claude_md: vec!["content1".to_string()],
            local_md: vec!["local1".to_string()],
            rule_indices: vec![],
        };

        let content2 = MemoryContent {
            claude_md: vec!["content2".to_string()],
            local_md: vec!["local2".to_string()],
            rule_indices: vec![],
        };

        content1.merge(content2);

        assert_eq!(content1.claude_md.len(), 2);
        assert_eq!(content1.local_md.len(), 2);
    }
}
