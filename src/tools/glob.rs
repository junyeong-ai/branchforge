//! Glob tool - file pattern matching with sandbox validation.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use super::SchemaTool;
use super::context::ExecutionContext;
use crate::types::ToolResult;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct GlobInput {
    /// The glob pattern to match files against
    pub pattern: String,
    /// The directory to search in. If not specified, the current working directory will be used.
    /// IMPORTANT: Omit this field to use the default directory. DO NOT enter "undefined" or "null" -
    /// simply omit it for the default behavior. Must be a valid directory path if provided.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GlobTool;

#[async_trait]
impl SchemaTool for GlobTool {
    type Input = GlobInput;

    const NAME: &'static str = "Glob";
    const DESCRIPTION: &'static str = r#"- Fast file pattern matching tool that works with any codebase size
- Supports glob patterns like "**/*.js" or "src/**/*.ts"
- Returns matching file paths sorted by modification time
- Use this tool when you need to find files by name patterns
- When you are doing an open ended search that may require multiple rounds of globbing and grepping, prefer a delegated workflow only if it is explicitly enabled
- You can call multiple tools in a single response. It is always better to speculatively perform multiple searches in parallel if they are potentially useful."#;

    async fn handle(&self, input: GlobInput, context: &ExecutionContext) -> ToolResult {
        let base_path = match context.try_resolve_or_root_for(Self::NAME, input.path.as_deref()) {
            Ok(path) => path,
            Err(e) => return e,
        };

        let full_pattern = base_path.join(&input.pattern);
        let pattern_str = full_pattern.to_string_lossy().to_string();

        let glob_result = tokio::task::spawn_blocking(move || {
            glob::glob(&pattern_str).map(|paths| {
                paths
                    .filter_map(|r| r.ok())
                    .filter_map(|p| {
                        std::fs::canonicalize(&p).ok().and_then(|canonical| {
                            canonical
                                .metadata()
                                .ok()
                                .and_then(|m| m.modified().ok())
                                .map(|mtime| (canonical, mtime))
                        })
                    })
                    .collect::<Vec<_>>()
            })
        })
        .await;

        let all_entries = match glob_result {
            Ok(Ok(entries)) => entries,
            Ok(Err(e)) => return ToolResult::error(format!("Invalid pattern: {}", e)),
            Err(e) => return ToolResult::error(format!("Glob task failed: {}", e)),
        };

        let mut entries: Vec<_> = all_entries
            .into_iter()
            .filter(|(p, _)| context.is_within(p))
            .collect();

        if entries.is_empty() {
            return ToolResult::success("No files matched the pattern");
        }

        entries.sort_by(|a, b| b.1.cmp(&a.1));

        let output: Vec<String> = entries
            .into_iter()
            .map(|(p, _)| p.display().to_string())
            .collect();

        ToolResult::success(output.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use crate::types::ToolOutput;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn test_glob_pattern() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        fs::write(root.join("test1.txt"), "").await.unwrap();
        fs::write(root.join("test2.txt"), "").await.unwrap();
        fs::write(root.join("other.rs"), "").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = GlobTool;

        let result = tool
            .execute(serde_json::json!({"pattern": "*.txt"}), &test_context)
            .await;

        match &result.output {
            ToolOutput::Success(content) => {
                assert!(content.contains("test1.txt"));
                assert!(content.contains("test2.txt"));
                assert!(!content.contains("other.rs"));
            }
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_glob_recursive_pattern() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let subdir = root.join("src");
        fs::create_dir_all(&subdir).await.unwrap();
        fs::write(root.join("main.rs"), "fn main() {}")
            .await
            .unwrap();
        fs::write(subdir.join("lib.rs"), "pub mod lib;")
            .await
            .unwrap();
        fs::write(subdir.join("utils.rs"), "pub fn util() {}")
            .await
            .unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = GlobTool;

        let result = tool
            .execute(serde_json::json!({"pattern": "**/*.rs"}), &test_context)
            .await;

        match &result.output {
            ToolOutput::Success(content) => {
                assert!(content.contains("main.rs"));
                assert!(content.contains("lib.rs"));
                assert!(content.contains("utils.rs"));
            }
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_glob_no_matches() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        fs::write(root.join("test.txt"), "").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = GlobTool;

        let result = tool
            .execute(serde_json::json!({"pattern": "*.py"}), &test_context)
            .await;

        match &result.output {
            ToolOutput::Success(content) => {
                assert!(content.contains("No files matched"));
            }
            _ => panic!("Expected success with no matches message"),
        }
    }

    #[tokio::test]
    async fn test_glob_with_path() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let subdir = root.join("nested");
        fs::create_dir_all(&subdir).await.unwrap();
        fs::write(root.join("root.txt"), "").await.unwrap();
        fs::write(subdir.join("nested.txt"), "").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = GlobTool;

        let result = tool
            .execute(
                serde_json::json!({"pattern": "*.txt", "path": "nested"}),
                &test_context,
            )
            .await;

        match &result.output {
            ToolOutput::Success(content) => {
                assert!(content.contains("nested.txt"));
                assert!(!content.contains("root.txt"));
            }
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_glob_invalid_pattern() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = GlobTool;

        let result = tool
            .execute(serde_json::json!({"pattern": "[invalid"}), &test_context)
            .await;

        match &result.output {
            ToolOutput::Error(e) => {
                assert!(e.to_string().contains("Invalid pattern"));
            }
            _ => panic!("Expected error for invalid pattern"),
        }
    }

    #[tokio::test]
    async fn test_glob_sorted_by_mtime() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        fs::write(root.join("old.txt"), "old").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        fs::write(root.join("new.txt"), "new").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = GlobTool;

        let result = tool
            .execute(serde_json::json!({"pattern": "*.txt"}), &test_context)
            .await;

        match &result.output {
            ToolOutput::Success(content) => {
                let new_pos = content.find("new.txt").unwrap();
                let old_pos = content.find("old.txt").unwrap();
                assert!(new_pos < old_pos, "Newer file should appear first");
            }
            _ => panic!("Expected success"),
        }
    }

    #[test]
    fn test_glob_input_parsing() {
        let input: GlobInput = serde_json::from_value(serde_json::json!({
            "pattern": "**/*.rs",
            "path": "src"
        }))
        .unwrap();
        assert_eq!(input.pattern, "**/*.rs");
        assert_eq!(input.path, Some("src".to_string()));
    }

    #[tokio::test]
    async fn test_glob_path_traversal_blocked() {
        // Create parent and working directories
        let parent = tempdir().unwrap();
        let parent_path = std::fs::canonicalize(parent.path()).unwrap();

        let working_dir = parent_path.join("sandbox");
        std::fs::create_dir_all(&working_dir).unwrap();
        let sandbox_path = std::fs::canonicalize(&working_dir).unwrap();

        // Create files
        std::fs::write(parent_path.join("secret.txt"), "SECRET").unwrap();
        std::fs::write(sandbox_path.join("allowed.txt"), "allowed").unwrap();

        // Context with sandbox_path as root
        let test_context = ExecutionContext::from_path(&sandbox_path).unwrap();
        let tool = GlobTool;

        // Try to access parent directory via ../*.txt
        let result = tool
            .execute(serde_json::json!({"pattern": "../*.txt"}), &test_context)
            .await;

        match &result.output {
            ToolOutput::Success(content) => {
                // Should NOT find secret.txt (outside sandbox)
                assert!(
                    !content.contains("secret.txt"),
                    "Path traversal should be blocked! Found: {}",
                    content
                );
            }
            ToolOutput::Error(_) => {
                // Error is also acceptable
            }
            _ => panic!("Unexpected result"),
        }
    }
}
