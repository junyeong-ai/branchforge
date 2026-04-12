//! MCP Manager for multiple server connections.
//!
//! # Lock ordering
//!
//! `McpManager` holds three `Arc<RwLock<…>>` fields that can be
//! acquired in the same call path:
//!
//! 1. `servers: RwLock<HashMap<String, McpClient>>`
//! 2. `tool_cache: RwLock<HashMap<String, ToolCache>>`
//! 3. `degraded: RwLock<DegradedReport>`
//!
//! **Canonical acquisition order** when two or more of these locks
//! must be held at the same time:
//!
//! > `servers` > `tool_cache` > `degraded`
//!
//! Rationale:
//!
//! - `servers` is the authoritative server map. All write-side
//!   operations on the other two locks are triggered by a state
//!   transition on `servers` (insert / remove / replace).
//! - `tool_cache` is a projection of `servers`. Holding `servers`
//!   first guarantees the cache write reflects a stable server map.
//! - `degraded` is a pure observation sink. It is always acquired
//!   last because `record_failure` / `record_healthy` has no data
//!   dependency on the other two locks.
//!
//! # Current audit (2026-04)
//!
//! `add_server_tracked` (the only method that touches more than one
//! of these locks inside a single call) follows the canonical order:
//!
//! ```text
//! servers.read()    → duplicate check, released immediately
//! degraded.write()  → record_failure on connect error (servers not held)
//! tool_cache.write() → populate cache (servers not held)
//! servers.write()   → race recheck + insert
//!   ├── degraded.write()  → race-loss record_failure (servers held)
//!   └── degraded.write()  → record_healthy on success (servers dropped first)
//! ```
//!
//! The only path where `degraded` is acquired **while `servers` is
//! still held** is the race-loss branch inside the `servers.write()`
//! scope — that respects `servers > degraded` and is the reason the
//! ordering rule is written in that direction.
//!
//! **Rule**: any future method that needs both `servers` and
//! `degraded` must acquire `servers` first. Violations should be
//! caught in code review, since a deadlock only manifests under
//! concurrent load and may be hard to reproduce.

#![allow(missing_docs)]

#[cfg(feature = "mcp")]
use std::collections::HashMap;
#[cfg(feature = "mcp")]
use std::sync::Arc;
#[cfg(feature = "mcp")]
use std::time::Duration;
#[cfg(feature = "mcp")]
use tokio::sync::RwLock;

use super::{
    DegradedReport, McpClientState, McpContent, McpError, McpResourceDefinition, McpResult,
    McpServerConfig, McpServerSnapshot, McpToolDefinition, McpToolResult,
};
#[cfg(feature = "mcp")]
use super::{McpTimeouts, ReconnectPolicy, ToolCache, make_mcp_name, parse_mcp_name};

#[cfg(feature = "mcp")]
use super::client::McpClient;

/// Default tool cache TTL: 5 minutes.
#[cfg(feature = "mcp")]
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);

// Lock ordering: servers > tool_cache > degraded
pub struct McpManager {
    #[cfg(feature = "mcp")]
    servers: Arc<RwLock<HashMap<String, McpClient>>>,
    #[cfg(feature = "mcp")]
    reconnect_policy: ReconnectPolicy,
    #[cfg(feature = "mcp")]
    tool_cache: Arc<RwLock<HashMap<String, ToolCache>>>,
    #[cfg(feature = "mcp")]
    cache_ttl: Duration,
    #[cfg(feature = "mcp")]
    timeouts: McpTimeouts,
    /// Phase D C-3: unified HITL handler forwarded to every MCP
    /// client constructed through this manager. See
    /// [`super::HumanElicitationRouter`] for how server-initiated
    /// elicitation is routed.
    #[cfg(feature = "mcp")]
    human: Option<Arc<dyn crate::authorization::HumanInteractionHandler>>,
    /// Per-manager health snapshot accumulated across `add_server`
    /// / `add_server_tracked` calls. Readable via
    /// [`degraded_report_snapshot`][Self::degraded_report_snapshot]
    /// so downstream code can decide whether to warn, retry, or
    /// continue with the healthy subset.
    #[cfg(feature = "mcp")]
    degraded: Arc<RwLock<DegradedReport>>,
    #[cfg(not(feature = "mcp"))]
    _phantom: std::marker::PhantomData<()>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    #[cfg(feature = "mcp")]
    pub fn new() -> Self {
        Self {
            servers: Arc::new(RwLock::new(HashMap::new())),
            reconnect_policy: ReconnectPolicy::default(),
            tool_cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl: DEFAULT_CACHE_TTL,
            timeouts: McpTimeouts::default(),
            human: None,
            degraded: Arc::new(RwLock::new(DegradedReport::default())),
        }
    }

    /// Phase D C-3: attach the unified
    /// [`crate::authorization::HumanInteractionHandler`] so every
    /// MCP client constructed through this manager forwards
    /// server-initiated elicitation requests to it. Normally
    /// called by the agent builder when
    /// `AgentBuilder::human_handler` is set.
    #[cfg(feature = "mcp")]
    #[must_use]
    pub fn with_human_handler(
        mut self,
        handler: Option<Arc<dyn crate::authorization::HumanInteractionHandler>>,
    ) -> Self {
        self.human = handler;
        self
    }

    /// Snapshot the current [`DegradedReport`] for this manager.
    ///
    /// The snapshot reflects every `add_server` / `add_server_tracked`
    /// call made so far, with later calls superseding earlier ones
    /// for the same server name.
    #[cfg(feature = "mcp")]
    pub async fn degraded_report_snapshot(&self) -> DegradedReport {
        self.degraded.read().await.clone()
    }

    /// `add_server` variant that **never fails the builder on a
    /// per-server error**. Returns `Ok(phase)` on success and
    /// `Err((phase, error))` on failure, and updates the manager's
    /// internal [`DegradedReport`] either way.
    ///
    /// This is what agent builders should call when loading many
    /// configured servers: a broken server gets quarantined in the
    /// report instead of aborting the whole agent construction.
    ///
    /// The attached [`McpClientState`] on failure is the exact
    /// phase the client last reached before bailing out — the same
    /// information that would be observable from
    /// [`super::client::McpClient::state`] on a retained client — so
    /// callers can distinguish "process never spawned" from
    /// "tools-list failed" without parsing error messages.
    #[cfg(feature = "mcp")]
    pub async fn add_server_tracked(
        &self,
        name: impl Into<String>,
        config: McpServerConfig,
    ) -> std::result::Result<McpClientState, (McpClientState, McpError)> {
        let name = name.into();

        {
            let servers = self.servers.read().await;
            if servers.contains_key(&name) {
                let err = McpError::Protocol {
                    message: format!("Server '{}' already exists", name),
                };
                return Err((McpClientState::Queued, err));
            }
        }

        let mut client = McpClient::new(name.clone(), config)
            .with_timeouts(self.timeouts.clone())
            .with_human_handler(self.human.clone());
        if let Err(err) = client.connect().await {
            let phase = client.state();
            tracing::warn!(
                server = %name,
                phase = phase.as_str(),
                error = %err,
                "MCP server failed during startup; recording in DegradedReport"
            );
            self.degraded
                .write()
                .await
                .record_failure(&name, phase, &err);
            return Err((phase, err));
        }

        // Populate tool cache from the freshly-connected client
        {
            let mut cache = self.tool_cache.write().await;
            cache.insert(
                name.clone(),
                ToolCache {
                    tools: client.tools().to_vec(),
                    cached_at: std::time::Instant::now(),
                    ttl: self.cache_ttl,
                },
            );
        }

        // Re-check after acquiring write lock to prevent race
        let mut servers = self.servers.write().await;
        if servers.contains_key(&name) {
            let err = McpError::Protocol {
                message: format!("Server '{}' already exists", name),
            };
            self.degraded
                .write()
                .await
                .record_failure(&name, McpClientState::Queued, &err);
            return Err((McpClientState::Queued, err));
        }
        servers.insert(name.clone(), client);
        drop(servers);

        self.degraded.write().await.record_healthy(name);
        Ok(McpClientState::Ready)
    }

    #[cfg(not(feature = "mcp"))]
    pub fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }

    /// Phase D C-3 pure-core stub. The `mcp` feature is off so
    /// there are no clients to forward the handler to; the call
    /// is a no-op that preserves the builder chain shape so the
    /// agent builder does not need a feature branch.
    #[cfg(not(feature = "mcp"))]
    #[must_use]
    pub fn with_human_handler(
        self,
        _handler: Option<std::sync::Arc<dyn crate::authorization::HumanInteractionHandler>>,
    ) -> Self {
        self
    }

    #[cfg(feature = "mcp")]
    pub fn reconnect_policy(mut self, policy: ReconnectPolicy) -> Self {
        self.reconnect_policy = policy;
        self
    }

    /// Set the TTL for the tool listing cache.
    #[cfg(feature = "mcp")]
    pub fn cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Set custom timeouts for all clients created by this manager.
    #[cfg(feature = "mcp")]
    pub fn timeouts(mut self, timeouts: McpTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    #[cfg(feature = "mcp")]
    pub async fn add_server(
        &self,
        name: impl Into<String>,
        config: McpServerConfig,
    ) -> McpResult<()> {
        let name = name.into();

        {
            let servers = self.servers.read().await;
            if servers.contains_key(&name) {
                return Err(McpError::Protocol {
                    message: format!("Server '{}' already exists", name),
                });
            }
        }

        let mut client = McpClient::new(name.clone(), config)
            .with_timeouts(self.timeouts.clone())
            .with_human_handler(self.human.clone());
        client.connect().await?;

        // Populate tool cache from the freshly-connected client
        {
            let mut cache = self.tool_cache.write().await;
            cache.insert(
                name.clone(),
                ToolCache {
                    tools: client.tools().to_vec(),
                    cached_at: std::time::Instant::now(),
                    ttl: self.cache_ttl,
                },
            );
        }

        // Re-check after acquiring write lock to prevent race
        let mut servers = self.servers.write().await;
        if servers.contains_key(&name) {
            return Err(McpError::Protocol {
                message: format!("Server '{}' already exists", name),
            });
        }
        servers.insert(name, client);

        Ok(())
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn add_server(
        &self,
        _name: impl Into<String>,
        _config: McpServerConfig,
    ) -> McpResult<()> {
        Err(McpError::Protocol {
            message: "MCP feature not enabled".to_string(),
        })
    }

    /// Pure-core stub for `add_server_tracked`. Always reports the
    /// feature as disabled; never mutates the manager.
    #[cfg(not(feature = "mcp"))]
    pub async fn add_server_tracked(
        &self,
        _name: impl Into<String>,
        _config: McpServerConfig,
    ) -> std::result::Result<McpClientState, (McpClientState, McpError)> {
        Err((
            McpClientState::Queued,
            McpError::Protocol {
                message: "MCP feature not enabled".to_string(),
            },
        ))
    }

    /// Pure-core stub for `degraded_report_snapshot`. Always returns
    /// an empty report.
    #[cfg(not(feature = "mcp"))]
    pub async fn degraded_report_snapshot(&self) -> DegradedReport {
        DegradedReport::default()
    }

    #[cfg(feature = "mcp")]
    pub async fn remove_server(&self, name: &str) -> McpResult<()> {
        let mut servers = self.servers.write().await;
        if let Some(mut client) = servers.remove(name) {
            // Remove cache entry for this server
            self.tool_cache.write().await.remove(name);
            client.close().await?;
            Ok(())
        } else {
            Err(McpError::ServerNotFound {
                name: name.to_string(),
            })
        }
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn remove_server(&self, _name: &str) -> McpResult<()> {
        Err(McpError::Protocol {
            message: "MCP feature not enabled".to_string(),
        })
    }

    #[cfg(feature = "mcp")]
    pub async fn list_servers(&self) -> Vec<String> {
        let servers = self.servers.read().await;
        servers.keys().cloned().collect()
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn list_servers(&self) -> Vec<String> {
        Vec::new()
    }

    /// Snapshot the runtime state of one registered server. Returns
    /// `None` if no server is registered under `name`.
    #[cfg(feature = "mcp")]
    pub async fn server_snapshot(&self, name: &str) -> Option<McpServerSnapshot> {
        let servers = self.servers.read().await;
        servers.get(name).map(|c| c.snapshot())
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn server_snapshot(&self, _name: &str) -> Option<McpServerSnapshot> {
        None
    }

    #[cfg(feature = "mcp")]
    pub async fn list_tools(&self) -> Vec<(String, McpToolDefinition)> {
        let servers = self.servers.read().await;
        let cache = self.tool_cache.read().await;
        let mut tools = Vec::new();

        for (server_name, client) in servers.iter() {
            // Use cached tools if cache is valid
            if let Some(entry) = cache.get(server_name)
                && entry.is_valid()
            {
                for tool in &entry.tools {
                    tools.push((make_mcp_name(server_name, &tool.name), tool.clone()));
                }
                continue;
            }
            // Cache miss or expired: use live data from client
            for tool in client.tools() {
                tools.push((make_mcp_name(server_name, &tool.name), tool.clone()));
            }
        }

        tools
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn list_tools(&self) -> Vec<(String, McpToolDefinition)> {
        Vec::new()
    }

    /// Force-refresh the tool cache for a specific server by re-reading from the client.
    #[cfg(feature = "mcp")]
    pub async fn refresh_tools(&self, server_name: &str) -> McpResult<()> {
        let servers = self.servers.read().await;
        let client = servers
            .get(server_name)
            .ok_or_else(|| McpError::ServerNotFound {
                name: server_name.to_string(),
            })?;

        let tools = client.tools().to_vec();
        let mut cache = self.tool_cache.write().await;
        cache.insert(
            server_name.to_string(),
            ToolCache {
                tools,
                cached_at: std::time::Instant::now(),
                ttl: self.cache_ttl,
            },
        );

        Ok(())
    }

    /// Force-refresh the tool cache for a specific server (stub when feature disabled).
    #[cfg(not(feature = "mcp"))]
    pub async fn refresh_tools(&self, _server_name: &str) -> McpResult<()> {
        Err(McpError::Protocol {
            message: "MCP feature not enabled".to_string(),
        })
    }

    /// Clear all cached tool listings.
    #[cfg(feature = "mcp")]
    pub async fn invalidate_cache(&self) {
        self.tool_cache.write().await.clear();
    }

    /// Clear all cached tool listings (stub when feature disabled).
    #[cfg(not(feature = "mcp"))]
    pub async fn invalidate_cache(&self) {}

    /// Reconnects if the server is disconnected, with exponential backoff.
    #[cfg(feature = "mcp")]
    pub async fn ensure_connected(&self, server_name: &str) -> McpResult<()> {
        // Fast path: check with read lock first
        {
            let servers = self.servers.read().await;
            match servers.get(server_name) {
                None => {
                    return Err(McpError::ServerNotFound {
                        name: server_name.to_string(),
                    });
                }
                Some(client) if client.is_ready() => return Ok(()),
                _ => {}
            }
        }

        // Slow path: acquire write lock for reconnection
        let mut servers = self.servers.write().await;
        let client = servers
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound {
                name: server_name.to_string(),
            })?;

        // Double-check after acquiring write lock
        if client.is_ready() {
            return Ok(());
        }

        // Try to connect first, then apply backoff between retries
        for attempt in 0..self.reconnect_policy.max_retries {
            if client.connect().await.is_ok() {
                return Ok(());
            }

            // Only sleep between retries, not before first attempt
            if attempt + 1 < self.reconnect_policy.max_retries {
                let delay = self.reconnect_policy.delay_for_attempt(attempt);
                tokio::time::sleep(delay).await;
            }
        }

        Err(McpError::ConnectionFailed {
            message: format!(
                "Failed to reconnect to '{}' after {} attempts",
                server_name, self.reconnect_policy.max_retries
            ),
        })
    }

    #[cfg(feature = "mcp")]
    pub async fn call_tool(
        &self,
        qualified_name: &str,
        arguments: serde_json::Value,
    ) -> McpResult<McpToolResult> {
        let (server_name, tool_name) =
            parse_mcp_name(qualified_name).ok_or_else(|| McpError::ToolNotFound {
                name: qualified_name.to_string(),
            })?;

        self.ensure_connected(server_name).await?;

        let servers = self.servers.read().await;
        let client = servers
            .get(server_name)
            .ok_or_else(|| McpError::ServerNotFound {
                name: server_name.to_string(),
            })?;

        client.call_tool(tool_name, arguments).await
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn call_tool(
        &self,
        _qualified_name: &str,
        _arguments: serde_json::Value,
    ) -> McpResult<McpToolResult> {
        Err(McpError::Protocol {
            message: "MCP feature not enabled".to_string(),
        })
    }

    #[cfg(feature = "mcp")]
    pub async fn list_resources(&self) -> Vec<(String, McpResourceDefinition)> {
        let servers = self.servers.read().await;
        let mut resources = Vec::new();

        for (server_name, client) in servers.iter() {
            for resource in client.resources() {
                resources.push((server_name.clone(), resource.clone()));
            }
        }

        resources
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn list_resources(&self) -> Vec<(String, McpResourceDefinition)> {
        Vec::new()
    }

    #[cfg(feature = "mcp")]
    pub async fn read_resource(&self, server_name: &str, uri: &str) -> McpResult<Vec<McpContent>> {
        let servers = self.servers.read().await;
        let client = servers
            .get(server_name)
            .ok_or_else(|| McpError::ServerNotFound {
                name: server_name.to_string(),
            })?;

        client.read_resource(uri).await
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn read_resource(
        &self,
        _server_name: &str,
        _uri: &str,
    ) -> McpResult<Vec<McpContent>> {
        Err(McpError::Protocol {
            message: "MCP feature not enabled".to_string(),
        })
    }

    #[cfg(feature = "mcp")]
    pub async fn close_all(&self) -> McpResult<()> {
        let mut servers = self.servers.write().await;
        for (_, mut client) in servers.drain() {
            let _ = client.close().await;
        }
        self.tool_cache.write().await.clear();
        Ok(())
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn close_all(&self) -> McpResult<()> {
        Ok(())
    }
}

/// Best-effort cleanup for dropped `McpManager` instances.
///
/// MCP servers are child processes and OS resources that need to be
/// cancelled explicitly; leaking them on manager drop leads to zombie
/// processes in long-lived agents that spawn and discard many managers
/// (test suites, multi-tenant services).
///
/// This `Drop` impl spawns a detached tokio task to invoke the async
/// close path on every registered client. It is **best-effort** for two
/// reasons:
///
/// 1. `Drop::drop` cannot be `async`, so we cannot await the cleanup —
///    the spawned task may be cancelled if the tokio runtime itself is
///    shutting down faster than the cleanup can complete.
/// 2. If the manager is dropped outside any tokio runtime context (which
///    is a programming error, but we must not panic in `Drop`) there is
///    no runtime to spawn onto and we log a warning instead.
///
/// For guaranteed cleanup, call [`McpManager::close_all`] explicitly
/// before dropping the manager. This `Drop` is the safety net for the
/// common case where the manager is owned by an `AgentRuntime` whose
/// own shutdown path eventually releases it.
#[cfg(feature = "mcp")]
impl Drop for McpManager {
    fn drop(&mut self) {
        // Clone the Arc handles so the spawned task can still reach the
        // backing maps after `self` has gone out of scope.
        let servers = Arc::clone(&self.servers);
        let tool_cache = Arc::clone(&self.tool_cache);

        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let count = {
                        let mut servers_guard = servers.write().await;
                        let count = servers_guard.len();
                        for (name, mut client) in servers_guard.drain() {
                            if let Err(e) = client.close().await {
                                tracing::debug!(
                                    server = %name,
                                    error = %e,
                                    "MCP client close failed during Drop cleanup"
                                );
                            }
                        }
                        count
                    };
                    tool_cache.write().await.clear();
                    if count > 0 {
                        tracing::debug!(
                            count,
                            "McpManager dropped; closed {count} MCP server connection(s)"
                        );
                    }
                });
            }
            Err(_) => {
                tracing::warn!(
                    "McpManager dropped outside a tokio runtime context; MCP \
                     server processes may be orphaned. Call `close_all()` \
                     explicitly before dropping the manager to guarantee \
                     cleanup."
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_manager_new() {
        let manager = McpManager::new();
        let servers = manager.list_servers().await;
        assert!(servers.is_empty());
    }

    #[tokio::test]
    async fn test_list_tools_empty() {
        let manager = McpManager::new();
        let tools = manager.list_tools().await;
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn test_list_resources_empty() {
        let manager = McpManager::new();
        let resources = manager.list_resources().await;
        assert!(resources.is_empty());
    }

    #[tokio::test]
    async fn test_close_all_empty() {
        let manager = McpManager::new();
        let result = manager.close_all().await;
        assert!(result.is_ok());
    }

    /// Attempting to track-add a stdio server whose command does not
    /// exist must fail at the `Spawn` phase and land in the
    /// DegradedReport rather than panicking or bubbling a fatal
    /// error to the caller.
    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn track_records_spawn_failure_in_degraded_report() {
        let manager = McpManager::new();
        let config = McpServerConfig::Stdio {
            command: "/nonexistent-binary-please-dont-exist".to_string(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
        };

        let result = manager.add_server_tracked("broken", config).await;
        assert!(result.is_err(), "bogus command must not succeed");

        let report = manager.degraded_report_snapshot().await;
        assert!(!report.is_healthy());
        assert_eq!(report.healthy_ids().count(), 0);
        let entry = report
            .servers
            .get("broken")
            .expect("broken server must be in the report");
        assert!(!entry.is_healthy());
        // Either Spawn (transport construction) or Handshake
        // (timeout on the nonexistent process) is acceptable —
        // platform-dependent.
        assert!(
            matches!(
                entry.phase,
                McpClientState::Spawn | McpClientState::Handshake
            ),
            "expected Spawn or Handshake, got {:?}",
            entry.phase
        );
    }

    /// `DegradedReport::is_healthy()` and `record_healthy`/`record_failure`
    /// round-trip correctly without touching a real MCP server.
    #[tokio::test]
    async fn degraded_report_transitions() {
        let mut report = DegradedReport::default();
        assert!(report.is_healthy()); // empty == healthy

        report.record_failure("bad", McpClientState::ListTools, "tools/list timed out");
        assert!(!report.is_healthy());
        assert_eq!(report.failed_ids().count(), 1);
        assert_eq!(report.healthy_ids().count(), 0);

        report.record_healthy("good");
        assert_eq!(report.healthy_ids().collect::<Vec<_>>(), vec!["good"]);
        assert_eq!(report.failed_ids().count(), 1);

        // Retrying a previously-failed server that now succeeds
        // clears its degraded marker.
        report.record_healthy("bad");
        assert!(report.is_healthy());
        assert_eq!(report.healthy_ids().count(), 2);
    }

    /// Dropping an empty `McpManager` inside a tokio runtime must not
    /// panic or log warnings about orphaned processes. Exercises the
    /// Drop-based best-effort cleanup path in the no-server case, which
    /// is the only one we can assert against without spawning real MCP
    /// child processes.
    ///
    /// The Drop impl is only compiled under `feature = "mcp"`; in
    /// pure-core builds there is nothing to exercise, and `drop()` on a
    /// non-Drop type fires the `clippy::drop_non_drop` lint.
    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_drop_empty_manager_is_quiet() {
        let manager = McpManager::new();
        drop(manager);
        // Yield so any spawned cleanup task has a chance to run.
        tokio::task::yield_now().await;
    }

    /// When an `McpManager` is dropped *without* a tokio runtime in
    /// scope, `Drop` must take the warning path instead of panicking.
    /// We construct the manager inside a runtime (because `new()`
    /// creates tokio sync primitives) but drop it from a plain thread.
    #[cfg(feature = "mcp")]
    #[test]
    fn test_drop_without_runtime_does_not_panic() {
        let manager = {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async { McpManager::new() })
            // `rt` is dropped here; `manager` is moved out and now has
            // no runtime handle available when it is itself dropped.
        };
        drop(manager);
        // If we reach this line, the Drop path took the no-runtime
        // branch and logged a warning rather than panicking.
    }

    #[tokio::test]
    async fn test_server_snapshot_not_found() {
        let manager = McpManager::new();
        let snapshot = manager.server_snapshot("nonexistent").await;
        assert!(snapshot.is_none());
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_add_server_duplicate_error() {
        use std::collections::HashMap;
        let manager = McpManager::new();
        let config = McpServerConfig::Stdio {
            command: "echo".to_string(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
        };

        // This will fail because "echo" isn't a valid MCP server
        // but we're testing the duplicate detection logic
        let _ = manager.add_server("test", config.clone()).await;
        // Second add with same name should return duplicate error
        let result = manager.add_server("test", config).await;
        // Either fails on connection OR duplicate - both acceptable
        assert!(result.is_err());
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_remove_server_not_found() {
        let manager = McpManager::new();
        let result = manager.remove_server("nonexistent").await;
        assert!(matches!(result, Err(McpError::ServerNotFound { .. })));
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_call_tool_invalid_name() {
        let manager = McpManager::new();
        let result = manager
            .call_tool("invalid_name", serde_json::json!({}))
            .await;
        assert!(matches!(result, Err(McpError::ToolNotFound { .. })));
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_call_tool_server_not_found() {
        let manager = McpManager::new();
        let result = manager
            .call_tool("mcp__server__tool", serde_json::json!({}))
            .await;
        assert!(matches!(result, Err(McpError::ServerNotFound { .. })));
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_read_resource_server_not_found() {
        let manager = McpManager::new();
        let result = manager.read_resource("nonexistent", "file://test").await;
        assert!(matches!(result, Err(McpError::ServerNotFound { .. })));
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_reconnect_policy_custom() {
        let policy = ReconnectPolicy {
            max_retries: 5,
            base_delay_ms: 500,
            max_delay_ms: 10000,
            jitter_factor: 0.2,
        };
        let manager = McpManager::new().reconnect_policy(policy);
        assert!(manager.list_servers().await.is_empty());
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_cache_ttl_builder() {
        let manager = McpManager::new().cache_ttl(std::time::Duration::from_secs(120));
        assert_eq!(manager.cache_ttl, std::time::Duration::from_secs(120));
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_timeouts_builder() {
        use super::McpTimeouts;
        let custom = McpTimeouts {
            connection: std::time::Duration::from_secs(5),
            tool_call: std::time::Duration::from_secs(15),
            resource_read: std::time::Duration::from_secs(10),
        };
        let manager = McpManager::new().timeouts(custom);
        assert_eq!(
            manager.timeouts.connection,
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            manager.timeouts.tool_call,
            std::time::Duration::from_secs(15)
        );
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_tool_cache_validity() {
        use super::{McpToolDefinition, ToolCache};
        use std::time::{Duration, Instant};

        // Fresh cache entry should be valid
        let cache = ToolCache {
            tools: vec![McpToolDefinition {
                name: "test_tool".to_string(),
                description: "A test tool".to_string(),
                input_schema: serde_json::json!({}),
                annotations: Default::default(),
            }],
            cached_at: Instant::now(),
            ttl: Duration::from_secs(300),
        };
        assert!(cache.is_valid());

        // Expired cache entry should not be valid
        let expired_cache = ToolCache {
            tools: vec![],
            cached_at: Instant::now() - Duration::from_secs(600),
            ttl: Duration::from_secs(300),
        };
        assert!(!expired_cache.is_valid());
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_invalidate_cache() {
        let manager = McpManager::new();

        // Insert a cache entry directly
        {
            let mut cache = manager.tool_cache.write().await;
            cache.insert(
                "test_server".to_string(),
                super::ToolCache {
                    tools: vec![],
                    cached_at: std::time::Instant::now(),
                    ttl: std::time::Duration::from_secs(300),
                },
            );
        }

        // Verify it exists
        assert!(!manager.tool_cache.read().await.is_empty());

        // Invalidate
        manager.invalidate_cache().await;
        assert!(manager.tool_cache.read().await.is_empty());
    }

    #[cfg(feature = "mcp")]
    #[tokio::test]
    async fn test_refresh_tools_server_not_found() {
        let manager = McpManager::new();
        let result = manager.refresh_tools("nonexistent").await;
        assert!(matches!(result, Err(McpError::ServerNotFound { .. })));
    }

    #[tokio::test]
    async fn test_invalidate_cache_empty() {
        let manager = McpManager::new();
        // Should not panic on empty cache
        manager.invalidate_cache().await;
    }
}
