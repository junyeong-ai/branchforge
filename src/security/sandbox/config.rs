//! Sandbox configuration types matching Claude Code settings.
//!
//! Reference: <https://code.claude.com/docs/en/sandboxing>

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_auto_allow_bash")]
    pub auto_allow_bash_if_sandboxed: bool,

    #[serde(default)]
    pub excluded_commands: HashSet<String>,

    #[serde(default = "default_allow_unsandboxed")]
    pub allow_unsandboxed_commands: bool,

    #[serde(default)]
    pub network: NetworkConfig,

    #[serde(default)]
    pub enable_weaker_nested_sandbox: bool,

    #[serde(skip)]
    pub working_dir: PathBuf,

    #[serde(skip)]
    pub allowed_paths: Vec<PathBuf>,

    #[serde(skip)]
    pub denied_paths: Vec<String>,

    #[serde(default)]
    pub allowed_domains: HashSet<String>,

    #[serde(default)]
    pub blocked_domains: HashSet<String>,
}

fn default_auto_allow_bash() -> bool {
    true
}

fn default_allow_unsandboxed() -> bool {
    true
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_allow_bash_if_sandboxed: true,
            excluded_commands: HashSet::new(),
            allow_unsandboxed_commands: true,
            network: NetworkConfig::default(),
            enable_weaker_nested_sandbox: false,
            working_dir: PathBuf::new(),
            allowed_paths: Vec::new(),
            denied_paths: Vec::new(),
            allowed_domains: HashSet::new(),
            blocked_domains: HashSet::new(),
        }
    }
}

impl SandboxConfig {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            enabled: true,
            working_dir,
            ..Default::default()
        }
    }

    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = dir;
        self
    }

    pub fn auto_allow_bash(mut self, enabled: bool) -> Self {
        self.auto_allow_bash_if_sandboxed = enabled;
        self
    }

    pub fn allowed_paths(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        self.allowed_paths = paths.into_iter().collect();
        self
    }

    pub fn denied_paths(mut self, patterns: impl IntoIterator<Item = String>) -> Self {
        self.denied_paths = patterns.into_iter().collect();
        self
    }

    pub fn excluded_commands(mut self, commands: impl IntoIterator<Item = String>) -> Self {
        self.excluded_commands = commands.into_iter().collect();
        self
    }

    pub fn network(mut self, network: NetworkConfig) -> Self {
        self.network = network;
        self
    }

    pub fn allowed_domains(mut self, domains: impl IntoIterator<Item = String>) -> Self {
        self.allowed_domains = domains.into_iter().collect();
        self
    }

    pub fn blocked_domains(mut self, domains: impl IntoIterator<Item = String>) -> Self {
        self.blocked_domains = domains.into_iter().collect();
        self
    }

    pub fn allow_domain(mut self, domain: impl Into<String>) -> Self {
        self.allowed_domains.insert(domain.into());
        self
    }

    pub fn deny_domain(mut self, domain: impl Into<String>) -> Self {
        self.blocked_domains.insert(domain.into());
        self
    }

    pub fn to_network_sandbox(&self) -> super::NetworkSandbox {
        super::NetworkSandbox::new()
            .with_allowed_domains(self.allowed_domains.iter().cloned())
            .with_blocked_domains(self.blocked_domains.iter().cloned())
    }

    pub fn is_command_excluded(&self, command: &str) -> bool {
        let base_command = command.split_whitespace().next().unwrap_or(command);
        self.excluded_commands.contains(base_command)
    }

    pub fn should_auto_allow_bash(&self) -> bool {
        self.enabled && self.auto_allow_bash_if_sandboxed
    }

    pub fn can_bypass_sandbox(&self, explicitly_requested: bool) -> bool {
        explicitly_requested && self.allow_unsandboxed_commands
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkConfig {
    #[serde(default)]
    pub allow_unix_sockets: Vec<String>,

    #[serde(default)]
    pub allow_local_binding: bool,

    #[serde(default)]
    pub http_proxy_port: Option<u16>,

    #[serde(default)]
    pub socks_proxy_port: Option<u16>,
}

impl NetworkConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn proxy(http_port: Option<u16>, socks_port: Option<u16>) -> Self {
        Self {
            http_proxy_port: http_port,
            socks_proxy_port: socks_port,
            ..Default::default()
        }
    }

    pub fn unix_sockets(mut self, paths: impl IntoIterator<Item = String>) -> Self {
        self.allow_unix_sockets = paths.into_iter().collect();
        self
    }

    pub fn local_binding(mut self, allow: bool) -> Self {
        self.allow_local_binding = allow;
        self
    }

    pub fn has_proxy(&self) -> bool {
        self.http_proxy_port.is_some() || self.socks_proxy_port.is_some()
    }

    pub fn http_proxy_url(&self) -> Option<String> {
        self.http_proxy_port
            .map(|port| format!("http://127.0.0.1:{}", port))
    }

    pub fn socks_proxy_url(&self) -> Option<String> {
        self.socks_proxy_port
            .map(|port| format!("socks5://127.0.0.1:{}", port))
    }

    pub fn no_proxy_value(&self) -> String {
        "localhost,127.0.0.1,::1".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert!(!config.enabled);
        assert!(config.auto_allow_bash_if_sandboxed);
        assert!(config.allow_unsandboxed_commands);
        assert!(!config.enable_weaker_nested_sandbox);
    }

    #[test]
    fn test_sandbox_config_enabled() {
        let config = SandboxConfig::new(PathBuf::from("/tmp/sandbox"));
        assert!(config.enabled);
        assert!(config.should_auto_allow_bash());
    }

    #[test]
    fn test_excluded_commands() {
        let config =
            SandboxConfig::disabled().excluded_commands(vec!["docker".into(), "git".into()]);

        assert!(config.is_command_excluded("docker"));
        assert!(config.is_command_excluded("docker run nginx"));
        assert!(config.is_command_excluded("git"));
        assert!(config.is_command_excluded("git status"));
        assert!(!config.is_command_excluded("ls"));
    }

    #[test]
    fn test_bypass_sandbox() {
        let config = SandboxConfig::new(PathBuf::from("/tmp"));
        assert!(config.can_bypass_sandbox(true));
        assert!(!config.can_bypass_sandbox(false));

        let strict_config = SandboxConfig::new(PathBuf::from("/tmp"));
        let strict_config = SandboxConfig {
            allow_unsandboxed_commands: false,
            ..strict_config
        };
        assert!(!strict_config.can_bypass_sandbox(true));
    }

    #[test]
    fn test_network_config() {
        let network = NetworkConfig::proxy(Some(8080), Some(1080));
        assert!(network.has_proxy());
        assert_eq!(
            network.http_proxy_url(),
            Some("http://127.0.0.1:8080".into())
        );
        assert_eq!(
            network.socks_proxy_url(),
            Some("socks5://127.0.0.1:1080".into())
        );
    }

    #[test]
    fn test_unix_sockets() {
        let network = NetworkConfig::new().unix_sockets(vec!["~/.ssh/agent-socket".into()]);
        assert_eq!(network.allow_unix_sockets.len(), 1);
    }

    #[test]
    fn test_serde() {
        let json = r#"{
            "enabled": true,
            "autoAllowBashIfSandboxed": true,
            "excludedCommands": ["docker", "git"],
            "allowUnsandboxedCommands": false,
            "network": {
                "allowUnixSockets": ["~/.ssh/agent"],
                "httpProxyPort": 8080
            }
        }"#;

        let config: SandboxConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert!(config.auto_allow_bash_if_sandboxed);
        assert!(config.excluded_commands.contains("docker"));
        assert!(!config.allow_unsandboxed_commands);
        assert_eq!(config.network.http_proxy_port, Some(8080));
    }
}
