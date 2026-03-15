use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::PluginError;
use super::manifest::PluginDescriptor;
use super::namespace;
use crate::common::SourceType;
use crate::config::HookConfig;
use crate::hooks::{HookEvent, HookRule};
use crate::mcp::McpServerConfig;
use crate::skills::{SkillIndex, SkillIndexLoader};
use crate::subagents::{SubagentIndex, SubagentIndexLoader};

const PLUGIN_ROOT_VAR: &str = "${CLAUDE_PLUGIN_ROOT}";

fn resolve_plugin_root(value: &str, root: &Path) -> String {
    value.replace(PLUGIN_ROOT_VAR, &root.display().to_string())
}

fn resolve_hook_config(config: HookConfig, root: &Path) -> HookConfig {
    match config {
        HookConfig::Command(cmd) => HookConfig::Command(resolve_plugin_root(&cmd, root)),
        HookConfig::Full {
            command,
            timeout_secs,
            matcher,
        } => HookConfig::Full {
            command: resolve_plugin_root(&command, root),
            timeout_secs,
            matcher,
        },
    }
}

fn resolve_mcp_config(config: McpServerConfig, root: &Path) -> McpServerConfig {
    match config {
        McpServerConfig::Stdio {
            command,
            args,
            env,
            cwd,
        } => McpServerConfig::Stdio {
            command: resolve_plugin_root(&command, root),
            args: args
                .into_iter()
                .map(|a| resolve_plugin_root(&a, root))
                .collect(),
            env: env
                .into_iter()
                .map(|(k, v)| (k, resolve_plugin_root(&v, root)))
                .collect(),
            cwd: cwd.map(|c| resolve_plugin_root(&c, root)),
        },
        other => other,
    }
}

/// Outer wrapper for hooks.json — supports both nested and flat formats.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PluginHooksFile {
    /// Nested format: `{"hooks": {"PreToolUse": [{"matcher": "...", "hooks": [...]}]}}`
    Nested {
        hooks: HashMap<String, Vec<HookRule>>,
    },
    /// Flat format: `{"PreToolUse": ["echo pre"]}`
    Flat(HashMap<String, Vec<HookConfig>>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginHookEntry {
    pub plugin: String,
    pub event: HookEvent,
    pub config: HookConfig,
    pub plugin_root: PathBuf,
}

#[derive(Debug, Default)]
pub struct PluginResources {
    pub skills: Vec<SkillIndex>,
    pub subagents: Vec<SubagentIndex>,
    pub hooks: Vec<PluginHookEntry>,
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

pub struct PluginLoader;

impl PluginLoader {
    pub async fn load(plugin: &PluginDescriptor) -> Result<PluginResources, PluginError> {
        let plugin_name = plugin.name();
        let mut resources = PluginResources::default();

        Self::load_skills(plugin, plugin_name, &mut resources).await?;
        Self::load_commands(plugin, plugin_name, &mut resources).await?;
        Self::load_subagents(plugin, plugin_name, &mut resources).await?;
        Self::load_hooks(plugin, plugin_name, &mut resources).await?;
        Self::load_mcp(plugin, plugin_name, &mut resources).await?;

        Ok(resources)
    }

    async fn load_skills(
        plugin: &PluginDescriptor,
        plugin_name: &str,
        resources: &mut PluginResources,
    ) -> Result<(), PluginError> {
        let skills_dir = plugin.skills_dir();
        if !skills_dir.exists() {
            return Ok(());
        }

        let loader = SkillIndexLoader::new();
        let skills =
            loader
                .scan_directory(&skills_dir)
                .await
                .map_err(|e| PluginError::ResourceLoad {
                    plugin: plugin_name.to_string(),
                    message: format!("Skills: {e}"),
                })?;

        let plugin_root = plugin.root_dir();
        for mut skill in skills {
            skill.name = namespace::namespaced(plugin_name, &skill.name);
            skill.source_type = SourceType::Plugin;
            Self::collect_resource_hooks(
                &skill.hooks,
                plugin_name,
                plugin_root,
                &mut resources.hooks,
            );
            resources.skills.push(skill);
        }

        Ok(())
    }

    /// Load `commands/` directory (simple markdown files treated as skills).
    async fn load_commands(
        plugin: &PluginDescriptor,
        plugin_name: &str,
        resources: &mut PluginResources,
    ) -> Result<(), PluginError> {
        let commands_dir = plugin.commands_dir();
        if !commands_dir.exists() {
            return Ok(());
        }

        let loader = SkillIndexLoader::new();
        let mut entries =
            tokio::fs::read_dir(&commands_dir)
                .await
                .map_err(|e| PluginError::ResourceLoad {
                    plugin: plugin_name.to_string(),
                    message: format!("Commands dir: {e}"),
                })?;

        while let Some(entry) =
            entries
                .next_entry()
                .await
                .map_err(|e| PluginError::ResourceLoad {
                    plugin: plugin_name.to_string(),
                    message: format!("Commands entry: {e}"),
                })?
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str());
            if ext != Some("md") {
                continue;
            }

            let skill = match loader.load_file(&path).await {
                Ok(s) => s,
                Err(_) => continue,
            };

            let namespaced = namespace::namespaced(plugin_name, &skill.name);
            // Only add if not already loaded from skills/ (skills take precedence)
            if !resources.skills.iter().any(|s| s.name == namespaced) {
                let mut skill = skill;
                skill.name = namespaced;
                skill.source_type = SourceType::Plugin;
                resources.skills.push(skill);
            }
        }

        Ok(())
    }

    async fn load_subagents(
        plugin: &PluginDescriptor,
        plugin_name: &str,
        resources: &mut PluginResources,
    ) -> Result<(), PluginError> {
        let agents_dir = plugin.agents_dir();
        if !agents_dir.exists() {
            return Ok(());
        }

        let loader = SubagentIndexLoader::new();
        let subagents =
            loader
                .scan_directory(&agents_dir)
                .await
                .map_err(|e| PluginError::ResourceLoad {
                    plugin: plugin_name.to_string(),
                    message: format!("Agents: {e}"),
                })?;

        let plugin_root = plugin.root_dir();
        for mut subagent in subagents {
            subagent.name = namespace::namespaced(plugin_name, &subagent.name);
            subagent.mcp_servers = subagent
                .mcp_servers
                .into_iter()
                .map(|server| {
                    if namespace::is_namespaced(&server) {
                        server
                    } else {
                        namespace::namespaced(plugin_name, &server)
                    }
                })
                .collect();
            subagent.source_type = SourceType::Plugin;
            Self::collect_resource_hooks(
                &subagent.hooks,
                plugin_name,
                plugin_root,
                &mut resources.hooks,
            );
            resources.subagents.push(subagent);
        }

        Ok(())
    }

    fn collect_resource_hooks(
        hooks: &Option<HashMap<String, Vec<HookRule>>>,
        plugin_name: &str,
        plugin_root: &Path,
        out: &mut Vec<PluginHookEntry>,
    ) {
        let Some(hooks_map) = hooks else { return };
        for (event_name, rules) in hooks_map {
            let Some(event) = HookEvent::from_pascal_case(event_name) else {
                continue;
            };
            for rule in rules {
                let matcher = rule.matcher.as_deref();
                for action in &rule.hooks {
                    if let Some(config) = action
                        .to_hook_config(matcher)
                        .map(|c| resolve_hook_config(c, plugin_root))
                    {
                        out.push(PluginHookEntry {
                            plugin: plugin_name.to_string(),
                            event,
                            config,
                            plugin_root: plugin_root.to_path_buf(),
                        });
                    }
                }
            }
        }
    }

    async fn load_hooks(
        plugin: &PluginDescriptor,
        plugin_name: &str,
        resources: &mut PluginResources,
    ) -> Result<(), PluginError> {
        let hooks_path = plugin.hooks_config_path();
        if !hooks_path.exists() {
            return Ok(());
        }

        let content = tokio::fs::read_to_string(&hooks_path).await?;
        let hooks_file: PluginHooksFile =
            serde_json::from_str(&content).map_err(|e| PluginError::InvalidManifest {
                path: hooks_path,
                reason: format!("hooks config: {e}"),
            })?;

        let plugin_root = plugin.root_dir();

        match hooks_file {
            PluginHooksFile::Nested { hooks } => {
                Self::collect_resource_hooks(
                    &Some(hooks),
                    plugin_name,
                    plugin_root,
                    &mut resources.hooks,
                );
            }
            PluginHooksFile::Flat(hooks_map) => {
                for (event_name, configs) in hooks_map {
                    let Some(event) = HookEvent::from_pascal_case(&event_name) else {
                        continue;
                    };
                    for mut config in configs {
                        config = resolve_hook_config(config, plugin_root);
                        resources.hooks.push(PluginHookEntry {
                            plugin: plugin_name.to_string(),
                            event,
                            config,
                            plugin_root: plugin_root.to_path_buf(),
                        });
                    }
                }
            }
        }

        Ok(())
    }

    async fn load_mcp(
        plugin: &PluginDescriptor,
        plugin_name: &str,
        resources: &mut PluginResources,
    ) -> Result<(), PluginError> {
        let mcp_path = plugin.mcp_config_path();
        if !mcp_path.exists() {
            return Ok(());
        }

        let content = tokio::fs::read_to_string(&mcp_path).await?;

        #[derive(Deserialize)]
        struct McpConfig {
            #[serde(rename = "mcpServers", default)]
            mcp_servers: HashMap<String, McpServerConfig>,
        }

        let config: McpConfig =
            serde_json::from_str(&content).map_err(|e| PluginError::InvalidManifest {
                path: mcp_path,
                reason: format!("MCP config: {e}"),
            })?;

        let plugin_root = plugin.root_dir();
        for (name, server_config) in config.mcp_servers {
            let namespaced_name = namespace::namespaced(plugin_name, &name);
            let resolved = resolve_mcp_config(server_config, plugin_root);
            resources.mcp_servers.insert(namespaced_name, resolved);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    use crate::plugins::manifest::PluginManifest;

    fn make_descriptor(root: PathBuf, name: &str) -> PluginDescriptor {
        PluginDescriptor::new(
            PluginManifest {
                name: name.into(),
                description: "test".into(),
                version: "1.0.0".into(),
                author: None,
                homepage: None,
                repository: None,
                license: None,
                keywords: Vec::new(),
            },
            root,
        )
    }

    #[tokio::test]
    async fn test_load_skills() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("commit.skill.md"),
            "---\nname: commit\ndescription: Git commit\n---\nContent",
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "my-plugin");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        assert_eq!(resources.skills.len(), 1);
        assert_eq!(resources.skills[0].name, "my-plugin:commit");
        assert_eq!(resources.skills[0].source_type, SourceType::Plugin);
    }

    #[tokio::test]
    async fn test_load_subagents() {
        let dir = tempdir().unwrap();
        let agents_dir = dir.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("reviewer.md"),
            "---\nname: reviewer\ndescription: Code reviewer\nmcpServers:\n  - context7\n---\nPrompt",
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "my-plugin");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        assert_eq!(resources.subagents.len(), 1);
        assert_eq!(resources.subagents[0].name, "my-plugin:reviewer");
        assert_eq!(
            resources.subagents[0].mcp_servers,
            vec!["my-plugin:context7"]
        );
        assert_eq!(resources.subagents[0].source_type, SourceType::Plugin);
    }

    #[tokio::test]
    async fn test_load_hooks() {
        let dir = tempdir().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("hooks.json"),
            r#"{"PreToolUse": ["echo pre"], "SessionStart": ["echo start"]}"#,
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "my-plugin");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        assert_eq!(resources.hooks.len(), 2);
        assert!(
            resources
                .hooks
                .iter()
                .any(|h| h.event == HookEvent::PreToolUse)
        );
        assert!(
            resources
                .hooks
                .iter()
                .any(|h| h.event == HookEvent::SessionStart)
        );
    }

    #[tokio::test]
    async fn test_load_mcp() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"context7":{"type":"stdio","command":"npx","args":["@context7/mcp"]}}}"#,
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "my-plugin");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        assert_eq!(resources.mcp_servers.len(), 1);
        assert!(resources.mcp_servers.contains_key("my-plugin:context7"));
    }

    #[tokio::test]
    async fn test_load_empty_plugin() {
        let dir = tempdir().unwrap();
        let descriptor = make_descriptor(dir.path().to_path_buf(), "empty");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        assert!(resources.skills.is_empty());
        assert!(resources.subagents.is_empty());
        assert!(resources.hooks.is_empty());
        assert!(resources.mcp_servers.is_empty());
    }

    #[tokio::test]
    async fn test_namespace_applied_to_all_resources() {
        let dir = tempdir().unwrap();

        // Create skills
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("build.skill.md"),
            "---\nname: build\ndescription: Build project\n---\nBuild it",
        )
        .unwrap();

        // Create MCP
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"server1":{"type":"stdio","command":"cmd"}}}"#,
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "acme");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        assert_eq!(resources.skills[0].name, "acme:build");
        assert!(resources.mcp_servers.contains_key("acme:server1"));
    }

    #[tokio::test]
    async fn test_load_hooks_official_format() {
        let dir = tempdir().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("hooks.json"),
            r#"{
                "hooks": {
                    "PostToolUse": [
                        {
                            "matcher": "Write|Edit",
                            "hooks": [
                                {"type": "command", "command": "scripts/format.sh"}
                            ]
                        }
                    ],
                    "PreToolUse": [
                        {
                            "hooks": [
                                {"type": "command", "command": "scripts/check.sh", "timeout": 10}
                            ]
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "my-plugin");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        assert_eq!(resources.hooks.len(), 2);

        let post = resources
            .hooks
            .iter()
            .find(|h| h.event == HookEvent::PostToolUse)
            .unwrap();
        match &post.config {
            HookConfig::Full {
                command, matcher, ..
            } => {
                assert_eq!(command, "scripts/format.sh");
                assert_eq!(matcher.as_deref(), Some("Write|Edit"));
            }
            _ => panic!("Expected Full config"),
        }

        let pre = resources
            .hooks
            .iter()
            .find(|h| h.event == HookEvent::PreToolUse)
            .unwrap();
        match &pre.config {
            HookConfig::Full {
                command,
                timeout_secs,
                ..
            } => {
                assert_eq!(command, "scripts/check.sh");
                assert_eq!(*timeout_secs, Some(10));
            }
            _ => panic!("Expected Full config with timeout"),
        }
    }

    #[tokio::test]
    async fn test_load_commands() {
        let dir = tempdir().unwrap();
        let commands_dir = dir.path().join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(
            commands_dir.join("hello.md"),
            "---\nname: hello\ndescription: Greet user\n---\nHello!",
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "my-plugin");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        assert_eq!(resources.skills.len(), 1);
        assert_eq!(resources.skills[0].name, "my-plugin:hello");
        assert_eq!(resources.skills[0].source_type, SourceType::Plugin);
    }

    #[tokio::test]
    async fn test_skills_take_precedence_over_commands() {
        let dir = tempdir().unwrap();

        // Create skill with name "deploy"
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("deploy.skill.md"),
            "---\nname: deploy\ndescription: Deploy (skill)\n---\nSkill content",
        )
        .unwrap();

        // Create command with same name "deploy"
        let commands_dir = dir.path().join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(
            commands_dir.join("deploy.md"),
            "---\nname: deploy\ndescription: Deploy (command)\n---\nCommand content",
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "my-plugin");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        // Only one skill should exist — skills/ takes precedence
        assert_eq!(resources.skills.len(), 1);
        assert_eq!(resources.skills[0].name, "my-plugin:deploy");
        assert_eq!(resources.skills[0].description, "Deploy (skill)");
    }

    #[test]
    fn test_resolve_plugin_root() {
        let root = std::path::Path::new("/plugins/my-plugin");
        assert_eq!(
            super::resolve_plugin_root("${CLAUDE_PLUGIN_ROOT}/scripts/check.sh", root),
            "/plugins/my-plugin/scripts/check.sh"
        );
        assert_eq!(super::resolve_plugin_root("echo hello", root), "echo hello");
        assert_eq!(
            super::resolve_plugin_root("${CLAUDE_PLUGIN_ROOT}/a ${CLAUDE_PLUGIN_ROOT}/b", root),
            "/plugins/my-plugin/a /plugins/my-plugin/b"
        );
    }

    #[tokio::test]
    async fn test_hooks_plugin_root_substitution() {
        let dir = tempdir().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("hooks.json"),
            r#"{"PreToolUse": ["${CLAUDE_PLUGIN_ROOT}/scripts/check.sh"]}"#,
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "my-plugin");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        assert_eq!(resources.hooks.len(), 1);
        let expected_cmd = format!("{}/scripts/check.sh", dir.path().display());
        match &resources.hooks[0].config {
            HookConfig::Command(cmd) => assert_eq!(cmd, &expected_cmd),
            _ => panic!("Expected Command config"),
        }
        assert_eq!(resources.hooks[0].plugin_root, dir.path());
    }

    #[tokio::test]
    async fn test_mcp_plugin_root_substitution() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"srv":{"type":"stdio","command":"${CLAUDE_PLUGIN_ROOT}/bin/server","args":["--config","${CLAUDE_PLUGIN_ROOT}/config.json"],"env":{"DB_PATH":"${CLAUDE_PLUGIN_ROOT}/data"}}}}"#,
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "my-plugin");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        let config = resources.mcp_servers.get("my-plugin:srv").unwrap();
        match config {
            McpServerConfig::Stdio {
                command, args, env, ..
            } => {
                let root = dir.path().display().to_string();
                assert_eq!(command, &format!("{root}/bin/server"));
                assert_eq!(args[1], format!("{root}/config.json"));
                assert_eq!(env.get("DB_PATH").unwrap(), &format!("{root}/data"));
            }
            _ => panic!("Expected Stdio config"),
        }
    }

    #[tokio::test]
    async fn test_hooks_official_format_plugin_root_substitution() {
        let dir = tempdir().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("hooks.json"),
            r#"{
                "hooks": {
                    "PostToolUse": [{
                        "matcher": "Write",
                        "hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/fmt.sh"}]
                    }]
                }
            }"#,
        )
        .unwrap();

        let descriptor = make_descriptor(dir.path().to_path_buf(), "my-plugin");
        let resources = PluginLoader::load(&descriptor).await.unwrap();

        assert_eq!(resources.hooks.len(), 1);
        match &resources.hooks[0].config {
            HookConfig::Full { command, .. } => {
                assert_eq!(command, &format!("{}/fmt.sh", dir.path().display()));
            }
            _ => panic!("Expected Full config"),
        }
    }
}
