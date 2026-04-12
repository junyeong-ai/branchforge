//! MCP (Model Context Protocol) server integration.

pub mod client;
#[cfg(feature = "mcp")]
pub mod elicitation;
pub mod manager;
pub mod resources;
pub mod toolset;

pub use client::McpClient;
#[cfg(feature = "mcp")]
pub use elicitation::HumanElicitationRouter;
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
#[non_exhaustive]
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

/// Phase of the MCP server lifecycle that a connection attempt was
/// in when it succeeded or failed.
///
/// The phases are ordered: a server that reaches `Ready` has
/// completed every earlier phase. When a failure is recorded, the
/// attached phase pinpoints exactly where the handshake broke down —
/// `Spawn` failures mean the process or transport never came up,
/// while `ListTools` failures mean the server handshake succeeded
/// but its tool catalogue was unreadable. Agent runtimes use this to
/// build a [`DegradedReport`] instead of treating every per-server
/// failure as a fatal agent-build error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpClientState {
    /// The server is registered but no connection attempt has started.
    #[default]
    Queued,
    /// Spawning the child process (stdio) or constructing the HTTP
    /// transport (SSE).
    Spawn,
    /// Establishing the MCP transport — framed stdio read/write or
    /// Streamable HTTP session.
    Handshake,
    /// JSON-RPC `initialize` request → response round trip.
    InitializeProtocol,
    /// Reading `peer_info` and validating the protocol version
    /// against the supported-protocol list.
    NegotiateCapabilities,
    /// `tools/list` round trip. Failure here means the server is
    /// handshake-healthy but its tools are unusable.
    ListTools,
    /// `resources/list` round trip. Best-effort — failure downgrades
    /// the server but does not disqualify it.
    ListResources,
    /// `prompts/list` round trip. Best-effort — failure downgrades
    /// the server but does not disqualify it.
    ListPrompts,
    /// Populating the manager-level tool cache with the discovered
    /// tool catalogue.
    CacheWarmup,
    /// Server is fully connected, tool catalogue populated, and
    /// operational — dispatching requests. A `Ready` client can
    /// transition to either [`Self::Closed`] (graceful shutdown)
    /// or [`Self::Failed`] (runtime error).
    Ready,
    /// Terminal: graceful shutdown requested and completed. This is
    /// the success-path end state — distinct from [`Self::Failed`]
    /// so degraded reports can tell "user closed the server" from
    /// "the server crashed."
    Closed,
    /// Terminal failure. The attached `DegradedReport` entry carries
    /// the specific earlier phase where the attempt broke down.
    Failed,
}

impl McpClientState {
    /// Human-readable phase name for logs and reports. Stable so
    /// downstream OTel / metrics labels can group by phase.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Spawn => "spawn",
            Self::Handshake => "handshake",
            Self::InitializeProtocol => "initialize_protocol",
            Self::NegotiateCapabilities => "negotiate_capabilities",
            Self::ListTools => "list_tools",
            Self::ListResources => "list_resources",
            Self::ListPrompts => "list_prompts",
            Self::CacheWarmup => "cache_warmup",
            Self::Ready => "ready",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }

    /// `true` if the phase represents a fully-usable server state.
    /// Only [`Self::Ready`] satisfies this.
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// `true` if the client has reached a terminal phase and will
    /// not transition further. `Closed` (graceful shutdown) and
    /// `Failed` (error) are the two terminals.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }

    /// Linear ordering used by the forward-only transition rule.
    /// Lower number = earlier in the lifecycle. `Closed` and
    /// `Failed` are terminal sentinels outside the linear handshake
    /// sequence.
    fn order(&self) -> u8 {
        match self {
            Self::Queued => 0,
            Self::Spawn => 1,
            Self::Handshake => 2,
            Self::InitializeProtocol => 3,
            Self::NegotiateCapabilities => 4,
            Self::ListTools => 5,
            Self::ListResources => 6,
            Self::ListPrompts => 7,
            Self::CacheWarmup => 8,
            Self::Ready => 9,
            Self::Closed => 254,
            Self::Failed => 255,
        }
    }

    /// `true` if a transition from `self` to `next` is legal.
    ///
    /// The legal moves form a forward DAG with two terminal lanes:
    ///
    /// * Handshake phases (`Queued`..`CacheWarmup`) advance forward
    ///   to any later handshake phase, or jump to `Failed`.
    /// * `Ready` may transition to `Closed` (graceful) or `Failed`
    ///   (runtime error) — it is **not** terminal despite being the
    ///   "happy path" end of the handshake.
    /// * `Closed` and `Failed` are both terminal; self-loops and
    ///   transitions out of a terminal are rejected.
    pub fn can_transition_to(&self, next: McpClientState) -> bool {
        if self.is_terminal() || *self == next {
            return false;
        }
        // Ready has its own two-lane exit: Closed or Failed.
        if matches!(self, Self::Ready) {
            return matches!(next, Self::Closed | Self::Failed);
        }
        // Handshake phases: Failed is reachable from anywhere,
        // forward moves go through the linear order.
        if matches!(next, Self::Failed) {
            return true;
        }
        // Closed is reachable only from Ready (handled above).
        if matches!(next, Self::Closed) {
            return false;
        }
        next.order() > self.order()
    }
}

/// Per-server health record attached to a [`DegradedReport`].
///
/// Symmetric across healthy and failed servers — every server has
/// a `phase` and an optional `error`. A server is "healthy" iff its
/// phase is [`McpClientState::Ready`] and `error` is `None`.
#[derive(Clone, Debug)]
pub struct ServerHealth {
    /// The server name as it was registered with the manager.
    pub server: String,
    /// Most advanced phase the lifecycle reached. For healthy
    /// servers this is [`McpClientState::Ready`]; for failed
    /// servers it is the last phase that was attempted.
    pub phase: McpClientState,
    /// Stringified error message from the underlying [`McpError`],
    /// when the server failed to reach `Ready`. Stored as `String`
    /// so the report is `Clone + Send + Sync` even for error
    /// variants that carry non-clone payloads.
    pub error: Option<String>,
}

impl ServerHealth {
    /// `true` if this server reached `Ready` without failure.
    pub fn is_healthy(&self) -> bool {
        self.phase.is_ready() && self.error.is_none()
    }
}

#[cfg(test)]
mod lifecycle_phase_tests {
    use super::*;

    #[test]
    fn forward_transition_succeeds() {
        assert!(McpClientState::Queued.can_transition_to(McpClientState::Spawn));
        assert!(McpClientState::Spawn.can_transition_to(McpClientState::Handshake));
        assert!(McpClientState::Handshake.can_transition_to(McpClientState::Ready));
    }

    #[test]
    fn backward_transition_rejected() {
        assert!(!McpClientState::Ready.can_transition_to(McpClientState::Spawn));
        assert!(!McpClientState::Handshake.can_transition_to(McpClientState::Spawn));
    }

    #[test]
    fn self_loop_rejected() {
        for p in [
            McpClientState::Queued,
            McpClientState::Spawn,
            McpClientState::Handshake,
            McpClientState::Ready,
            McpClientState::Closed,
            McpClientState::Failed,
        ] {
            assert!(!p.can_transition_to(p));
        }
    }

    #[test]
    fn failed_reachable_from_any_non_terminal() {
        for start in [
            McpClientState::Queued,
            McpClientState::Spawn,
            McpClientState::Handshake,
            McpClientState::ListTools,
            McpClientState::Ready,
        ] {
            assert!(start.can_transition_to(McpClientState::Failed));
        }
    }

    #[test]
    fn ready_exits_via_closed_or_failed_only() {
        assert!(McpClientState::Ready.can_transition_to(McpClientState::Closed));
        assert!(McpClientState::Ready.can_transition_to(McpClientState::Failed));
        // Ready cannot fall back into the handshake phases.
        assert!(!McpClientState::Ready.can_transition_to(McpClientState::ListTools));
        assert!(!McpClientState::Ready.can_transition_to(McpClientState::Spawn));
    }

    #[test]
    fn closed_is_only_reachable_from_ready() {
        for start in [
            McpClientState::Queued,
            McpClientState::Spawn,
            McpClientState::Handshake,
            McpClientState::ListTools,
        ] {
            assert!(!start.can_transition_to(McpClientState::Closed));
        }
    }

    #[test]
    fn terminal_phases_reject_all() {
        for terminal in [McpClientState::Closed, McpClientState::Failed] {
            for target in [
                McpClientState::Queued,
                McpClientState::Spawn,
                McpClientState::Ready,
                McpClientState::Closed,
                McpClientState::Failed,
            ] {
                assert!(
                    !terminal.can_transition_to(target),
                    "terminal {terminal:?} must reject {target:?}"
                );
            }
        }
        assert!(McpClientState::Closed.is_terminal());
        assert!(McpClientState::Failed.is_terminal());
        assert!(!McpClientState::Ready.is_terminal());
    }
}

/// Per-manager health snapshot after a bulk `add_server` pass.
///
/// Agent builders call [`crate::mcp::McpManager::add_server_tracked`]
/// in a loop and collect failures into this report instead of aborting
/// on the first per-server error. Runtime consumers can read the
/// report via [`crate::mcp::McpManager::degraded_report_snapshot`] to
/// decide whether to surface a warning, retry failed servers on the
/// background, or continue with the healthy subset.
///
/// The report is **symmetric** — every server is keyed under
/// `servers` regardless of health, with [`ServerHealth::is_healthy`]
/// telling consumers which subset is usable. Convenience accessors
/// [`Self::healthy_ids`] / [`Self::failed_ids`] / [`Self::is_healthy`]
/// preserve the read-mostly API.
#[derive(Clone, Debug, Default)]
pub struct DegradedReport {
    /// All registered servers, keyed by name.
    pub servers: std::collections::BTreeMap<String, ServerHealth>,
}

impl DegradedReport {
    /// `true` if every registered server reached `Ready`. An empty
    /// report (no servers at all) is considered non-degraded.
    pub fn is_healthy(&self) -> bool {
        self.servers.values().all(ServerHealth::is_healthy)
    }

    /// Iterator over names of healthy servers (alphabetically by
    /// `BTreeMap` ordering).
    pub fn healthy_ids(&self) -> impl Iterator<Item = &str> {
        self.servers
            .values()
            .filter(|h| h.is_healthy())
            .map(|h| h.server.as_str())
    }

    /// Iterator over names of failed servers.
    pub fn failed_ids(&self) -> impl Iterator<Item = &str> {
        self.servers
            .values()
            .filter(|h| !h.is_healthy())
            .map(|h| h.server.as_str())
    }

    /// Record a successful handshake. Overwrites any prior entry —
    /// a retry that reaches `Ready` clears the degraded marker.
    pub fn record_healthy(&mut self, name: impl Into<String>) {
        let name = name.into();
        self.servers.insert(
            name.clone(),
            ServerHealth {
                server: name,
                phase: McpClientState::Ready,
                error: None,
            },
        );
    }

    /// Record a per-server failure along with the phase it reached.
    pub fn record_failure(
        &mut self,
        name: impl Into<String>,
        phase: McpClientState,
        error: impl std::fmt::Display,
    ) {
        let name = name.into();
        self.servers.insert(
            name.clone(),
            ServerHealth {
                server: name,
                phase,
                error: Some(error.to_string()),
            },
        );
    }
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

/// Point-in-time snapshot of an MCP server's runtime state.
///
/// Carries the connection `state` (the canonical handshake FSM), any
/// negotiated `server_info`, and the discovered tool / resource
/// catalogues. Returned by [`McpManager::server_snapshot`] and held
/// internally by [`client::McpClient`].
///
/// This is a **data container**, not a state machine — mutation goes
/// through the owning [`client::McpClient`], which drives the
/// [`McpClientState`] FSM via [`McpClientState::can_transition_to`].
#[derive(Clone, Debug)]
pub struct McpServerSnapshot {
    pub name: String,
    pub config: McpServerConfig,
    pub state: McpClientState,
    pub server_info: Option<McpServerInfo>,
    pub tools: Vec<McpToolDefinition>,
    pub resources: Vec<McpResourceDefinition>,
}

impl McpServerSnapshot {
    pub fn new(name: impl Into<String>, config: McpServerConfig) -> Self {
        Self {
            name: name.into(),
            config,
            state: McpClientState::Queued,
            server_info: None,
            tools: Vec::new(),
            resources: Vec::new(),
        }
    }

    /// `true` if the handshake FSM has reached the `Ready` terminal.
    /// This is the canonical "connection is live" check — there is no
    /// separate connection-status enum.
    pub fn is_ready(&self) -> bool {
        self.state == McpClientState::Ready
    }
}

#[non_exhaustive]
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

#[non_exhaustive]
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
    fn test_mcp_server_snapshot_new() {
        let snapshot = McpServerSnapshot::new(
            "test",
            McpServerConfig::Stdio {
                command: "test".to_string(),
                args: vec![],
                env: HashMap::new(),
                cwd: None,
            },
        );

        assert_eq!(snapshot.name, "test");
        // Fresh snapshot starts at the beginning of the FSM, not Ready.
        assert_eq!(snapshot.state, McpClientState::Queued);
        assert!(!snapshot.is_ready());
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
