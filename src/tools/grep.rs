//! Grep tool - content search with regex using ripgrep.

use std::process::Stdio;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::process::Command;

use super::SchemaTool;
use super::context::ExecutionContext;
use crate::types::ToolResult;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct GrepInput {
    /// The regular expression pattern to search for in file contents
    pub pattern: String,
    /// File or directory to search in. Defaults to current working directory.
    #[serde(default)]
    pub path: Option<String>,
    /// Glob pattern to filter files (e.g. "*.js", "*.{ts,tsx}")
    #[serde(default)]
    pub glob: Option<String>,
    /// File type to search (e.g. "js", "py", "rust", "go")
    #[serde(default, rename = "type")]
    pub file_type: Option<String>,
    /// Output mode: "files_with_matches" (default), "content", or "count"
    #[serde(default)]
    pub output_mode: Option<String>,
    /// Case insensitive search
    #[serde(default, rename = "-i")]
    pub case_insensitive: Option<bool>,
    /// Lines of context around each match (rg -C). Only applies to output_mode: "content".
    #[serde(default)]
    pub context: Option<u32>,
    /// Enable multiline matching where patterns can span lines. Default: false.
    #[serde(default)]
    pub multiline: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GrepTool;

#[async_trait]
impl SchemaTool for GrepTool {
    type Input = GrepInput;

    const NAME: &'static str = "Grep";
    const DESCRIPTION: &'static str = r#"A powerful search tool built on ripgrep

  Usage:
  - ALWAYS use Grep for search tasks. NEVER invoke `grep` or `rg` as a Bash command. The Grep tool has been optimized for correct permissions and access.
  - Supports full regex syntax (e.g., "log.*Error", "function\s+\w+")
  - Filter files with glob parameter (e.g., "*.js", "**/*.tsx") or type parameter (e.g., "js", "py", "rust")
  - Output modes: "content" shows matching lines, "files_with_matches" shows only file paths (default), "count" shows match counts
  - Prefer a delegated workflow only when it is explicitly enabled for open-ended searches requiring multiple rounds
  - Pattern syntax: Uses ripgrep (not grep) - literal braces need escaping (use `interface\{\}` to find `interface{}` in Go code)
  - Multiline matching: By default patterns match within single lines only. For cross-line patterns like `struct \{[\s\S]*?field`, use `multiline: true`"#;

    async fn handle(&self, input: GrepInput, context: &ExecutionContext) -> ToolResult {
        let search_path = match context.try_resolve_or_root_for(Self::NAME, input.path.as_deref()) {
            Ok(path) => path,
            Err(e) => return e,
        };

        let mut cmd = Command::new("rg");

        match input.output_mode.as_deref() {
            Some("content") => {
                cmd.arg("-n"); // Always show line numbers in content mode
            }
            Some("files_with_matches") | None => {
                cmd.arg("-l");
            }
            Some("count") => {
                cmd.arg("-c");
            }
            Some(mode) => {
                return ToolResult::error(format!("Unknown output_mode: {}", mode));
            }
        }

        if input.case_insensitive.unwrap_or(false) {
            cmd.arg("-i");
        }

        if let Some(c) = input.context {
            cmd.arg("-C").arg(c.to_string());
        }

        if let Some(t) = &input.file_type {
            cmd.arg("-t").arg(t);
        }

        if let Some(g) = &input.glob {
            cmd.arg("-g").arg(g);
        }

        if input.multiline.unwrap_or(false) {
            cmd.arg("-U").arg("--multiline-dotall");
        }

        cmd.arg(&input.pattern);
        cmd.arg(&search_path);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) => {
                return ToolResult::error(format!(
                    "Failed to execute ripgrep (is rg installed?): {}",
                    e
                ));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() && !stderr.is_empty() {
            return ToolResult::error(format!("Ripgrep error: {}", stderr));
        }

        if stdout.is_empty() {
            return ToolResult::success("No matches found");
        }

        ToolResult::success(stdout.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use tempfile::tempdir;
    use tokio::fs;

    #[test]
    fn test_grep_input_parsing() {
        let input: GrepInput = serde_json::from_value(serde_json::json!({
            "pattern": "test",
            "-i": true
        }))
        .unwrap();

        assert_eq!(input.pattern, "test");
        assert_eq!(input.case_insensitive, Some(true));
    }

    #[test]
    fn test_grep_input_all_options() {
        let input: GrepInput = serde_json::from_value(serde_json::json!({
            "pattern": "fn main",
            "path": "src",
            "glob": "*.rs",
            "type": "rust",
            "output_mode": "content",
            "-i": false,
            "context": 3,
            "multiline": true,
        }))
        .unwrap();

        assert_eq!(input.pattern, "fn main");
        assert_eq!(input.path, Some("src".to_string()));
        assert_eq!(input.glob, Some("*.rs".to_string()));
        assert_eq!(input.file_type, Some("rust".to_string()));
        assert_eq!(input.output_mode, Some("content".to_string()));
        assert_eq!(input.case_insensitive, Some(false));
        assert_eq!(input.context, Some(3));
        assert_eq!(input.multiline, Some(true));
    }

    #[tokio::test]
    async fn test_grep_basic_search() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        fs::write(
            root.join("test.rs"),
            "fn main() {\n    println!(\"hello\");\n}",
        )
        .await
        .unwrap();
        fs::write(root.join("lib.rs"), "pub fn helper() {}")
            .await
            .unwrap();

        let test_context = super::super::context::ExecutionContext::from_path(&root).unwrap();
        let tool = GrepTool;

        // Default output_mode is now files_with_matches, so it returns file paths
        let result = tool
            .execute(serde_json::json!({"pattern": "fn main"}), &test_context)
            .await;

        match &result.output {
            crate::types::ToolOutput::Success(content) => {
                assert!(content.contains("test.rs"));
            }
            crate::types::ToolOutput::Error(e) => {
                let error_message = e.to_string();
                if error_message.contains("is rg installed") {
                    return;
                }
                panic!("Unexpected error: {}", error_message);
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn test_grep_no_matches() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        fs::write(root.join("test.txt"), "hello world")
            .await
            .unwrap();

        let test_context = super::super::context::ExecutionContext::from_path(&root).unwrap();
        let tool = GrepTool;

        let result = tool
            .execute(
                serde_json::json!({"pattern": "nonexistent_pattern_xyz"}),
                &test_context,
            )
            .await;

        match &result.output {
            crate::types::ToolOutput::Success(content) => {
                assert!(content.contains("No matches"));
            }
            crate::types::ToolOutput::Error(e) => {
                let error_message = e.to_string();
                if error_message.contains("is rg installed") {
                    return;
                }
                panic!("Unexpected error: {}", error_message);
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn test_grep_case_insensitive() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        fs::write(root.join("test.txt"), "Hello World\nHELLO WORLD")
            .await
            .unwrap();

        let test_context = super::super::context::ExecutionContext::from_path(&root).unwrap();
        let tool = GrepTool;

        let result = tool
            .execute(
                serde_json::json!({"pattern": "hello", "-i": true, "output_mode": "content"}),
                &test_context,
            )
            .await;

        match &result.output {
            crate::types::ToolOutput::Success(content) => {
                assert!(content.contains("Hello") || content.contains("HELLO"));
            }
            crate::types::ToolOutput::Error(e) => {
                let error_message = e.to_string();
                if error_message.contains("is rg installed") {
                    return;
                }
                panic!("Unexpected error: {}", error_message);
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn test_grep_files_with_matches_mode() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        fs::write(root.join("a.txt"), "pattern here").await.unwrap();
        fs::write(root.join("b.txt"), "no match").await.unwrap();

        let test_context = super::super::context::ExecutionContext::from_path(&root).unwrap();
        let tool = GrepTool;

        let result = tool
            .execute(
                serde_json::json!({"pattern": "pattern", "output_mode": "files_with_matches"}),
                &test_context,
            )
            .await;

        match &result.output {
            crate::types::ToolOutput::Success(content) => {
                assert!(content.contains("a.txt"));
                assert!(!content.contains("b.txt"));
            }
            crate::types::ToolOutput::Error(e) => {
                let error_message = e.to_string();
                if error_message.contains("is rg installed") {
                    return;
                }
                panic!("Unexpected error: {}", error_message);
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn test_grep_count_mode() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        fs::write(root.join("test.txt"), "line1\nline2\nline3")
            .await
            .unwrap();

        let test_context = super::super::context::ExecutionContext::from_path(&root).unwrap();
        let tool = GrepTool;

        let result = tool
            .execute(
                serde_json::json!({"pattern": "line", "output_mode": "count"}),
                &test_context,
            )
            .await;

        match &result.output {
            crate::types::ToolOutput::Success(content) => {
                assert!(content.contains("3") || content.contains(":3"));
            }
            crate::types::ToolOutput::Error(e) => {
                let error_message = e.to_string();
                if error_message.contains("is rg installed") {
                    return;
                }
                panic!("Unexpected error: {}", error_message);
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn test_grep_invalid_output_mode() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        fs::write(root.join("test.txt"), "content").await.unwrap();

        let test_context = super::super::context::ExecutionContext::from_path(&root).unwrap();
        let tool = GrepTool;

        let result = tool
            .execute(
                serde_json::json!({"pattern": "test", "output_mode": "invalid_mode"}),
                &test_context,
            )
            .await;

        match &result.output {
            crate::types::ToolOutput::Error(e) => {
                assert!(e.to_string().contains("Unknown output_mode"));
            }
            _ => panic!("Expected error for invalid output_mode"),
        }
    }

    #[tokio::test]
    async fn test_grep_with_glob_filter() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        fs::write(root.join("code.rs"), "fn test() {}")
            .await
            .unwrap();
        fs::write(root.join("doc.md"), "fn test() {}")
            .await
            .unwrap();

        let test_context = super::super::context::ExecutionContext::from_path(&root).unwrap();
        let tool = GrepTool;

        let result = tool
            .execute(
                serde_json::json!({"pattern": "fn test", "glob": "*.rs", "output_mode": "files_with_matches"}),
                &test_context,
            )
            .await;

        match &result.output {
            crate::types::ToolOutput::Success(content) => {
                assert!(content.contains("code.rs"));
                assert!(!content.contains("doc.md"));
            }
            crate::types::ToolOutput::Error(e) => {
                let error_message = e.to_string();
                if error_message.contains("is rg installed") {
                    return;
                }
                panic!("Unexpected error: {}", error_message);
            }
            _ => {}
        }
    }
}
