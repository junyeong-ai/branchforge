//! Tool execution input and output types.

use serde::{Deserialize, Serialize};

use super::error::ToolError;
use crate::types::response::Usage;

#[derive(Debug, Clone)]
pub struct ToolInput {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum ToolOutput {
    Success(String),
    SuccessBlocks(Vec<ToolOutputBlock>),
    Error(ToolError),
    Empty,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOutputBlock {
    Text {
        text: String,
    },
    Image {
        data: String,
        media_type: String,
    },
    #[serde(rename = "search_result")]
    SearchResult(crate::types::search::SearchResultBlock),
}

impl ToolOutput {
    pub fn success(content: impl Into<String>) -> Self {
        Self::Success(content.into())
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error(ToolError::execution_failed(message))
    }

    pub fn tool_error(error: ToolError) -> Self {
        Self::Error(error)
    }

    pub fn authorization_denied(tool: impl Into<String>, permission: impl Into<String>) -> Self {
        Self::Error(ToolError::authorization_denied(tool, permission))
    }

    pub fn not_found(path: impl Into<String>) -> Self {
        Self::Error(ToolError::not_found(path))
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::Error(ToolError::invalid_input(message))
    }

    pub fn timeout(timeout_ms: u64) -> Self {
        Self::Error(ToolError::timeout(timeout_ms))
    }

    pub fn security_error(message: impl Into<String>) -> Self {
        Self::Error(ToolError::security_violation(message))
    }

    pub fn empty() -> Self {
        Self::Empty
    }

    pub fn search_results(results: Vec<crate::types::search::SearchResultBlock>) -> Self {
        Self::SuccessBlocks(
            results
                .into_iter()
                .map(ToolOutputBlock::SearchResult)
                .collect(),
        )
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    pub fn as_error(&self) -> Option<&ToolError> {
        match self {
            Self::Error(e) => Some(e),
            _ => None,
        }
    }

    pub fn error_message(&self) -> String {
        match self {
            Self::Error(e) => e.to_string(),
            _ => String::new(),
        }
    }

    pub fn text(&self) -> String {
        match self {
            Self::Success(content) => content.clone(),
            Self::SuccessBlocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ToolOutputBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Self::Error(e) => e.to_string(),
            Self::Empty => String::new(),
        }
    }
}

impl From<String> for ToolOutput {
    fn from(s: String) -> Self {
        Self::Success(s)
    }
}

impl From<&str> for ToolOutput {
    fn from(s: &str) -> Self {
        Self::Success(s.to_string())
    }
}

impl<T, E> From<Result<T, E>> for ToolOutput
where
    T: Into<String>,
    E: std::fmt::Display,
{
    fn from(result: Result<T, E>) -> Self {
        match result {
            Ok(content) => Self::Success(content.into()),
            Err(e) => Self::error(e.to_string()),
        }
    }
}

impl From<ToolError> for ToolOutput {
    fn from(error: ToolError) -> Self {
        Self::Error(error)
    }
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub output: ToolOutput,
    pub inner_usage: Option<Usage>,
    pub inner_model: Option<String>,
}

impl ToolResult {
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            output: ToolOutput::success(content),
            inner_usage: None,
            inner_model: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            output: ToolOutput::error(message),
            inner_usage: None,
            inner_model: None,
        }
    }

    pub fn empty() -> Self {
        Self {
            output: ToolOutput::Empty,
            inner_usage: None,
            inner_model: None,
        }
    }

    pub fn usage(mut self, usage: Usage) -> Self {
        self.inner_usage = Some(usage);
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.inner_model = Some(model.into());
        self
    }

    pub fn inner_call(mut self, usage: Usage, model: impl Into<String>) -> Self {
        self.inner_usage = Some(usage);
        self.inner_model = Some(model.into());
        self
    }

    pub fn is_error(&self) -> bool {
        self.output.is_error()
    }

    pub fn is_non_retryable(&self) -> bool {
        matches!(
            self.output,
            ToolOutput::Error(
                ToolError::AuthorizationDenied { .. }
                    | ToolError::SecurityViolation { .. }
                    | ToolError::UnknownTool { .. }
            )
        )
    }

    pub fn text(&self) -> String {
        self.output.text()
    }

    pub fn error_message(&self) -> String {
        self.output.error_message()
    }

    pub fn as_error(&self) -> Option<&ToolError> {
        self.output.as_error()
    }

    pub fn authorization_denied(tool: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            output: ToolOutput::authorization_denied(tool, reason),
            inner_usage: None,
            inner_model: None,
        }
    }

    pub fn unknown_tool(name: impl Into<String>) -> Self {
        Self {
            output: ToolOutput::tool_error(ToolError::unknown_tool(name)),
            inner_usage: None,
            inner_model: None,
        }
    }

    pub fn timeout(timeout_ms: u64) -> Self {
        Self {
            output: ToolOutput::timeout(timeout_ms),
            inner_usage: None,
            inner_model: None,
        }
    }

    pub fn security_error(message: impl Into<String>) -> Self {
        Self {
            output: ToolOutput::security_error(message),
            inner_usage: None,
            inner_model: None,
        }
    }
}

impl From<ToolOutput> for ToolResult {
    fn from(output: ToolOutput) -> Self {
        Self {
            output,
            inner_usage: None,
            inner_model: None,
        }
    }
}

impl From<String> for ToolResult {
    fn from(s: String) -> Self {
        Self::success(s)
    }
}

impl From<&str> for ToolResult {
    fn from(s: &str) -> Self {
        Self::success(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_output_from_result() {
        let ok: Result<&str, &str> = Ok("success");
        let output: ToolOutput = ok.into();
        assert!(!output.is_error());

        let err: Result<&str, &str> = Err("failed");
        let output: ToolOutput = err.into();
        assert!(output.is_error());
    }
}
