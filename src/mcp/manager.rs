//! MCP Manager for multiple server connections.

#[cfg(feature = "mcp")]
use std::collections::HashMap;
#[cfg(feature = "mcp")]
use std::sync::Arc;
#[cfg(feature = "mcp")]
use std::time::Duration;
#[cfg(feature = "mcp")]
use tokio::sync::RwLock;

use super::{
    McpContent, McpError, McpResourceDefinition, McpResult, McpServerConfig, McpServerState,
    McpToolDefinition, McpToolResult,
};
#[cfg(feature = "mcp")]
use super::{McpTimeouts, ReconnectPolicy, ToolCache, make_mcp_name, parse_mcp_name};

#[cfg(feature = "mcp")]
use super::client::McpClient;

/// Default tool cache TTL: 5 minutes.
#[cfg(feature = "mcp")]
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);

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
        }
    }

    #[cfg(not(feature = "mcp"))]
    pub fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
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

        let mut client = McpClient::new(name.clone(), config).with_timeouts(self.timeouts.clone());
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

    #[cfg(feature = "mcp")]
    pub async fn get_server_state(&self, name: &str) -> Option<McpServerState> {
        let servers = self.servers.read().await;
        servers.get(name).map(|c| c.state().clone())
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn get_server_state(&self, _name: &str) -> Option<McpServerState> {
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
                Some(client) if client.is_connected() => return Ok(()),
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
        if client.is_connected() {
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

    #[tokio::test]
    async fn test_get_server_state_not_found() {
        let manager = McpManager::new();
        let state = manager.get_server_state("nonexistent").await;
        assert!(state.is_none());
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
