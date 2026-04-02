//! Read tool - reads file contents with TOCTOU protection and multimedia support.

use std::fmt::Write;
use std::path::Path;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use super::SchemaTool;
use super::context::ExecutionContext;
use crate::types::ToolResult;

const LARGE_FILE_THRESHOLD: u64 = 10 * 1024 * 1024; // 10MB

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ReadInput {
    /// The absolute path to the file to read
    pub file_path: String,
    /// The line number to start reading from. Only provide if the file is too large to read at once
    #[serde(default)]
    pub offset: Option<usize>,
    /// The number of lines to read. Only provide if the file is too large to read at once.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ReadTool;

enum FileType {
    Text,
    #[cfg(feature = "multimedia")]
    Pdf,
    #[cfg(feature = "multimedia")]
    Image,
    Jupyter,
}

fn detect_file_type(path: &Path) -> FileType {
    match path.extension().and_then(|e| e.to_str()) {
        #[cfg(feature = "multimedia")]
        Some("pdf") => FileType::Pdf,
        #[cfg(feature = "multimedia")]
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tiff") => FileType::Image,
        Some("ipynb") => FileType::Jupyter,
        _ => FileType::Text,
    }
}

async fn read_text(path: &Path, offset: usize, limit: usize) -> ToolResult {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to read file: {}", e)),
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let selected_lines: Vec<&str> = lines.into_iter().skip(offset).take(limit).collect();

    if selected_lines.is_empty() {
        return ToolResult::success(format!(
            "File is empty or offset {} exceeds file length {}",
            offset, total_lines
        ));
    }

    let estimated_capacity: usize = selected_lines
        .iter()
        .map(|line| 8 + line.len().min(2003))
        .sum();
    let mut output = String::with_capacity(estimated_capacity);

    for (i, line) in selected_lines.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }
        let line_num = offset + i + 1;
        if line.len() > 2000 {
            let _ = write!(output, "{:>6}\t{}...", line_num, &line[..2000]);
        } else {
            let _ = write!(output, "{:>6}\t{}", line_num, line);
        }
    }

    ToolResult::success(output)
}

#[cfg(feature = "multimedia")]
async fn read_pdf(path: &Path) -> ToolResult {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => return ToolResult::error(format!("Failed to read PDF: {}", e)),
    };

    match pdf_extract::extract_text_from_mem(&bytes) {
        Ok(text) => ToolResult::success(text),
        Err(e) => ToolResult::error(format!("Failed to extract PDF text: {}", e)),
    }
}

#[cfg(feature = "multimedia")]
async fn read_image(path: &Path) -> ToolResult {
    use base64::Engine;

    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => return ToolResult::error(format!("Failed to read image: {}", e)),
    };

    let mime = mime_guess::from_path(path)
        .first()
        .map(|m| m.to_string())
        .unwrap_or_else(|| "application/octet-stream".to_string());

    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    ToolResult::success(format!("data:{};base64,{}", mime, encoded))
}

async fn read_jupyter(path: &Path) -> ToolResult {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) => return ToolResult::error(format!("Failed to read notebook: {}", e)),
    };

    let notebook: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => return ToolResult::error(format!("Invalid notebook JSON: {}", e)),
    };

    let cells = match notebook.get("cells").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return ToolResult::error("Invalid notebook: no cells array"),
    };

    let mut output = String::new();
    for (i, cell) in cells.iter().enumerate() {
        let cell_type = cell
            .get("cell_type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");
        let source = cell.get("source").map(extract_source).unwrap_or_default();

        let _ = writeln!(output, "--- Cell {} [{}] ---", i + 1, cell_type);
        let _ = writeln!(output, "{}", source);

        if cell_type == "code"
            && let Some(outputs) = cell.get("outputs").and_then(|o| o.as_array())
        {
            for out in outputs {
                if let Some(text) = out.get("text") {
                    let _ = writeln!(output, "[Output]\n{}", extract_source(text));
                } else if let Some(data) = out.get("data")
                    && let Some(text) = data.get("text/plain")
                {
                    let _ = writeln!(output, "[Output]\n{}", extract_source(text));
                }
            }
        }
        output.push('\n');
    }

    ToolResult::success(output)
}

fn extract_source(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

async fn warn_if_large_file(path: &Path) {
    if let Ok(meta) = tokio::fs::metadata(path).await
        && meta.len() > LARGE_FILE_THRESHOLD
    {
        tracing::warn!(
            path = %path.display(),
            size_mb = meta.len() / (1024 * 1024),
            "Reading large file into memory"
        );
    }
}

#[async_trait]
impl SchemaTool for ReadTool {
    type Input = ReadInput;

    const NAME: &'static str = "Read";
    const READ_ONLY: bool = true;

    const DESCRIPTION: &'static str = r#"Reads a file from the local filesystem. You can access any file directly by using this tool.
Assume this tool is able to read all files on the machine. If a path to a file is provided assume that path is valid. It is okay to read a file that does not exist; an error will be returned.

Usage:
- The file_path parameter must be an absolute path, not a relative path
- By default, it reads up to 2000 lines starting from the beginning of the file
- You can optionally specify a line offset and limit (especially handy for long files), but it's recommended to read the whole file by not providing these parameters
- Any lines longer than 2000 characters will be truncated
- Results are returned using cat -n format, with line numbers starting at 1
- This tool can read images (eg PNG, JPG, etc). When reading an image file the contents are returned as base64-encoded data URI for multimodal processing.
- This tool can read PDF files (.pdf). PDFs are processed page by page, extracting both text and visual content for analysis.
- This tool can read Jupyter notebooks (.ipynb files) and returns all cells with their outputs, combining code, text, and visualizations.
- This tool can only read files, not directories. To read a directory, use an ls command via the Bash tool.
- You can call multiple tools in a single response. It is always better to speculatively read multiple potentially useful files in parallel.
- If you read a file that exists but has empty contents you will receive a system reminder warning in place of file contents."#;

    async fn handle(&self, input: ReadInput, context: &ExecutionContext) -> ToolResult {
        let path = match context.try_resolve_for(Self::NAME, &input.file_path) {
            Ok(p) => p,
            Err(e) => return e,
        };

        let file_type = detect_file_type(path.as_path());

        if !matches!(file_type, FileType::Text) {
            warn_if_large_file(path.as_path()).await;
        }

        match file_type {
            FileType::Text => {
                let offset = input.offset.unwrap_or(0);
                let limit = input.limit.unwrap_or(2000);
                read_text(path.as_path(), offset, limit).await
            }
            #[cfg(feature = "multimedia")]
            FileType::Pdf => read_pdf(path.as_path()).await,
            #[cfg(feature = "multimedia")]
            FileType::Image => read_image(path.as_path()).await,
            FileType::Jupyter => read_jupyter(path.as_path()).await,
        }
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
    async fn test_read_file() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "line 1\nline 2\nline 3")
            .await
            .unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = ReadTool;

        let result = tool
            .execute(
                serde_json::json!({"file_path": file_path.to_str().unwrap()}),
                &test_context,
            )
            .await;

        match &result.output {
            ToolOutput::Success(content) => {
                assert!(content.contains("line 1"));
                assert!(content.contains("line 2"));
                assert!(content.contains("line 3"));
            }
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_read_jupyter_notebook() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("test.ipynb");

        let notebook = serde_json::json!({
            "cells": [
                {
                    "cell_type": "markdown",
                    "source": ["# Title"]
                },
                {
                    "cell_type": "code",
                    "source": ["print('hello')"],
                    "outputs": [{"text": ["hello\n"]}]
                }
            ]
        });

        fs::write(&file_path, serde_json::to_string(&notebook).unwrap())
            .await
            .unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = ReadTool;

        let result = tool
            .execute(
                serde_json::json!({"file_path": file_path.to_str().unwrap()}),
                &test_context,
            )
            .await;

        match &result.output {
            ToolOutput::Success(content) => {
                assert!(content.contains("# Title"));
                assert!(content.contains("print('hello')"));
                assert!(content.contains("[Output]"));
            }
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_read_path_traversal_blocked() {
        let dir = tempdir().unwrap();
        let test_context = ExecutionContext::from_path(dir.path()).unwrap();
        let tool = ReadTool;

        let result = tool
            .execute(
                serde_json::json!({"file_path": "../../../etc/passwd"}),
                &test_context,
            )
            .await;

        assert!(result.is_error());
    }

    #[tokio::test]
    async fn test_read_with_offset_and_limit() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "line 1\nline 2\nline 3\nline 4\nline 5")
            .await
            .unwrap();

        let test_context = ExecutionContext::from_path(&root).unwrap();
        let tool = ReadTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": file_path.to_str().unwrap(),
                    "offset": 1,
                    "limit": 2
                }),
                &test_context,
            )
            .await;

        match &result.output {
            ToolOutput::Success(content) => {
                assert!(!content.contains("line 1"));
                assert!(content.contains("line 2"));
                assert!(content.contains("line 3"));
                assert!(!content.contains("line 4"));
            }
            _ => panic!("Expected success"),
        }
    }
}
