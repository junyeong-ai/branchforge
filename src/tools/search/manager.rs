//! Tool search manager for coordinating search operations.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::engine::{SearchEngine, SearchMode};
use super::index::{ToolIndex, ToolIndexEntry};
use crate::mcp::{McpManager, McpToolDefinition, McpToolsetRegistry};
use crate::types::ToolDefinition;

#[derive(Debug, Clone)]
pub struct ToolSearchConfig {
    pub threshold: f64,
    pub context_window: usize,
    pub search_mode: SearchMode,
    pub max_results: usize,
    pub always_load: Vec<String>,
}

impl Default for ToolSearchConfig {
    fn default() -> Self {
        Self {
            threshold: 0.10,
            context_window: 200_000,
            search_mode: SearchMode::Regex,
            max_results: 5,
            always_load: Vec::new(),
        }
    }
}

impl ToolSearchConfig {
    pub fn threshold_tokens(&self) -> usize {
        (self.context_window as f64 * self.threshold) as usize
    }

    pub fn threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold.clamp(0.0, 1.0);
        self
    }

    pub fn context_window(mut self, tokens: usize) -> Self {
        self.context_window = tokens;
        self
    }

    pub fn search_mode(mut self, mode: SearchMode) -> Self {
        self.search_mode = mode;
        self
    }

    pub fn always_load(mut self, tools: Vec<String>) -> Self {
        self.always_load = tools;
        self
    }
}

pub struct ToolSearchManager {
    config: ToolSearchConfig,
    index: Arc<RwLock<ToolIndex>>,
    definitions: Arc<RwLock<HashMap<String, McpToolDefinition>>>,
    engine: SearchEngine,
    toolset_registry: Arc<RwLock<McpToolsetRegistry>>,
}

impl ToolSearchManager {
    pub fn new(config: ToolSearchConfig) -> Self {
        let engine = SearchEngine::new(config.search_mode);
        Self {
            config,
            index: Arc::new(RwLock::new(ToolIndex::new())),
            definitions: Arc::new(RwLock::new(HashMap::new())),
            engine,
            toolset_registry: Arc::new(RwLock::new(McpToolsetRegistry::new())),
        }
    }

    pub fn config(&self) -> &ToolSearchConfig {
        &self.config
    }

    pub fn set_toolset_registry(&self, registry: McpToolsetRegistry) {
        if let Ok(mut guard) = self.toolset_registry.try_write() {
            *guard = registry;
        }
    }

    pub async fn build_index(&self, mcp_manager: &McpManager) {
        let tools = mcp_manager.list_tools().await;

        let mut index = self.index.write().await;
        let mut definitions = self.definitions.write().await;

        index.clear();
        definitions.clear();

        for (qualified_name, tool) in tools {
            if let Some((server, _)) = crate::mcp::parse_mcp_name(&qualified_name) {
                let entry = ToolIndexEntry::from_mcp_tool(server, &tool);
                index.add(entry);
                definitions.insert(qualified_name, tool);
            }
        }
    }

    pub async fn should_use_search(&self) -> bool {
        let index = self.index.read().await;
        index.total_tokens() > self.config.threshold_tokens()
    }

    pub async fn total_tokens(&self) -> usize {
        self.index.read().await.total_tokens()
    }

    pub async fn tool_count(&self) -> usize {
        self.index.read().await.len()
    }

    pub async fn prepare_tools(&self) -> PreparedTools {
        let index = self.index.read().await;
        let definitions = self.definitions.read().await;
        let toolset_registry = self.toolset_registry.read().await;

        let use_search = index.total_tokens() > self.config.threshold_tokens();
        let mut immediate = Vec::new();
        let mut deferred = Vec::new();

        for entry in index.entries() {
            let Some(def) = definitions.get(&entry.qualified_name) else {
                continue;
            };

            let is_always_load = self.config.always_load.contains(&entry.qualified_name)
                || self.config.always_load.contains(&entry.tool_name);

            // always_load has highest priority - never defer these tools
            if is_always_load {
                let tool_def = ToolDefinition {
                    name: entry.qualified_name.clone(),
                    description: def.description.clone(),
                    input_schema: def.input_schema.clone(),
                    strict: None,
                    defer_loading: None,
                };
                immediate.push(tool_def);
                continue;
            }

            // Toolset config takes precedence over automatic threshold
            let toolset_deferred =
                toolset_registry.is_deferred(&entry.server_name, &entry.tool_name);

            // Defer if: toolset explicitly requests OR threshold exceeded
            let should_defer = toolset_deferred || use_search;

            let tool_def = ToolDefinition {
                name: entry.qualified_name.clone(),
                description: def.description.clone(),
                input_schema: def.input_schema.clone(),
                strict: None,
                defer_loading: if should_defer { Some(true) } else { None },
            };

            if should_defer {
                deferred.push(tool_def);
            } else {
                immediate.push(tool_def);
            }
        }

        PreparedTools {
            use_search,
            search_mode: self.config.search_mode,
            immediate,
            deferred,
            total_tokens: index.total_tokens(),
            threshold_tokens: self.config.threshold_tokens(),
        }
    }

    pub async fn search(&self, query: &str) -> Vec<String> {
        let index = self.index.read().await;
        let hits = self.engine.search(&index, query, self.config.max_results);
        hits.into_iter().map(|h| h.entry.qualified_name).collect()
    }

    pub async fn get_definition(&self, qualified_name: &str) -> Option<ToolDefinition> {
        let definitions = self.definitions.read().await;
        definitions.get(qualified_name).map(|def| ToolDefinition {
            name: qualified_name.to_string(),
            description: def.description.clone(),
            input_schema: def.input_schema.clone(),
            strict: None,
            defer_loading: None,
        })
    }

    pub async fn get_definitions(&self, names: &[String]) -> Vec<ToolDefinition> {
        let definitions = self.definitions.read().await;
        names
            .iter()
            .filter_map(|name| {
                definitions.get(name).map(|def| ToolDefinition {
                    name: name.clone(),
                    description: def.description.clone(),
                    input_schema: def.input_schema.clone(),
                    strict: None,
                    defer_loading: None,
                })
            })
            .collect()
    }
}

impl Default for ToolSearchManager {
    fn default() -> Self {
        Self::new(ToolSearchConfig::default())
    }
}

#[derive(Debug)]
pub struct PreparedTools {
    pub use_search: bool,
    pub search_mode: SearchMode,
    pub immediate: Vec<ToolDefinition>,
    pub deferred: Vec<ToolDefinition>,
    pub total_tokens: usize,
    pub threshold_tokens: usize,
}

impl PreparedTools {
    pub fn all_tools(&self) -> impl Iterator<Item = &ToolDefinition> {
        self.immediate.iter().chain(self.deferred.iter())
    }

    pub fn filtered_for_access(&self, access: &crate::tools::ToolSurface) -> Self {
        let mut immediate: Vec<_> = self
            .immediate
            .iter()
            .filter(|tool| access.is_allowed(&tool.name))
            .cloned()
            .collect();
        let mut deferred: Vec<_> = self
            .deferred
            .iter()
            .filter(|tool| access.is_allowed(&tool.name))
            .cloned()
            .collect();

        let total_tokens = immediate
            .iter()
            .chain(deferred.iter())
            .map(ToolDefinition::estimated_tokens)
            .sum::<usize>();
        let use_search =
            self.use_search && !deferred.is_empty() && total_tokens > self.threshold_tokens;

        if !use_search {
            immediate.append(&mut deferred);
        }

        Self {
            use_search,
            search_mode: self.search_mode,
            immediate,
            deferred,
            total_tokens,
            threshold_tokens: self.threshold_tokens,
        }
    }

    pub fn token_savings(&self) -> usize {
        if self.use_search {
            self.deferred
                .iter()
                .map(|t| t.estimated_tokens())
                .sum::<usize>()
        } else {
            0
        }
    }
}

impl ToolSearchManager {
    pub async fn prepare_tools_for_access(
        &self,
        access: &crate::tools::ToolSurface,
    ) -> PreparedTools {
        self.prepare_tools().await.filtered_for_access(access)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolSurface;

    #[test]
    fn test_config_threshold_tokens() {
        let config = ToolSearchConfig::default();
        assert_eq!(config.threshold_tokens(), 20_000); // 10% of 200k
    }

    #[test]
    fn test_config_builder() {
        let config = ToolSearchConfig::default()
            .threshold(0.05)
            .context_window(100_000)
            .search_mode(SearchMode::Bm25);

        assert_eq!(config.threshold, 0.05);
        assert_eq!(config.context_window, 100_000);
        assert_eq!(config.search_mode, SearchMode::Bm25);
        assert_eq!(config.threshold_tokens(), 5_000);
    }

    #[tokio::test]
    async fn test_manager_creation() {
        let manager = ToolSearchManager::default();
        assert!(!manager.should_use_search().await);
        assert_eq!(manager.total_tokens().await, 0);
    }

    #[test]
    fn filtered_tools_hide_denied_entries_and_disable_search_when_empty() {
        let prepared = PreparedTools {
            use_search: true,
            search_mode: SearchMode::Regex,
            immediate: vec![],
            deferred: vec![ToolDefinition::new(
                "mcp__filesystem__read_file",
                "Read a file",
                serde_json::json!({"type": "object"}),
            )],
            total_tokens: 100,
            threshold_tokens: 1,
        };

        let filtered = prepared.filtered_for_access(&ToolSurface::none());

        assert!(!filtered.use_search);
        assert!(filtered.immediate.is_empty());
        assert!(filtered.deferred.is_empty());
        assert_eq!(filtered.total_tokens, 0);
    }

    #[test]
    fn filtered_tools_keep_only_allowed_mcp_metadata() {
        let prepared = PreparedTools {
            use_search: true,
            search_mode: SearchMode::Regex,
            immediate: vec![],
            deferred: vec![
                ToolDefinition::new(
                    "mcp__context7__search",
                    "Search docs",
                    serde_json::json!({"type": "object"}),
                ),
                ToolDefinition::new(
                    "mcp__filesystem__read_file",
                    "Read a file",
                    serde_json::json!({"type": "object"}),
                ),
            ],
            total_tokens: 10_000,
            threshold_tokens: 1,
        };

        let filtered = prepared.filtered_for_access(&ToolSurface::only(["mcp__context7__search"]));

        assert!(filtered.use_search);
        assert!(filtered.immediate.is_empty());
        assert_eq!(filtered.deferred.len(), 1);
        assert_eq!(filtered.deferred[0].name, "mcp__context7__search");
    }
}
