//! MCP Toolset configuration for API requests with deferred loading support.
//!
//! # Relationship to `McpManager`
//!
//! `McpToolsetRegistry` and [`super::McpManager`] live side-by-side
//! but address orthogonal concerns and are intentionally **not**
//! consolidated:
//!
//! * **`McpToolsetRegistry`** — build-time declarative policy lookup.
//!   Answers "should this `(server, tool)` be deferred until first
//!   use?". Populated once from agent config; never mutates at
//!   runtime; owns no connections.
//! * **`McpManager`** — runtime lifecycle surface. Owns live
//!   `McpClient` instances, tracks handshake state via
//!   [`super::McpClientState`], handles reconnects, accumulates
//!   [`super::DegradedReport`] entries. Never consults deferred-load
//!   policy — that is the caller's responsibility before dispatching
//!   a tool call.
//!
//! Per the project's naming taxonomy (Registry = init-time keyed
//! lookup; Manager = lifecycle ownership) these two names are
//! already correctly applied.

#![allow(missing_docs)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolLoadConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
}

impl ToolLoadConfig {
    pub fn deferred() -> Self {
        Self {
            defer_loading: Some(true),
        }
    }

    pub fn immediate() -> Self {
        Self {
            defer_loading: Some(false),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolset {
    #[serde(rename = "type")]
    pub toolset_type: String,
    pub mcp_server_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_config: Option<ToolLoadConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configs: Option<HashMap<String, ToolLoadConfig>>,
}

impl McpToolset {
    pub fn new(server_name: impl Into<String>) -> Self {
        Self {
            toolset_type: "mcp_toolset".to_string(),
            mcp_server_name: server_name.into(),
            default_config: None,
            configs: None,
        }
    }

    pub fn defer_all(mut self) -> Self {
        self.default_config = Some(ToolLoadConfig::deferred());
        self
    }

    pub fn keep_loaded(mut self, tool_names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let configs = self.configs.get_or_insert_with(HashMap::new);
        for name in tool_names {
            configs.insert(name.into(), ToolLoadConfig::immediate());
        }
        self
    }

    pub fn defer_tools(mut self, tool_names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let configs = self.configs.get_or_insert_with(HashMap::new);
        for name in tool_names {
            configs.insert(name.into(), ToolLoadConfig::deferred());
        }
        self
    }

    pub fn is_deferred(&self, tool_name: &str) -> bool {
        if let Some(defer) = self
            .configs
            .as_ref()
            .and_then(|c| c.get(tool_name))
            .and_then(|c| c.defer_loading)
        {
            return defer;
        }
        self.default_config
            .as_ref()
            .and_then(|c| c.defer_loading)
            .unwrap_or(false)
    }

    pub fn server_name(&self) -> &str {
        &self.mcp_server_name
    }
}

#[derive(Debug, Clone, Default)]
pub struct McpToolsetRegistry {
    toolsets: HashMap<String, McpToolset>,
}

impl McpToolsetRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, toolset: McpToolset) {
        self.toolsets
            .insert(toolset.mcp_server_name.clone(), toolset);
    }

    pub fn get(&self, server_name: &str) -> Option<&McpToolset> {
        self.toolsets.get(server_name)
    }

    pub fn is_deferred(&self, server_name: &str, tool_name: &str) -> bool {
        self.toolsets
            .get(server_name)
            .map(|ts| ts.is_deferred(tool_name))
            .unwrap_or(false)
    }

    pub fn iter(&self) -> impl Iterator<Item = &McpToolset> {
        self.toolsets.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_toolset_defer_all() {
        let toolset = McpToolset::new("database").defer_all();
        assert!(toolset.is_deferred("any_tool"));
    }

    #[test]
    fn test_toolset_keep_loaded() {
        let toolset = McpToolset::new("database")
            .defer_all()
            .keep_loaded(["search_events"]);

        assert!(!toolset.is_deferred("search_events"));
        assert!(toolset.is_deferred("other_tool"));
    }

    #[test]
    fn test_toolset_serialization() {
        let toolset = McpToolset::new("database")
            .defer_all()
            .keep_loaded(["search"]);

        let json = serde_json::to_string_pretty(&toolset).unwrap();
        assert!(json.contains("mcp_toolset"));
        assert!(json.contains("database"));
    }

    #[test]
    fn test_registry() {
        let mut registry = McpToolsetRegistry::new();
        registry.register(McpToolset::new("server1").defer_all());
        registry.register(McpToolset::new("server2"));

        assert!(registry.is_deferred("server1", "any_tool"));
        assert!(!registry.is_deferred("server2", "any_tool"));
        assert!(!registry.is_deferred("server3", "any_tool"));
    }
}
