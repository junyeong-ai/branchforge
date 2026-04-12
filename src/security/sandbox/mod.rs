//! OS-level sandboxing for secure command execution.
//!
//! Provides filesystem and network isolation using:
//! - Linux: Landlock LSM (5.13+)
//! - macOS: Seatbelt (sandbox-exec)
//!
//! Reference: <https://code.claude.com/docs/en/sandboxing>

mod config;
mod detect;
mod error;

#[cfg(target_os = "linux")]
mod landlock;
#[cfg(target_os = "macos")]
mod macos;

pub use config::{NetworkConfig, SandboxConfig};
pub use detect::{ContainerRuntime, detect_container, is_container};
pub use error::{SandboxError, SandboxResult};
// `NetworkSandbox` was relocated to `crate::network_sandbox` during the Layer 1
// / Layer 2a split. It is domain-whitelist HTTP egress control with no
// filesystem or shell coupling (verified V5 audit), so it belongs in pure
// core, not behind the `local-fs` feature. Re-exported here only so callers
// inside the security subtree that still reach it through the sandbox path
// keep compiling during migration; the canonical path is
// `crate::network_sandbox::{NetworkSandbox, DomainCheck}`.
pub use crate::network_sandbox::{DomainCheck, NetworkSandbox};

use std::collections::HashMap;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use tracing::{info, warn};

pub trait SandboxRuntime: Send + Sync {
    fn is_available(&self) -> bool;
    fn apply(&self) -> SandboxResult<()>;
    fn wrap_command(&self, command: &str) -> SandboxResult<String>;
    fn environment_vars(&self) -> HashMap<String, String>;
}

pub struct Sandbox {
    config: SandboxConfig,
    runtime: Option<Box<dyn SandboxRuntime>>,
    /// Set to the detected container runtime when sandbox construction
    /// short-circuits because the host already provides isolation
    /// (Docker, Podman, LXC, Kubernetes). `apply()` then becomes a
    /// no-op instead of erroring out on missing runtime.
    skipped_for_container: Option<ContainerRuntime>,
}

impl Sandbox {
    pub fn new(config: SandboxConfig) -> Self {
        let skipped_for_container = Self::detect_container_skip(&config);
        let runtime = if skipped_for_container.is_some() {
            None
        } else {
            Self::create_runtime(&config)
        };
        Self {
            config,
            runtime,
            skipped_for_container,
        }
    }

    pub fn disabled() -> Self {
        Self {
            config: SandboxConfig::disabled(),
            runtime: None,
            skipped_for_container: None,
        }
    }

    /// If sandbox is requested but the process is already running
    /// inside a container, return the detected runtime so `Sandbox::new`
    /// can skip nested namespace isolation.
    ///
    /// Container runtimes (Docker, Podman, LXC, Kubernetes) already
    /// apply their own seccomp + capability + namespace isolation.
    /// Stacking another Linux-namespace sandbox on top is both
    /// redundant and likely to fail — most container seccomp profiles
    /// deny nested `unshare()`.
    fn detect_container_skip(config: &SandboxConfig) -> Option<ContainerRuntime> {
        if !config.enabled {
            return None;
        }
        let runtime = detect::detect_container()?;
        info!(
            target: "branchforge::security::sandbox",
            container_runtime = runtime.as_str(),
            "Running inside a container — skipping nested namespace sandbox; \
             host isolation already applies"
        );
        Some(runtime)
    }

    fn create_runtime(config: &SandboxConfig) -> Option<Box<dyn SandboxRuntime>> {
        if !config.enabled {
            return None;
        }

        #[cfg(target_os = "linux")]
        {
            let sandbox = landlock::LandlockSandbox::new(config.clone());
            if sandbox.is_available() {
                return Some(Box::new(sandbox));
            }
            warn!(
                "Sandbox requested but Landlock not available (requires Linux 5.13+). \
                 Commands will execute without filesystem isolation."
            );
        }

        #[cfg(target_os = "macos")]
        {
            let sandbox = macos::SeatbeltSandbox::new(config);
            if sandbox.is_available() {
                return Some(Box::new(sandbox));
            }
            warn!(
                "Sandbox requested but Seatbelt not available. \
                 Commands will execute without filesystem isolation."
            );
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        warn!(
            "Sandbox requested but no sandbox implementation available for this platform. \
             Commands will execute without filesystem isolation."
        );

        None
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled && self.runtime.is_some()
    }

    pub fn is_available(&self) -> bool {
        self.runtime.as_ref().is_some_and(|r| r.is_available())
    }

    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Returns the container runtime detected at construction time, if
    /// any. Callers that need to branch on "host-managed isolation"
    /// semantics read this.
    pub fn skipped_for_container(&self) -> Option<ContainerRuntime> {
        self.skipped_for_container
    }

    pub fn apply(&self) -> SandboxResult<()> {
        match &self.runtime {
            Some(runtime) => runtime.apply(),
            None if self.skipped_for_container.is_some() => {
                // Host container runtime provides isolation — no-op.
                Ok(())
            }
            None if self.config.enabled => Err(SandboxError::NotAvailable(
                "no sandbox runtime available".into(),
            )),
            None => Ok(()),
        }
    }

    pub fn wrap_command(&self, command: &str) -> SandboxResult<String> {
        if self.config.is_command_excluded(command) {
            if self.config.allow_unsandboxed_commands {
                return Ok(command.to_string());
            }
            return Err(SandboxError::InvalidConfig(format!(
                "command '{}' is excluded but unsandboxed commands not allowed",
                command.split_whitespace().next().unwrap_or(command)
            )));
        }

        match &self.runtime {
            Some(runtime) => runtime.wrap_command(command),
            None => Ok(command.to_string()),
        }
    }

    pub fn environment_vars(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();

        if let Some(runtime) = &self.runtime {
            env.extend(runtime.environment_vars());
        }

        let network = &self.config.network;
        if network.has_proxy() {
            if let Some(url) = network.http_proxy_url() {
                env.insert("HTTP_PROXY".into(), url.clone());
                env.insert("HTTPS_PROXY".into(), url.clone());
                env.insert("http_proxy".into(), url.clone());
                env.insert("https_proxy".into(), url);
            }
            if let Some(url) = network.socks_proxy_url() {
                env.insert("ALL_PROXY".into(), url.clone());
                env.insert("all_proxy".into(), url);
            }
            let no_proxy = network.no_proxy_value();
            env.insert("NO_PROXY".into(), no_proxy.clone());
            env.insert("no_proxy".into(), no_proxy);
        }

        env
    }

    pub fn should_auto_allow_bash(&self) -> bool {
        self.is_enabled() && self.config.should_auto_allow_bash()
    }

    pub fn can_bypass(&self, explicitly_requested: bool) -> bool {
        self.config.can_bypass_sandbox(explicitly_requested)
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::disabled()
    }
}

pub fn is_sandbox_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        landlock::is_landlock_supported()
    }
    #[cfg(target_os = "macos")]
    {
        macos::is_seatbelt_supported()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

pub fn create_sandbox(working_dir: &Path, auto_allow_bash: bool) -> Sandbox {
    let config = SandboxConfig::new(working_dir.to_path_buf()).auto_allow_bash(auto_allow_bash);
    Sandbox::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_sandbox() {
        let sandbox = Sandbox::disabled();
        assert!(!sandbox.is_enabled());
        assert!(sandbox.apply().is_ok());
    }

    #[test]
    fn test_wrap_command_disabled() {
        let sandbox = Sandbox::disabled();
        let wrapped = sandbox.wrap_command("echo test").unwrap();
        assert_eq!(wrapped, "echo test");
    }

    #[test]
    fn test_excluded_command() {
        let config =
            SandboxConfig::new(PathBuf::from("/tmp")).excluded_commands(vec!["docker".into()]);
        let sandbox = Sandbox::new(config);

        let result = sandbox.wrap_command("docker run nginx");
        assert!(result.is_err() || result.unwrap() == "docker run nginx");
    }

    #[test]
    fn test_proxy_environment() {
        let config =
            SandboxConfig::disabled().network(NetworkConfig::proxy(Some(8080), Some(1080)));
        let sandbox = Sandbox::new(config);

        let env = sandbox.environment_vars();
        assert_eq!(env.get("HTTP_PROXY"), Some(&"http://127.0.0.1:8080".into()));
        assert_eq!(
            env.get("ALL_PROXY"),
            Some(&"socks5://127.0.0.1:1080".into())
        );
    }

    #[test]
    fn test_auto_allow_bash() {
        let config = SandboxConfig::new(PathBuf::from("/tmp"));
        assert!(config.should_auto_allow_bash());

        let config = SandboxConfig::new(PathBuf::from("/tmp")).auto_allow_bash(false);
        assert!(!config.should_auto_allow_bash());
    }

    #[test]
    fn test_bypass_sandbox() {
        let config = SandboxConfig::new(PathBuf::from("/tmp"));
        let sandbox = Sandbox::new(config);

        assert!(sandbox.can_bypass(true));
        assert!(!sandbox.can_bypass(false));
    }

    /// W-26: when running inside a container, `create_runtime` must
    /// bail out before attempting to build a nested namespace sandbox.
    /// We force detection by setting `KUBERNETES_SERVICE_HOST`, which
    /// [`detect::detect_container`] recognises unconditionally.
    #[test]
    fn nested_sandbox_skipped_inside_container() {
        // Save and restore so we do not leak state into parallel
        // tests running in the same process.
        let prior = std::env::var("KUBERNETES_SERVICE_HOST").ok();
        unsafe {
            std::env::set_var("KUBERNETES_SERVICE_HOST", "10.0.0.1");
        }

        let config = SandboxConfig::new(PathBuf::from("/tmp"));
        let sandbox = Sandbox::new(config);

        // Detection short-circuits `create_runtime` → no runtime is
        // attached, so `is_enabled()` is false even though the
        // original config was `enabled: true`.
        assert!(!sandbox.is_enabled());
        assert!(sandbox.runtime.is_none());
        assert!(
            sandbox.skipped_for_container().is_some(),
            "skipped_for_container must record the detection reason"
        );
        // `apply()` must be a successful no-op when host isolation
        // already applies — not an error.
        assert!(sandbox.apply().is_ok());

        unsafe {
            match prior {
                Some(v) => std::env::set_var("KUBERNETES_SERVICE_HOST", v),
                None => std::env::remove_var("KUBERNETES_SERVICE_HOST"),
            }
        }
    }
}
