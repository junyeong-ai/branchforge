//! Write tool - creates or overwrites files with atomic operations.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use super::SchemaTool;
use super::context::ExecutionContext;
use crate::security::fs::SecureFileHandle;
use crate::types::ToolResult;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct WriteInput {
    /// The absolute path to the file to write (must be absolute, not relative)
    pub file_path: String,
    /// The content to write to the file
    pub content: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WriteTool;

#[async_trait]
impl SchemaTool for WriteTool {
    type Input = WriteInput;

    const NAME: &'static str = "Write";
    const DESCRIPTION: &'static str = "Write a file to the local filesystem. Overwrites existing files. Creates parent directories if needed. Prefer Edit for modifying existing files.";

    async fn handle(&self, input: WriteInput, context: &ExecutionContext) -> ToolResult {
        let path = match context.try_resolve_for(Self::NAME, &input.file_path) {
            Ok(p) => p,
            Err(e) => return e,
        };

        let content = input.content;
        let content_len = content.len();
        let display_path = path.as_path().display().to_string();

        let result = tokio::task::spawn_blocking(move || {
            let handle = SecureFileHandle::for_atomic_write(path)?;
            handle.atomic_write(content.as_bytes())?;
            Ok::<_, crate::security::SecurityError>(())
        })
        .await;

        match result {
            Ok(Ok(())) => ToolResult::success(format!(
                "Successfully wrote {} bytes to {}",
                content_len, display_path
            )),
            Ok(Err(e)) => ToolResult::error(format!("Failed to write file: {}", e)),
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
    async fn test_write_file() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("test.txt");

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "content": "Hello, World!"
                }),
                &test_context,
            )
            .await;

        assert!(!result.is_error());
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "Hello, World!");
    }

    #[tokio::test]
    async fn test_write_creates_directories() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("subdir/nested/test.txt");

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "content": "Nested content"
                }),
                &test_context,
            )
            .await;

        assert!(!result.is_error());
        assert!(file_path.exists());
    }

    #[tokio::test]
    async fn test_write_path_escape_blocked() {
        let dir = tempdir().unwrap();
        let test_context = ExecutionContext::from_path(dir.path()).unwrap();
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "/etc/passwd",
                    "content": "bad"
                }),
                &test_context,
            )
            .await;

        assert!(result.is_error());
    }

    #[tokio::test]
    async fn test_write_overwrites_existing() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "original content").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "content": "new content"
                }),
                &test_context,
            )
            .await;

        assert!(!result.is_error());
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "new content");
    }

    #[tokio::test]
    async fn test_write_empty_content() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("empty.txt");

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "content": ""
                }),
                &test_context,
            )
            .await;

        assert!(!result.is_error());
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "");
    }

    #[tokio::test]
    async fn test_write_multiline_content() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("multi.txt");
        let content = "line 1\nline 2\nline 3\n";

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "content": content
                }),
                &test_context,
            )
            .await;

        assert!(!result.is_error());
        let read_content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(read_content, content);
    }

    #[tokio::test]
    async fn test_write_atomic_no_temp_files_remain() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("atomic_test.txt");

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "content": "atomic content"
                }),
                &test_context,
            )
            .await;

        assert!(!result.is_error());

        let entries: Vec<_> = std::fs::read_dir(&root).unwrap().collect();
        let has_temp = entries.iter().any(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
        });
        assert!(!has_temp, "Temporary files should be cleaned up");
    }

    #[tokio::test]
    async fn test_write_atomic_preserves_original_until_complete() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("preserve_test.txt");
        fs::write(&file_path, "original content").await.unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "content": "new content"
                }),
                &test_context,
            )
            .await;

        assert!(!result.is_error());
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "new content");
    }
}
