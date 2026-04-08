//! Authorization records for the tool execution layer.

use serde::{Deserialize, Serialize};

/// Record of an authorization denial for a tool request.
///
/// Tracks when and why a tool execution was blocked, useful for
/// debugging and audit logging. Lives in the agent domain (not the
/// LLM IR) because it carries authorization-policy concerns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizationDenied {
    /// Name of the tool that was denied.
    pub tool_name: String,
    /// The provider tool-call id from the API request.
    pub tool_call_id: String,
    /// The input that was provided to the tool.
    pub tool_input: serde_json::Value,
    /// Reason for the denial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Timestamp when the denial occurred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

impl AuthorizationDenied {
    /// Create a new authorization denial record.
    pub fn new(
        tool_name: impl Into<String>,
        tool_call_id: impl Into<String>,
        tool_input: serde_json::Value,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            tool_call_id: tool_call_id.into(),
            tool_input,
            reason: None,
            timestamp: Some(chrono::Utc::now()),
        }
    }

    /// Add a reason for the denial.
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authorization_denial() {
        let denial =
            AuthorizationDenied::new("WebSearch", "tool_123", serde_json::json!({"query": "x"}))
                .reason("User denied");
        assert_eq!(denial.tool_name, "WebSearch");
        assert_eq!(denial.tool_call_id, "tool_123");
        assert_eq!(denial.reason.as_deref(), Some("User denied"));
        assert!(denial.timestamp.is_some());
    }
}
