use std::collections::HashMap;
use std::path::PathBuf;

use crate::common::IndexRegistry;
use crate::mcp::McpServerConfig;
use crate::skills::SkillIndex;
use crate::subagents::SubagentIndex;

use super::PluginError;
use super::discovery::PluginDiscovery;
use super::loader::{PluginHookEntry, PluginLoader as InternalPluginLoader, PluginResources};
use super::manifest::PluginDescriptor;

pub struct PluginLoader {
    plugins: Vec<PluginDescriptor>,
    resources: PluginResources,
}

impl PluginLoader {
    pub async fn load_from_dirs(dirs: &[PathBuf]) -> Result<Self, PluginError> {
        let plugins = PluginDiscovery::discover(dirs)?;

        Self::validate_plugins(&plugins)?;

        let mut resources = PluginResources::default();

        for plugin in &plugins {
            let plugin_resources = InternalPluginLoader::load(plugin).await?;
            Self::merge(&mut resources, plugin_resources);
        }

        Ok(Self { plugins, resources })
    }

    pub fn register_skills(&self, registry: &mut IndexRegistry<SkillIndex>) {
        for skill in &self.resources.skills {
            registry.register(skill.clone());
        }
    }

    pub fn register_subagents(&self, registry: &mut IndexRegistry<SubagentIndex>) {
        for subagent in &self.resources.subagents {
            registry.register(subagent.clone());
        }
    }

    pub fn hooks(&self) -> &[PluginHookEntry] {
        &self.resources.hooks
    }

    pub fn mcp_servers(&self) -> &HashMap<String, McpServerConfig> {
        &self.resources.mcp_servers
    }

    pub fn plugins(&self) -> &[PluginDescriptor] {
        &self.plugins
    }

    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }

    pub fn has_plugin(&self, name: &str) -> bool {
        self.plugins.iter().any(|p| p.name() == name)
    }

    fn validate_plugins(plugins: &[PluginDescriptor]) -> Result<(), PluginError> {
        let mut seen: HashMap<String, &PathBuf> = HashMap::new();
        for plugin in plugins {
            let name = plugin.name();

            if name.contains(super::namespace::NAMESPACE_SEP) {
                return Err(PluginError::InvalidName {
                    name: name.to_string(),
                    reason: format!(
                        "must not contain namespace separator '{}'",
                        super::namespace::NAMESPACE_SEP
                    ),
                });
            }

            if let Some(first_path) = seen.get(name) {
                return Err(PluginError::DuplicateName {
                    name: name.to_string(),
                    first: (*first_path).clone(),
                    second: plugin.root_dir.clone(),
                });
            }
            seen.insert(name.to_string(), &plugin.root_dir);
        }
        Ok(())
    }

    fn merge(target: &mut PluginResources, source: PluginResources) {
        target.skills.extend(source.skills);
        target.subagents.extend(source.subagents);
        target.hooks.extend(source.hooks);
        target.mcp_servers.extend(source.mcp_servers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_full_plugin(parent: &std::path::Path, name: &str) {
        let plugin_dir = parent.join(name);
        let config_dir = plugin_dir.join(".claude-plugin");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("plugin.json"),
            format!(
                r#"{{"name":"{}","description":"Test","version":"1.0.0"}}"#,
                name
            ),
        )
        .unwrap();

        let skills_dir = plugin_dir.join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("test.skill.md"),
            format!(
                "---\nname: test-skill\ndescription: A test skill for {}\n---\nContent",
                name
            ),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_load_from_dirs() {
        let dir = tempdir().unwrap();
        create_full_plugin(dir.path(), "plugin-a");
        create_full_plugin(dir.path(), "plugin-b");

        let manager = PluginLoader::load_from_dirs(&[dir.path().to_path_buf()])
            .await
            .unwrap();

        assert_eq!(manager.plugin_count(), 2);
        assert!(manager.has_plugin("plugin-a"));
        assert!(manager.has_plugin("plugin-b"));
        assert!(!manager.has_plugin("plugin-c"));
        let mut registry = IndexRegistry::new();
        manager.register_skills(&mut registry);
        assert_eq!(registry.len(), 2);
    }

    #[tokio::test]
    async fn test_duplicate_detection() {
        let dir1 = tempdir().unwrap();
        let dir2 = tempdir().unwrap();

        let plugin_dir1 = dir1.path().join("same");
        let config1 = plugin_dir1.join(".claude-plugin");
        std::fs::create_dir_all(&config1).unwrap();
        std::fs::write(
            config1.join("plugin.json"),
            r#"{"name":"same","description":"A","version":"1.0.0"}"#,
        )
        .unwrap();

        let plugin_dir2 = dir2.path().join("same");
        let config2 = plugin_dir2.join(".claude-plugin");
        std::fs::create_dir_all(&config2).unwrap();
        std::fs::write(
            config2.join("plugin.json"),
            r#"{"name":"same","description":"B","version":"1.0.0"}"#,
        )
        .unwrap();

        let result =
            PluginLoader::load_from_dirs(&[dir1.path().to_path_buf(), dir2.path().to_path_buf()])
                .await;

        assert!(
            matches!(result, Err(PluginError::DuplicateName { ref name, .. }) if name == "same")
        );
    }

    #[tokio::test]
    async fn test_register_skills() {
        let dir = tempdir().unwrap();
        create_full_plugin(dir.path(), "my-plugin");

        let manager = PluginLoader::load_from_dirs(&[dir.path().to_path_buf()])
            .await
            .unwrap();

        let mut registry = IndexRegistry::new();
        manager.register_skills(&mut registry);

        assert_eq!(registry.len(), 1);
        assert!(registry.get("my-plugin:test-skill").is_some());
    }

    #[tokio::test]
    async fn test_register_subagents() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("agent-plugin");
        let config_dir = plugin_dir.join(".claude-plugin");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("plugin.json"),
            r#"{"name":"agent-plugin","description":"Has agents","version":"1.0.0"}"#,
        )
        .unwrap();

        let agents_dir = plugin_dir.join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("reviewer.md"),
            "---\nname: reviewer\ndescription: Code reviewer\n---\nReview prompt",
        )
        .unwrap();

        let manager = PluginLoader::load_from_dirs(&[dir.path().to_path_buf()])
            .await
            .unwrap();

        let mut registry = IndexRegistry::new();
        manager.register_subagents(&mut registry);

        assert_eq!(registry.len(), 1);
        assert!(registry.get("agent-plugin:reviewer").is_some());
    }

    #[tokio::test]
    async fn test_mcp_servers_aggregation() {
        let dir = tempdir().unwrap();
        let plugin_dir = dir.path().join("mcp-plugin");
        let config_dir = plugin_dir.join(".claude-plugin");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("plugin.json"),
            r#"{"name":"mcp-plugin","description":"Has MCP","version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join(".mcp.json"),
            r#"{"mcpServers":{"ctx":{"type":"stdio","command":"npx","args":["@ctx/mcp"]}}}"#,
        )
        .unwrap();

        let manager = PluginLoader::load_from_dirs(&[dir.path().to_path_buf()])
            .await
            .unwrap();

        let servers = manager.mcp_servers();
        assert_eq!(servers.len(), 1);
        assert!(servers.contains_key("mcp-plugin:ctx"));
    }

    #[tokio::test]
    async fn test_empty_dirs() {
        let manager = PluginLoader::load_from_dirs(&[]).await.unwrap();
        assert_eq!(manager.plugin_count(), 0);
    }

    #[tokio::test]
    async fn test_plugins_accessor() {
        let dir = tempdir().unwrap();
        create_full_plugin(dir.path(), "accessible");

        let manager = PluginLoader::load_from_dirs(&[dir.path().to_path_buf()])
            .await
            .unwrap();

        assert_eq!(manager.plugins().len(), 1);
        assert_eq!(manager.plugins()[0].name(), "accessible");
    }
}
