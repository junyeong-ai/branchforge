//! Claude CLI workspace settings loader with hierarchical merging.
//!
//! Loads settings from (lowest to highest priority):
//! 1. User settings: ~/.claude/settings.json
//! 2. Project settings: .claude/settings.json
//! 3. Local settings: .claude/settings.local.json (not committed)
//! 4. Managed settings: organization policy (locked, cannot be overridden)

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::ConfigResult;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsSource {
    Builtin,
    #[default]
    User,
    Project,
    Local,
    Managed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(skip)]
    pub source: SettingsSource,

    #[serde(default)]
    pub env: HashMap<String, String>,

    #[serde(default)]
    pub authorization: AuthorizationSettings,

    #[serde(default)]
    pub sandbox: SandboxSettings,

    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: HashMap<String, serde_json::Value>,

    #[serde(default)]
    pub model: Option<String>,

    #[serde(default, rename = "smallModel")]
    pub small_model: Option<String>,

    #[serde(default, rename = "maxTokens")]
    pub max_tokens: Option<u32>,

    #[serde(default)]
    pub hooks: Option<HooksSettings>,

    #[serde(default, rename = "outputStyle")]
    pub output_style: Option<String>,

    #[serde(default, rename = "awsAuthRefresh")]
    pub aws_auth_refresh: Option<String>,

    #[serde(default, rename = "awsCredentialExport")]
    pub aws_credential_export: Option<String>,

    #[serde(default, rename = "apiKeyHelper")]
    pub api_key_helper: Option<String>,

    #[serde(default, rename = "toolSearch")]
    pub tool_search: ToolSearchSettings,

    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl Settings {
    pub fn source(mut self, source: SettingsSource) -> Self {
        self.source = source;
        self
    }

    pub fn is_managed(&self) -> bool {
        self.source == SettingsSource::Managed
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HooksSettings {
    #[serde(default, rename = "PreToolUse")]
    pub pre_tool_use: HashMap<String, HookConfig>,

    #[serde(default, rename = "PostToolUse")]
    pub post_tool_use: HashMap<String, HookConfig>,

    #[serde(default, rename = "SessionStart")]
    pub session_start: Vec<HookConfig>,

    #[serde(default, rename = "SessionEnd")]
    pub session_end: Vec<HookConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HookConfig {
    Command(String),
    Full {
        command: String,
        #[serde(default)]
        timeout_secs: Option<u64>,
        #[serde(default)]
        matcher: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthorizationSettings {
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default, rename = "defaultMode")]
    pub default_mode: Option<String>,
}

impl AuthorizationSettings {
    pub fn to_policy(&self) -> crate::authorization::ToolPolicy {
        use crate::authorization::ToolPolicy;

        let mut builder = ToolPolicy::builder();

        for pattern in &self.deny {
            builder = builder.deny(pattern);
        }

        for pattern in &self.allow {
            builder = builder.allow(pattern);
        }

        builder.build()
    }

    pub fn is_empty(&self) -> bool {
        self.deny.is_empty() && self.allow.is_empty() && self.default_mode.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxSettings {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub network: NetworkSandboxSettings,

    #[serde(default, rename = "excludedCommands")]
    pub excluded_commands: Vec<String>,

    #[serde(default, rename = "allowUnsandboxedCommands")]
    pub allow_unsandboxed_commands: bool,

    #[serde(default, rename = "autoAllowBashIfSandboxed")]
    pub auto_allow_bash_if_sandboxed: Option<bool>,
}

impl SandboxSettings {
    /// Convert settings to SandboxConfig for use with SecurityContext.
    ///
    /// # Default Behaviors
    /// - `auto_allow_bash_if_sandboxed`: defaults to `true`
    /// - `enable_weaker_nested_sandbox`: defaults to `false` (strict mode)
    /// - `allowed_paths` and `denied_paths`: empty by default (use working_dir as root)
    pub fn to_sandbox_config(
        &self,
        working_dir: std::path::PathBuf,
    ) -> crate::security::sandbox::SandboxConfig {
        use crate::security::sandbox::{NetworkConfig, SandboxConfig};

        SandboxConfig {
            enabled: self.enabled,
            auto_allow_bash_if_sandboxed: self.auto_allow_bash_if_sandboxed.unwrap_or(true),
            excluded_commands: self.excluded_commands.iter().cloned().collect(),
            allow_unsandboxed_commands: self.allow_unsandboxed_commands,
            network: NetworkConfig {
                http_proxy_port: self.network.http_proxy_port,
                socks_proxy_port: self.network.socks_proxy_port,
                allow_unix_sockets: Vec::new(),
                allow_local_binding: false,
            },
            working_dir,
            allowed_domains: self.network.allowed_domains.clone(),
            blocked_domains: self.network.blocked_domains.clone(),
            // Explicit defaults for clarity
            enable_weaker_nested_sandbox: false,
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn has_network_settings(&self) -> bool {
        !self.network.allowed_domains.is_empty() || !self.network.blocked_domains.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkSandboxSettings {
    #[serde(default, rename = "allowedDomains")]
    pub allowed_domains: HashSet<String>,

    #[serde(default, rename = "blockedDomains")]
    pub blocked_domains: HashSet<String>,

    #[serde(default, rename = "httpProxyPort")]
    pub http_proxy_port: Option<u16>,

    #[serde(default, rename = "socksProxyPort")]
    pub socks_proxy_port: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolSearchSettings {
    #[serde(default)]
    pub enabled: Option<bool>,

    #[serde(default)]
    pub threshold: Option<f64>,

    #[serde(default)]
    pub mode: Option<String>,

    #[serde(default, rename = "maxResults")]
    pub max_results: Option<usize>,

    #[serde(default, rename = "alwaysLoad")]
    pub always_load: Option<Vec<String>>,
}

impl ToolSearchSettings {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn to_config(&self, context_window: usize) -> crate::tools::ToolSearchConfig {
        use crate::tools::{SearchMode, ToolSearchConfig};

        let mut config = ToolSearchConfig::default().context_window(context_window);

        if let Some(threshold) = self.threshold {
            config = config.threshold(threshold);
        }

        if let Some(ref mode) = self.mode {
            let search_mode = match mode.to_lowercase().as_str() {
                "bm25" => SearchMode::Bm25,
                _ => SearchMode::Regex,
            };
            config = config.search_mode(search_mode);
        }

        if let Some(max_results) = self.max_results {
            config.max_results = max_results;
        }

        if let Some(ref always_load) = self.always_load {
            config = config.always_load(always_load.clone());
        }

        config
    }

    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.threshold.is_none()
            && self.mode.is_none()
            && self.max_results.is_none()
            && self.always_load.is_none()
    }
}

/// Settings loader that merges from multiple sources.
#[derive(Debug, Default)]
pub struct SettingsLoader {
    settings: Settings,
    locked_keys: HashSet<String>,
}

impl SettingsLoader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads settings from all levels (enterprise + user + project + local).
    /// Priority (lowest to highest): Enterprise → User → Project → Local.
    /// Enterprise settings lock keys and cannot be overridden by lower levels.
    pub async fn load(&mut self, project_dir: &Path) -> ConfigResult<&Settings> {
        // 1. Enterprise settings (lowest priority, but locks keys)
        if let Some(enterprise_path) = crate::context::enterprise_base_path() {
            self.load_enterprise(&enterprise_path).await?;
        }

        // 2. User settings
        if let Some(home) = crate::common::home_dir() {
            self.load_from_dir(&home.join(".claude"), SettingsSource::User)
                .await?;
        }

        // 3. Project settings
        self.load_from_dir(&project_dir.join(".claude"), SettingsSource::Project)
            .await?;

        // 4. Local settings (highest priority)
        let local_settings = project_dir.join(".claude").join("settings.local.json");
        if local_settings.exists() {
            self.merge_file(&local_settings, SettingsSource::Local)
                .await?;
        }

        Ok(&self.settings)
    }

    /// Loads settings from a single base directory.
    /// Use this for loading from a specific resource level.
    pub async fn load_from(&mut self, base_dir: &Path) -> ConfigResult<&Settings> {
        // Check for settings.json directly in base_dir
        let direct_path = base_dir.join("settings.json");
        if direct_path.exists() {
            self.merge_file(&direct_path, SettingsSource::Project)
                .await?;
        } else {
            // Check for .claude/settings.json
            self.load_from_dir(&base_dir.join(".claude"), SettingsSource::Project)
                .await?;
        }
        Ok(&self.settings)
    }

    /// Loads only local settings (settings.local.json).
    pub async fn load_local(&mut self, project_dir: &Path) -> ConfigResult<&Settings> {
        let local_path = project_dir.join(".claude").join("settings.local.json");
        if local_path.exists() {
            self.merge_file(&local_path, SettingsSource::Local).await?;
        }
        Ok(&self.settings)
    }

    /// Internal: Load enterprise settings with key locking.
    async fn load_enterprise(&mut self, enterprise_dir: &Path) -> ConfigResult<()> {
        let settings_path = enterprise_dir.join("settings.json");
        if settings_path.exists() {
            let content = tokio::fs::read_to_string(&settings_path).await?;
            let managed: Settings = serde_json::from_str(&content)?;

            // Lock non-empty fields from enterprise settings
            if !managed.authorization.deny.is_empty() {
                self.locked_keys.insert("authorization.deny".to_string());
            }
            if !managed.authorization.allow.is_empty() {
                self.locked_keys.insert("authorization.allow".to_string());
            }
            if managed.authorization.default_mode.is_some() {
                self.locked_keys
                    .insert("authorization.defaultMode".to_string());
            }
            if managed.model.is_some() {
                self.locked_keys.insert("model".to_string());
            }

            self.merge_settings(managed, true);
        }
        Ok(())
    }

    /// Internal: Load from a .claude directory.
    async fn load_from_dir(
        &mut self,
        claude_dir: &Path,
        source: SettingsSource,
    ) -> ConfigResult<()> {
        let settings_path = claude_dir.join("settings.json");
        if settings_path.exists() {
            self.merge_file(&settings_path, source).await?;
        }
        Ok(())
    }

    async fn merge_file(&mut self, path: &PathBuf, source: SettingsSource) -> ConfigResult<()> {
        let content = tokio::fs::read_to_string(path).await?;
        let mut file_settings: Settings = serde_json::from_str(&content)?;
        file_settings.source = source;
        self.merge_settings(file_settings, false);
        Ok(())
    }

    fn merge_settings(&mut self, other: Settings, is_managed: bool) {
        if is_managed {
            if !other.authorization.deny.is_empty() {
                self.locked_keys.insert("authorization.deny".to_string());
            }
            if !other.authorization.allow.is_empty() {
                self.locked_keys.insert("authorization.allow".to_string());
            }
            if other.authorization.default_mode.is_some() {
                self.locked_keys
                    .insert("authorization.defaultMode".to_string());
            }
            if other.model.is_some() {
                self.locked_keys.insert("model".to_string());
            }
        }

        self.settings.env.extend(other.env);

        if !self.locked_keys.contains("authorization.deny") || is_managed {
            self.settings
                .authorization
                .deny
                .extend(other.authorization.deny);
        }
        if !self.locked_keys.contains("authorization.allow") || is_managed {
            self.settings
                .authorization
                .allow
                .extend(other.authorization.allow);
        }
        if (!self.locked_keys.contains("authorization.defaultMode") || is_managed)
            && other.authorization.default_mode.is_some()
        {
            self.settings.authorization.default_mode = other.authorization.default_mode;
        }

        self.settings
            .sandbox
            .network
            .allowed_domains
            .extend(other.sandbox.network.allowed_domains);
        self.settings
            .sandbox
            .network
            .blocked_domains
            .extend(other.sandbox.network.blocked_domains);
        self.settings
            .sandbox
            .excluded_commands
            .extend(other.sandbox.excluded_commands);

        if other.sandbox.enabled {
            self.settings.sandbox.enabled = true;
        }
        if other.sandbox.allow_unsandboxed_commands {
            self.settings.sandbox.allow_unsandboxed_commands = true;
        }
        if other.sandbox.auto_allow_bash_if_sandboxed.is_some() {
            self.settings.sandbox.auto_allow_bash_if_sandboxed =
                other.sandbox.auto_allow_bash_if_sandboxed;
        }
        if let Some(port) = other.sandbox.network.http_proxy_port {
            self.settings.sandbox.network.http_proxy_port = Some(port);
        }
        if let Some(port) = other.sandbox.network.socks_proxy_port {
            self.settings.sandbox.network.socks_proxy_port = Some(port);
        }

        self.settings.mcp_servers.extend(other.mcp_servers);

        if other.aws_auth_refresh.is_some() {
            self.settings.aws_auth_refresh = other.aws_auth_refresh;
        }
        if other.aws_credential_export.is_some() {
            self.settings.aws_credential_export = other.aws_credential_export;
        }
        if other.api_key_helper.is_some() {
            self.settings.api_key_helper = other.api_key_helper;
        }

        self.settings.extra.extend(other.extra);

        if (!self.locked_keys.contains("model") || is_managed) && other.model.is_some() {
            self.settings.model = other.model;
        }
        if other.small_model.is_some() {
            self.settings.small_model = other.small_model;
        }
        if other.max_tokens.is_some() {
            self.settings.max_tokens = other.max_tokens;
        }
        if let Some(other_hooks) = other.hooks {
            match &mut self.settings.hooks {
                Some(existing) => {
                    existing.pre_tool_use.extend(other_hooks.pre_tool_use);
                    existing.post_tool_use.extend(other_hooks.post_tool_use);
                    existing.session_start.extend(other_hooks.session_start);
                    existing.session_end.extend(other_hooks.session_end);
                }
                None => self.settings.hooks = Some(other_hooks),
            }
        }
        if other.output_style.is_some() {
            self.settings.output_style = other.output_style;
        }

        // Merge tool_search settings (later settings override earlier)
        if other.tool_search.enabled.is_some() {
            self.settings.tool_search.enabled = other.tool_search.enabled;
        }
        if other.tool_search.threshold.is_some() {
            self.settings.tool_search.threshold = other.tool_search.threshold;
        }
        if other.tool_search.mode.is_some() {
            self.settings.tool_search.mode = other.tool_search.mode;
        }
        if other.tool_search.max_results.is_some() {
            self.settings.tool_search.max_results = other.tool_search.max_results;
        }
        if let Some(always_load) = other.tool_search.always_load {
            match &mut self.settings.tool_search.always_load {
                Some(existing) => existing.extend(always_load),
                None => self.settings.tool_search.always_load = Some(always_load),
            }
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn into_settings(self) -> Settings {
        self.settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_settings_loader() {
        let loader = SettingsLoader::new();
        assert!(loader.settings.env.is_empty());
    }

    #[test]
    fn test_authorization_settings_to_policy() {
        let settings = AuthorizationSettings {
            deny: vec!["Bash(rm:*)".to_string()],
            allow: vec!["Bash(git:*)".to_string()],
            default_mode: Some("auto".to_string()),
        };

        let policy = settings.to_policy();
        assert_eq!(policy.rules.len(), 2);
    }

    #[test]
    fn test_authorization_settings_is_empty() {
        let empty = AuthorizationSettings::default();
        assert!(empty.is_empty());

        let with_deny = AuthorizationSettings {
            deny: vec!["Bash".to_string()],
            ..Default::default()
        };
        assert!(!with_deny.is_empty());
    }

    #[test]
    fn test_sandbox_settings_enabled() {
        let settings = SandboxSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(settings.is_enabled());

        let disabled = SandboxSettings::default();
        assert!(!disabled.is_enabled());
    }

    #[test]
    fn test_sandbox_settings_to_sandbox_config() {
        use std::path::PathBuf;

        let settings = SandboxSettings {
            enabled: true,
            network: NetworkSandboxSettings {
                allowed_domains: ["example.com".to_string()].into_iter().collect(),
                blocked_domains: ["malware.com".to_string()].into_iter().collect(),
                ..Default::default()
            },
            ..Default::default()
        };

        let config = settings.to_sandbox_config(PathBuf::from("/tmp"));
        assert!(config.enabled);
        assert!(config.allowed_domains.contains("example.com"));
        assert!(config.blocked_domains.contains("malware.com"));

        let network_sandbox = config.to_network_sandbox();
        // Use check() to verify domains - allowed_domains/blocked_domains are builder methods
        use crate::security::DomainCheck;
        assert_eq!(network_sandbox.check("example.com"), DomainCheck::Allowed);
        assert_eq!(network_sandbox.check("malware.com"), DomainCheck::Blocked);
    }

    #[test]
    fn test_tool_search_settings_default() {
        let settings = ToolSearchSettings::default();
        assert!(settings.is_empty());
        assert!(settings.is_enabled()); // Default is enabled
    }

    #[test]
    fn test_tool_search_settings_to_config() {
        use crate::tools::SearchMode;

        let settings = ToolSearchSettings {
            enabled: Some(true),
            threshold: Some(0.15),
            mode: Some("bm25".to_string()),
            max_results: Some(10),
            always_load: Some(vec!["mcp__my__tool".to_string()]),
        };

        let config = settings.to_config(200_000);
        assert_eq!(config.threshold, 0.15);
        assert_eq!(config.search_mode, SearchMode::Bm25);
        assert_eq!(config.max_results, 10);
        assert!(config.always_load.contains(&"mcp__my__tool".to_string()));
    }

    #[test]
    fn test_tool_search_settings_regex_mode() {
        use crate::tools::SearchMode;

        let settings = ToolSearchSettings {
            mode: Some("regex".to_string()),
            ..Default::default()
        };

        let config = settings.to_config(100_000);
        assert_eq!(config.search_mode, SearchMode::Regex);
        assert_eq!(config.context_window, 100_000);
    }

    #[test]
    fn test_tool_search_settings_disabled() {
        let settings = ToolSearchSettings {
            enabled: Some(false),
            ..Default::default()
        };

        assert!(!settings.is_enabled());
        assert!(!settings.is_empty());
    }

    #[test]
    fn test_managed_default_mode_is_locked() {
        let mut loader = SettingsLoader::new();

        loader.merge_settings(
            Settings {
                authorization: AuthorizationSettings {
                    default_mode: Some("readOnly".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            true,
        );

        loader.merge_settings(
            Settings {
                authorization: AuthorizationSettings {
                    default_mode: Some("allowAll".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            false,
        );

        assert_eq!(
            loader.settings.authorization.default_mode.as_deref(),
            Some("readOnly")
        );
    }
}
