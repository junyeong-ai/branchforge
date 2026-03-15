//! Helper types for message requests.

use serde::{Deserialize, Serialize};

use crate::types::{ToolDefinition, ToolSearchTool, WebFetchTool, WebSearchTool};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Tenant ID — preserved for SDK-level audit/attribution, not sent to API.
    #[serde(skip)]
    pub tenant_id: Option<String>,
    /// Session ID — preserved for SDK-level correlation, not sent to API.
    #[serde(skip)]
    pub session_id: Option<String>,
}

impl RequestMetadata {
    pub fn from_identity(
        tenant_id: Option<&str>,
        principal_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Option<Self> {
        let principal_id = principal_id?;
        Some(Self {
            user_id: Some(principal_id.to_string()),
            tenant_id: tenant_id.map(String::from),
            session_id: session_id.map(String::from),
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
        assert_eq!(metadata.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(metadata.session_id.as_deref(), Some("session-1"));

        // Without tenant/session, fields should be None
        let metadata = RequestMetadata::from_identity(None, Some("user-1"), None).unwrap();
        assert_eq!(metadata.user_id.as_deref(), Some("user-1"));
        assert!(metadata.tenant_id.is_none());
        assert!(metadata.session_id.is_none());

        // Without principal, returns None
        assert!(RequestMetadata::from_identity(Some("t"), None, None).is_none());

        // Verify serialization only includes user_id (not tenant/session)
        let metadata = RequestMetadata::from_identity(Some("t"), Some("u"), Some("s")).unwrap();
        let json = serde_json::to_value(&metadata).unwrap();
        assert!(json.get("user_id").is_some());
        assert!(
            json.get("tenant_id").is_none(),
            "tenant_id must not be serialized to API"
        );
        assert!(
            json.get("session_id").is_none(),
            "session_id must not be serialized to API"
        );
    }
}
