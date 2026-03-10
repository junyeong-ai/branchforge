//! Execution context for tool operations.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::hooks::{HookContext, HookEvent, HookInput, HookManager};
use crate::permissions::{PermissionResult, ToolLimits};
use crate::security::bash::{BashAnalysis, SanitizedEnv};
use crate::security::fs::SecureFileHandle;
use crate::security::guard::SecurityGuard;
use crate::security::path::SafePath;
use crate::security::sandbox::{DomainCheck, SandboxResult};
use crate::security::{ResourceLimits, SecurityContext, SecurityError};
use crate::session::SessionManager;

#[derive(Clone)]
pub struct ExecutionContext {
    security: Arc<SecurityContext>,
    hooks: Option<HookManager>,
    session_id: Option<String>,
    session_manager: Option<SessionManager>,
}

impl ExecutionContext {
    pub fn new(security: SecurityContext) -> Self {
        Self {
            security: Arc::new(security),
            hooks: None,
            session_id: None,
            session_manager: None,
        }
    }

    pub fn from_path(root: impl AsRef<Path>) -> Result<Self, SecurityError> {
        let security = SecurityContext::new(root)?;
        Ok(Self::new(security))
    }

    /// Create a permissive ExecutionContext that allows all operations.
    ///
    /// # Panics
    /// Panics if the root filesystem cannot be accessed.
    pub fn permissive() -> Self {
        Self {
            security: Arc::new(SecurityContext::permissive()),
            hooks: None,
            session_id: None,
            session_manager: None,
        }
    }

    pub fn hooks(mut self, hooks: HookManager, session_id: impl Into<String>) -> Self {
        self.hooks = Some(hooks);
        self.session_id = Some(session_id.into());
        self
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn session_manager(mut self, manager: SessionManager) -> Self {
        self.session_manager = Some(manager);
        self
    }

    pub fn graph_manager(&self) -> Option<&SessionManager> {
        self.session_manager.as_ref()
    }

    pub async fn fire_hook(&self, event: HookEvent, input: HookInput) {
        if let Some(ref hooks) = self.hooks {
            let context = HookContext::new(input.session_id.clone()).cwd(self.root().to_path_buf());
            if let Err(e) = hooks.execute(event, input, &context).await {
                tracing::warn!(error = %e, "Hook execution failed");
            }
        }
    }

    pub fn root(&self) -> &Path {
        self.security.root()
    }

    pub fn limits_for(&self, tool_name: &str) -> ToolLimits {
        self.security
            .policy
            .permission
            .limits(tool_name)
            .cloned()
            .unwrap_or_default()
    }

    pub fn resolve(&self, input: &str) -> Result<SafePath, SecurityError> {
        self.security.fs.resolve(input)
    }

    pub fn resolve_with_limits(
        &self,
        input: &str,
        limits: &ToolLimits,
    ) -> Result<SafePath, SecurityError> {
        self.security.fs.resolve_with_limits(input, limits)
    }

    pub fn resolve_for(&self, tool_name: &str, path: &str) -> Result<SafePath, SecurityError> {
        let limits = self.limits_for(tool_name);
        self.resolve_with_limits(path, &limits)
    }

    pub fn try_resolve_for(
        &self,
        tool_name: &str,
        path: &str,
    ) -> Result<SafePath, crate::types::ToolResult> {
        self.resolve_for(tool_name, path)
            .map_err(|e| crate::types::ToolResult::error(e.to_string()))
    }

    pub fn try_resolve_or_root_for(
        &self,
        tool_name: &str,
        path: Option<&str>,
    ) -> Result<std::path::PathBuf, crate::types::ToolResult> {
        let limits = self.limits_for(tool_name);
        self.resolve_or_root(path, &limits)
            .map_err(|e| crate::types::ToolResult::error(e.to_string()))
    }

    pub fn resolve_or_root(
        &self,
        path: Option<&str>,
        limits: &ToolLimits,
    ) -> Result<std::path::PathBuf, SecurityError> {
        match path {
            Some(p) => self
                .resolve_with_limits(p, limits)
                .map(|sp| sp.as_path().to_path_buf()),
            None => Ok(self.root().to_path_buf()),
        }
    }

    pub fn open_read(&self, input: &str) -> Result<SecureFileHandle, SecurityError> {
        self.security.fs.open_read(input)
    }

    pub fn open_write(&self, input: &str) -> Result<SecureFileHandle, SecurityError> {
        self.security.fs.open_write(input)
    }

    pub fn is_within(&self, path: &Path) -> bool {
        self.security.fs.is_within(path)
    }

    pub fn analyze_bash(&self, command: &str) -> BashAnalysis {
        self.security.bash.analyze(command)
    }

    pub fn validate_bash(&self, command: &str) -> Result<BashAnalysis, String> {
        self.security.bash.validate(command)
    }

    fn sanitized_env(&self) -> SanitizedEnv {
        SanitizedEnv::from_current().working_dir(self.root())
    }

    pub fn resource_limits(&self) -> &ResourceLimits {
        &self.security.limits
    }

    pub fn check_domain(&self, domain: &str) -> DomainCheck {
        self.security.network.check(domain)
    }

    pub fn can_bypass_sandbox(&self) -> bool {
        self.security.policy.can_bypass_sandbox()
    }

    pub fn is_sandboxed(&self) -> bool {
        self.security.is_sandboxed()
    }

    pub fn should_auto_allow_bash(&self) -> bool {
        self.security.should_auto_allow_bash()
    }

    pub fn wrap_command(&self, command: &str) -> SandboxResult<String> {
        self.security.sandbox.wrap_command(command)
    }

    pub fn sandbox_env(&self) -> HashMap<String, String> {
        self.security.sandbox.environment_vars()
    }

    pub fn sanitized_env_with_sandbox(&self) -> SanitizedEnv {
        let sandbox_env = self.sandbox_env();
        self.sanitized_env().vars(sandbox_env)
    }

    pub fn check_permission(&self, tool_name: &str, input: &serde_json::Value) -> PermissionResult {
        self.security.policy.permission.check(tool_name, input)
    }

    pub fn validate_security(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Result<(), String> {
        SecurityGuard::validate(&self.security, tool_name, input).map_err(|e| e.to_string())
    }
}

impl Default for ExecutionContext {
    fn default() -> Self {
        let security = SecurityContext::builder()
            .build()
            .unwrap_or_else(|_| SecurityContext::permissive());
        Self::new(security)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_execution_context_new() {
        let dir = tempdir().unwrap();
        let context = ExecutionContext::from_path(dir.path()).unwrap();
        assert!(context.is_within(&std::fs::canonicalize(dir.path()).unwrap()));
    }

    #[test]
    fn test_permissive_context() {
        let context = ExecutionContext::permissive();
        assert!(context.can_bypass_sandbox());
    }

    #[test]
    fn test_resolve() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test.txt"), "content").unwrap();

        let context = ExecutionContext::from_path(&root).unwrap();
        let path = context.resolve("test.txt").unwrap();
        assert_eq!(path.as_path(), root.join("test.txt"));
    }

    #[test]
    fn test_path_escape_blocked() {
        let dir = tempdir().unwrap();
        let context = ExecutionContext::from_path(dir.path()).unwrap();
        let result = context.resolve("../../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_analyze_bash() {
        let context = ExecutionContext::default();
        let analysis = context.analyze_bash("cat /etc/passwd");
        assert!(analysis.paths.iter().any(|p| p.path == "/etc/passwd"));
    }
}
