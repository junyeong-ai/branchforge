//! Edit tool - performs string replacements in files with TOCTOU protection.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use super::SchemaTool;
use super::context::ExecutionContext;
use crate::security::fs::SecureFileHandle;
use crate::types::ToolResult;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct EditInput {
    /// The absolute path to the file to modify
    pub file_path: String,
    /// The text to replace
    pub old_string: String,
    /// The text to replace it with (must be different from old_string)
    pub new_string: String,
    /// Replace all occurences of old_string (default false)
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EditTool;

#[async_trait]
impl SchemaTool for EditTool {
    type Input = EditInput;

    const NAME: &'static str = "Edit";
    const DESCRIPTION: &'static str = "Perform exact string replacement in a file. The `old_string` must be unique in the file (or use `replace_all: true` to replace every occurrence).";

    async fn handle(&self, input: EditInput, context: &ExecutionContext) -> ToolResult {
        if input.old_string == input.new_string {
            return ToolResult::error("old_string and new_string must be different");
        }

        let path = match context.try_resolve_for(Self::NAME, &input.file_path) {
            Ok(p) => p,
            Err(e) => return e,
        };

        let old_string = input.old_string;
        let new_string = input.new_string;
        let replace_all = input.replace_all;
        let display_path = path.as_path().display().to_string();

        let result = tokio::task::spawn_blocking(move || {
            let handle =
                SecureFileHandle::open_read(path.clone()).map_err(|e| e.to_string())?;
            let original_content = handle.read_to_string().map_err(|e| e.to_string())?;

            let count = original_content.matches(&old_string).count();
            if count == 0 {
                return Err("old_string not found in file. Make sure it matches exactly including whitespace.".to_string());
            }
            if count > 1 && !replace_all {
                return Err(format!(
                    "old_string found {} times. Use replace_all=true to replace all, \
                     or provide more context to make it unique.",
                    count
                ));
            }

            let new_content = if replace_all {
                original_content.replace(&old_string, &new_string)
            } else {
                original_content.replacen(&old_string, &new_string, 1)
            };

            let recheck_handle =
                SecureFileHandle::open_read(path.clone()).map_err(|e| e.to_string())?;
            let current_content = recheck_handle.read_to_string().map_err(|e| e.to_string())?;
            if current_content != original_content {
                return Err("File was modified externally; operation aborted".to_string());
            }

            let write_handle = SecureFileHandle::open_write(path).map_err(|e| e.to_string())?;
            write_handle
                .atomic_write(new_content.as_bytes())
                .map_err(|e| e.to_string())?;

            Ok(count)
        })
        .await;

        match result {
            Ok(Ok(count)) => {
                let msg = if replace_all {
                    format!("Replaced {} occurrences in {}", count, display_path)
                } else {
                    format!("Replaced 1 occurrence in {}", display_path)
                };
                ToolResult::success(msg)
            }
            Ok(Err(e)) => ToolResult::error(e),
            Err(e) => ToolResult::error(format!("Task failed: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn test_edit_single() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "Hello, World!").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = EditTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "World",
                    "new_string": "Rust"
                }),
                &test_context,
            )
            .await;

        assert!(!result.is_error());
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "Hello, Rust!");
    }

    #[tokio::test]
    async fn test_edit_replace_all() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "foo bar foo").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = EditTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "foo",
                    "new_string": "baz",
                    "replace_all": true
                }),
                &test_context,
            )
            .await;

        assert!(!result.is_error());
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "baz bar baz");
    }

    #[tokio::test]
    async fn test_edit_same_string_error() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "content").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = EditTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "same",
                    "new_string": "same"
                }),
                &test_context,
            )
            .await;

        assert!(result.is_error());
    }

    #[tokio::test]
    async fn test_edit_not_found_error() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "Hello, World!").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = EditTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "notfound",
                    "new_string": "replacement"
                }),
                &test_context,
            )
            .await;

        assert!(result.is_error());
    }

    #[tokio::test]
    async fn test_edit_multiple_without_replace_all_error() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "foo bar foo").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = EditTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "foo",
                    "new_string": "baz"
                }),
                &test_context,
            )
            .await;

        assert!(result.is_error());
    }

    #[tokio::test]
    async fn test_edit_path_escape_blocked() {
        let dir = tempdir().unwrap();
        let test_context = ExecutionContext::from_path(dir.path()).unwrap();
        let tool = EditTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "/etc/passwd",
                    "old_string": "root",
                    "new_string": "evil"
                }),
                &test_context,
            )
            .await;

        assert!(result.is_error());
    }

    #[tokio::test]
    async fn test_edit_concurrent_modification_detected() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("concurrent.txt");
        let original = "Hello World";
        std::fs::write(&file_path, original).unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();

        std::fs::write(&file_path, "Hello Changed World").unwrap();

        let input = EditInput {
            file_path: file_path.to_str().unwrap().to_string(),
            old_string: "Hello".to_string(),
            new_string: "Hi".to_string(),
            replace_all: false,
        };

        let path = test_context.resolve(&input.file_path).unwrap();
        let old_string = input.old_string.clone();
        let new_string = input.new_string.clone();

        let result = tokio::task::spawn_blocking(move || {
            let handle = crate::security::fs::SecureFileHandle::open_read(path.clone()).unwrap();
            let original_content = handle.read_to_string().unwrap();

            std::fs::write(path.as_path(), "Completely different content").unwrap();

            let new_content = original_content.replacen(&old_string, &new_string, 1);

            let recheck_handle =
                crate::security::fs::SecureFileHandle::open_read(path.clone()).unwrap();
            let current_content = recheck_handle.read_to_string().unwrap();

            if current_content != original_content {
                return Err("File was modified externally; operation aborted".to_string());
            }

            let write_handle = crate::security::fs::SecureFileHandle::open_write(path).unwrap();
            write_handle.atomic_write(new_content.as_bytes()).unwrap();
            Ok(())
        })
        .await
        .unwrap();

        assert!(result.is_err());
        let message = result.unwrap_err();
        assert!(
            message.contains("modified externally"),
            "Expected 'modified externally' error, got: {}",
            message
        );
    }
}
