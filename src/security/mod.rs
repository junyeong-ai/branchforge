//! Security sandbox system providing TOCTOU-safe file operations and process isolation.
//!
//! This module provides comprehensive security controls:
//! - TOCTOU-safe path resolution using `openat()` with `O_NOFOLLOW`
//! - Symlink attack prevention with depth limiting
//! - AST-based bash command analysis
//! - Environment variable sanitization
//! - Process resource limits via `setrlimit`
//! - OS-level sandboxing (Landlock on Linux, Seatbelt on macOS)

#![allow(missing_docs)]

#[cfg(feature = "coding-tools")]
pub mod bash;
pub mod fs;
pub mod guard;
pub mod limits;
pub mod path;
pub mod policy;
pub mod sandbox;

mod error;

pub use error::SecurityError;
pub use fs::{SecureFileHandle, SecureFs};
pub use guard::SecurityGuard;
pub use limits::ResourceLimits;
pub use path::SafePath;
pub use policy::SecurityPolicy;
pub use sandbox::{DomainCheck, NetworkConfig, NetworkSandbox, Sandbox, SandboxConfig};

use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub struct SecurityContext {
    pub fs: SecureFs,
    #[cfg(feature = "coding-tools")]
    pub bash: bash::BashAnalyzer,
    pub limits: ResourceLimits,
    pub policy: SecurityPolicy,
    pub network: Arc<NetworkSandbox>,
    pub sandbox: Arc<Sandbox>,
}

/// Phase G-4: [`crate::common::Extensions`]-friendly wrapper around
/// [`SecurityContext`].
///
/// Before G-4 the filesystem/shell security state was carried on
/// `ExecutionContext` as a feature-gated field
/// (`#[cfg(feature = "local-fs")] security: Arc<SecurityContext>`). That
/// made the `ExecutionContext` struct's byte layout dependent on which
/// features were compiled in — subtly breaking the "pure Layer 1 types
/// have stable shape" invariant. G-4 moves the handle into the type-map
/// [`crate::common::Extensions`] bag, so `ExecutionContext` is now
/// feature-invariant and adding new optional context concerns no longer
/// requires editing the core struct.
///
/// `SecurityExtension` is registered automatically by every Layer 2a
/// constructor (`ExecutionContext::new`, `from_path`, `try_permissive`),
/// and accessed internally by the `#[cfg(feature = "local-fs")]`
/// dispatcher methods on `ExecutionContext`. Tool authors typically do
/// not interact with it directly — they keep calling `ctx.open_read`,
/// `ctx.analyze_bash`, etc. — but authors wiring up custom security
/// contexts in tests or embedding scenarios register one via
/// [`Extensions::insert`][`crate::common::Extensions::insert`].
#[cfg(feature = "local-fs")]
#[derive(Clone)]
pub struct SecurityExtension(pub Arc<SecurityContext>);

#[cfg(feature = "local-fs")]
impl SecurityExtension {
    /// Build an extension from a [`SecurityContext`]. Takes ownership so
    /// the `Arc` allocation happens exactly once; subsequent
    /// `ExecutionContext` clones share the same inner handle.
    pub fn new(context: SecurityContext) -> Self {
        Self(Arc::new(context))
    }

    /// Shared reference to the underlying security context.
    pub fn context(&self) -> &SecurityContext {
        &self.0
    }
}

impl SecurityContext {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, SecurityError> {
        Self::builder().root(root).build()
    }

    pub fn builder() -> SecurityContextBuilder {
        SecurityContextBuilder::default()
    }

    /// Create a permissive SecurityContext that allows all operations.
    pub fn try_permissive() -> Result<Self, SecurityError> {
        Ok(Self {
            fs: SecureFs::try_permissive()?,
            #[cfg(feature = "coding-tools")]
            bash: bash::BashAnalyzer::new(bash::BashPolicy::default()),
            limits: ResourceLimits::none(),
            policy: SecurityPolicy::permissive(),
            network: Arc::new(NetworkSandbox::permissive()),
            sandbox: Arc::new(Sandbox::disabled()),
        })
    }

    pub fn root(&self) -> &Path {
        self.fs.root()
    }

    pub fn is_sandboxed(&self) -> bool {
        self.sandbox.is_enabled()
    }

    pub fn should_auto_allow_bash(&self) -> bool {
        self.sandbox.should_auto_allow_bash()
    }
}

#[derive(Default)]
pub struct SecurityContextBuilder {
    root: Option<PathBuf>,
    allowed_paths: Vec<PathBuf>,
    denied_patterns: Vec<String>,
    limits: Option<ResourceLimits>,
    #[cfg(feature = "coding-tools")]
    bash_policy: Option<bash::BashPolicy>,
    max_symlink_depth: Option<u8>,
    network: Option<NetworkSandbox>,
    sandbox_config: Option<SandboxConfig>,
}

impl SecurityContextBuilder {
    pub fn root(mut self, path: impl AsRef<Path>) -> Self {
        self.root = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn allowed_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.allowed_paths = paths;
        self
    }

    pub fn denied_patterns(mut self, patterns: Vec<String>) -> Self {
        self.denied_patterns = patterns;
        self
    }

    pub fn limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    #[cfg(feature = "coding-tools")]
    pub fn bash_policy(mut self, policy: bash::BashPolicy) -> Self {
        self.bash_policy = Some(policy);
        self
    }

    pub fn max_symlink_depth(mut self, depth: u8) -> Self {
        self.max_symlink_depth = Some(depth);
        self
    }

    pub fn network(mut self, sandbox: NetworkSandbox) -> Self {
        self.network = Some(sandbox);
        self
    }

    pub fn sandbox(mut self, config: SandboxConfig) -> Self {
        self.sandbox_config = Some(config);
        self
    }

    pub fn sandbox_enabled(mut self, enabled: bool) -> Self {
        let root = self
            .root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        self.sandbox_config = Some(if enabled {
            SandboxConfig::new(root)
        } else {
            SandboxConfig::disabled()
        });
        self
    }

    pub fn auto_allow_bash_if_sandboxed(mut self, auto_allow: bool) -> Self {
        let root = self
            .root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let config = self
            .sandbox_config
            .take()
            .unwrap_or_else(|| SandboxConfig::new(root));
        self.sandbox_config = Some(config.auto_allow_bash(auto_allow));
        self
    }

    pub fn build(self) -> Result<SecurityContext, SecurityError> {
        let root = self
            .root
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let fs = SecureFs::new(
            &root,
            self.allowed_paths.clone(),
            &self.denied_patterns,
            self.max_symlink_depth
                .unwrap_or(crate::security::path::DEFAULT_MAX_SYMLINK_DEPTH),
        )?;

        let sandbox_config = self.sandbox_config.unwrap_or_else(|| {
            SandboxConfig::disabled()
                .working_dir(root)
                .allowed_paths(self.allowed_paths)
                .denied_paths(self.denied_patterns)
        });

        let network = self
            .network
            .unwrap_or_else(|| sandbox_config.to_network_sandbox());

        Ok(SecurityContext {
            fs,
            #[cfg(feature = "coding-tools")]
            bash: bash::BashAnalyzer::new(self.bash_policy.unwrap_or_default()),
            limits: self.limits.unwrap_or_default(),
            policy: SecurityPolicy::default(),
            network: Arc::new(network),
            sandbox: Arc::new(Sandbox::new(sandbox_config)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_security_context_new() {
        let dir = tempdir().unwrap();
        let security = SecurityContext::new(dir.path()).unwrap();
        assert_eq!(security.root(), std::fs::canonicalize(dir.path()).unwrap());
    }

    #[test]
    fn test_security_context_permissive() {
        let security = SecurityContext::try_permissive().unwrap();
        assert_eq!(security.root(), Path::new("/"));
    }

    #[test]
    fn test_builder() {
        let dir = tempdir().unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let security = SecurityContext::builder()
            .root(&canonical_dir)
            .max_symlink_depth(5)
            .limits(ResourceLimits::default())
            .build()
            .unwrap();

        assert_eq!(security.root(), canonical_dir);
    }
}
