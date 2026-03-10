//! Helper types for message requests.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::{ToolDefinition, ToolSearchTool, WebFetchTool, WebSearchTool};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl RequestMetadata {
    pub fn from_identity(
        tenant_id: Option<&str>,
        principal_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Option<Self> {
        let principal_id = principal_id?;
        let mut extra = HashMap::new();
        if let Some(tenant_id) = tenant_id {
            extra.insert(
                "tenant_id".to_string(),
                serde_json::Value::String(tenant_id.to_string()),
            );
        }
        if let Some(session_id) = session_id {
            extra.insert(
                "session_id".to_string(),
                serde_json::Value::String(session_id.to_string()),
            );
        }
        Some(Self {
            user_id: Some(principal_id.to_string()),
            extra,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApiTool {
    Custom(ToolDefinition),
    WebSearch(WebSearchTool),
    WebFetch(WebFetchTool),
    ToolSearch(ToolSearchTool),
}

impl From<ToolDefinition> for ApiTool {
    fn from(tool: ToolDefinition) -> Self {
        Self::Custom(tool)
    }
}

impl From<WebSearchTool> for ApiTool {
    fn from(tool: WebSearchTool) -> Self {
        Self::WebSearch(tool)
    }
}

impl From<ToolSearchTool> for ApiTool {
    fn from(tool: ToolSearchTool) -> Self {
        Self::ToolSearch(tool)
    }
}

impl From<WebFetchTool> for ApiTool {
    fn from(tool: WebFetchTool) -> Self {
        Self::WebFetch(tool)
    }
}

impl ApiTool {
    pub fn is_strict(&self) -> bool {
        match self {
            Self::Custom(def) => def.strict == Some(true),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorResponse {
    #[serde(rename = "type")]
    pub error_type: String,
    pub error: ErrorDetail,
}

impl ErrorResponse {
    pub fn into_error(self, status: u16) -> crate::Error {
        crate::Error::Api {
            message: self.error.message,
            status: Some(status),
            error_type: Some(self.error.error_type),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorDetail {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_metadata_from_identity() {
        let metadata =
            RequestMetadata::from_identity(Some("tenant-a"), Some("user-1"), Some("session-1"))
                .unwrap();
        assert_eq!(metadata.user_id.as_deref(), Some("user-1"));
        assert_eq!(
            metadata.extra.get("tenant_id"),
            Some(&serde_json::json!("tenant-a"))
        );
        assert_eq!(
            metadata.extra.get("session_id"),
            Some(&serde_json::json!("session-1"))
        );
    }
}
