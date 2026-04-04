//! MCP (Model Context Protocol) server integration.

pub mod client;
pub mod manager;
pub mod resources;
pub mod toolset;

pub use client::McpClient;
pub use manager::McpManager;
pub use resources::{ResourceManager, ResourceQuery};
pub use toolset::{McpToolset, McpToolsetRegistry, ToolLoadConfig};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

const MCP_TOOL_PREFIX: &str = "mcp__";

#[cfg(feature = "mcp")]
pub(crate) const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26"];

/// Configurable timeouts for MCP operations.
#[derive(Clone, Debug)]
pub struct McpTimeouts {
    /// Timeout for establishing a connection (default: 30s).
    pub connection: Duration,
    /// Timeout for a tool call (default: 60s).
    pub tool_call: Duration,
    /// Timeout for reading a resource (default: 30s).
    pub resource_read: Duration,
}

impl Default for McpTimeouts {
    fn default() -> Self {
        Self {
            connection: Duration::from_secs(30),
            tool_call: Duration::from_secs(60),
            resource_read: Duration::from_secs(30),
        }
    }
}

/// TTL-based cache entry for tool listings from a single MCP server.
#[cfg(feature = "mcp")]
pub(crate) struct ToolCache {
    pub tools: Vec<McpToolDefinition>,
    pub cached_at: std::time::Instant,
    pub ttl: Duration,
}

#[cfg(feature = "mcp")]
impl ToolCache {
    pub fn is_valid(&self) -> bool {
        self.cached_at.elapsed() < self.ttl
    }
}

/// MCP server configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpServerConfig {
    /// stdio transport - communicates with server via stdin/stdout
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// Server-Sent Events transport (requires rmcp SSE support)
    Sse {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

/// Reconnection policy with exponential backoff and jitter
#[derive(Clone, Debug)]
pub struct ReconnectPolicy {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter_factor: f64,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30000,
            jitter_factor: 0.3,
        }
    }
}

impl ReconnectPolicy {
    pub fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration {
        let base = self.base_delay_ms * 2u64.pow(attempt.min(10));
        let jitter = (base as f64 * self.jitter_factor * rand::random::<f64>()) as u64;
        std::time::Duration::from_millis((base + jitter).min(self.max_delay_ms))
    }
}

/// Parse MCP qualified name (mcp__server__tool) into (server, tool)
pub(crate) fn parse_mcp_name(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix(MCP_TOOL_PREFIX)?.split_once("__")
}

/// Create MCP qualified name from server and tool names
pub(crate) fn make_mcp_name(server: &str, tool: &str) -> String {
    format!("{}{server}__{tool}", MCP_TOOL_PREFIX)
}

/// Check if a name matches MCP naming pattern
pub(crate) fn is_mcp_name(name: &str) -> bool {
    name.starts_with(MCP_TOOL_PREFIX)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum McpConnectionStatus {
    #[default]
    Connecting,
    Connected,
    Disconnected,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerInfo {
    pub name: String,
    pub version: String,
    /// Protocol version (e.g., "2025-06-18")
    #[serde(default)]
    pub protocol_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceDefinition {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Clone, Debug)]
pub struct McpServerState {
    pub name: String,
    pub config: McpServerConfig,
    pub status: McpConnectionStatus,
    pub server_info: Option<McpServerInfo>,
    pub tools: Vec<McpToolDefinition>,
    pub resources: Vec<McpResourceDefinition>,
}

impl McpServerState {
    pub fn new(name: impl Into<String>, config: McpServerConfig) -> Self {
        Self {
            name: name.into(),
            config,
            status: McpConnectionStatus::Connecting,
            server_info: None,
            tools: Vec::new(),
            resources: Vec::new(),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.status == McpConnectionStatus::Connected
    }
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("Connection failed: {message}")]
    ConnectionFailed { message: String },

    #[error("Protocol error: {message}")]
    Protocol { message: String },

    #[error("JSON-RPC error {code}: {message}")]
    JsonRpc { code: i32, message: String },

    #[error("Tool error: {message}")]
    ToolError { message: String },

    #[error("Server not found: {name}")]
    ServerNotFound { name: String },

    #[error("Tool not found: {name}")]
    ToolNotFound { name: String },

    #[error("Resource not found: {uri}")]
    ResourceNotFound { uri: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type McpResult<T> = std::result::Result<T, McpError>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpToolResult {
    pub content: Vec<McpContent>,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContent {
    Text {
        text: String,
    },
    Image {
        data: String,
        mime_type: String,
    },
    Resource {
        uri: String,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        blob: Option<String>,
        #[serde(default)]
        mime_type: Option<String>,
    },
}

impl McpContent {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            McpContent::Text { text } => Some(text),
            _ => None,
        }
    }
}

impl McpToolResult {
    pub fn to_string_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.as_text())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mcp_name() {
        assert_eq!(
            parse_mcp_name("mcp__server__tool"),
            Some(("server", "tool"))
        );
        assert_eq!(
            parse_mcp_name("mcp__fs__read_file"),
            Some(("fs", "read_file"))
        );
        assert_eq!(
            parse_mcp_name("mcp__my_server__tool"),
            Some(("my_server", "tool"))
        );
        assert_eq!(parse_mcp_name("Read"), None);
        assert_eq!(parse_mcp_name("mcp_invalid"), None);
    }

    #[test]
    fn test_make_mcp_name() {
        assert_eq!(make_mcp_name("server", "tool"), "mcp__server__tool");
        assert_eq!(make_mcp_name("fs", "read_file"), "mcp__fs__read_file");
    }

    #[test]
    fn test_is_mcp_name() {
        assert!(is_mcp_name("mcp__server__tool"));
        assert!(!is_mcp_name("Read"));
        assert!(!is_mcp_name("mcp_invalid"));
    }

    #[test]
    fn test_reconnect_policy_delay() {
        let policy = ReconnectPolicy::default();
        let d0 = policy.delay_for_attempt(0);
        let d1 = policy.delay_for_attempt(1);
        assert!(d1 > d0);
        assert!(d0.as_millis() >= 1000);
        assert!(d0.as_millis() <= 1300);
    }

    #[test]
    fn test_mcp_server_config_serde() {
        let config = McpServerConfig::Stdio {
            command: "npx".to_string(),
            args: vec!["server".to_string()],
            env: HashMap::new(),
            cwd: None,
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("stdio"));
        assert!(json.contains("npx"));
    }

    #[test]
    fn test_mcp_server_config_sse_serde() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer token123".to_string());
        let config = McpServerConfig::Sse {
            url: "http://localhost:8080/mcp".to_string(),
            headers,
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("sse"));
        assert!(json.contains("http://localhost:8080/mcp"));
        assert!(json.contains("Bearer token123"));

        // Round-trip
        let deserialized: McpServerConfig = serde_json::from_str(&json).unwrap();
        match deserialized {
            McpServerConfig::Sse { url, headers } => {
                assert_eq!(url, "http://localhost:8080/mcp");
                assert_eq!(headers.get("Authorization").unwrap(), "Bearer token123");
            }
            _ => panic!("Expected Sse variant"),
        }
    }

    #[test]
    fn test_mcp_server_config_sse_empty_headers() {
        let json = r#"{"type":"sse","url":"http://example.com/mcp"}"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        match config {
            McpServerConfig::Sse { url, headers } => {
                assert_eq!(url, "http://example.com/mcp");
                assert!(headers.is_empty());
            }
            _ => panic!("Expected Sse variant"),
        }
    }

    #[test]
    fn test_mcp_server_state_new() {
        let state = McpServerState::new(
            "test",
            McpServerConfig::Stdio {
                command: "test".to_string(),
                args: vec![],
                env: HashMap::new(),
                cwd: None,
            },
        );

        assert_eq!(state.name, "test");
        assert_eq!(state.status, McpConnectionStatus::Connecting);
        assert!(!state.is_connected());
    }

    #[test]
    fn test_mcp_timeouts_default() {
        let timeouts = McpTimeouts::default();
        assert_eq!(timeouts.connection, Duration::from_secs(30));
        assert_eq!(timeouts.tool_call, Duration::from_secs(60));
        assert_eq!(timeouts.resource_read, Duration::from_secs(30));
    }

    #[test]
    fn test_mcp_timeouts_custom() {
        let timeouts = McpTimeouts {
            connection: Duration::from_secs(10),
            tool_call: Duration::from_secs(120),
            resource_read: Duration::from_secs(5),
        };
        assert_eq!(timeouts.connection, Duration::from_secs(10));
        assert_eq!(timeouts.tool_call, Duration::from_secs(120));
        assert_eq!(timeouts.resource_read, Duration::from_secs(5));
    }

    #[test]
    fn test_mcp_content_as_text() {
        let content = McpContent::Text {
            text: "hello".to_string(),
        };
        assert_eq!(content.as_text(), Some("hello"));

        let image = McpContent::Image {
            data: "base64".to_string(),
            mime_type: "image/png".to_string(),
        };
        assert_eq!(image.as_text(), None);
    }
}
