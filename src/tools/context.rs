//! Execution context for tool operations.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::authorization::{ToolDecision, ToolLimits};
use crate::hooks::{HookContext, HookEvent, HookInput, HookManager};
#[cfg(feature = "coding-tools")]
use crate::security::bash::{BashAnalysis, SanitizedEnv};
use crate::security::fs::SecureFileHandle;
use crate::security::guard::SecurityGuard;
use crate::security::path::SafePath;
use crate::security::sandbox::{DomainCheck, SandboxResult};
use crate::security::{ResourceLimits, SecurityContext, SecurityError};
use crate::session::{SessionAccessScope, SessionManager, ToolState};

/// Step lifecycle status for tool progress events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStatus {
    Started,
    Completed,
    Failed,
}

/// Progress event from a tool sub-step, emitted via [`ExecutionContext::progress()`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub step: String,
    pub status: ProgressStatus,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Sender for tool progress events (bounded to prevent runaway memory).
pub(crate) type ProgressSender = tokio::sync::mpsc::Sender<ProgressEvent>;

/// Channel buffer size for tool progress events.
pub(crate) const PROGRESS_CHANNEL_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct ExecutionContext {
    security: Arc<SecurityContext>,
    hooks: Option<HookManager>,
    session_id: Option<String>,
    session_manager: Option<SessionManager>,
    session_scope: Option<SessionAccessScope>,
    progress_tx: Option<ProgressSender>,
    cancel_token: Option<CancellationToken>,
}

impl ExecutionContext {
    pub fn new(security: SecurityContext) -> Self {
        Self {
            security: Arc::new(security),
            hooks: None,
            session_id: None,
            session_manager: None,
            session_scope: None,
            progress_tx: None,
            cancel_token: None,
        }
    }

    pub fn from_path(root: impl AsRef<Path>) -> Result<Self, SecurityError> {
        let security = SecurityContext::new(root)?;
        Ok(Self::new(security))
    }

    /// Create a permissive ExecutionContext that allows all operations.
    pub fn try_permissive() -> Result<Self, crate::security::SecurityError> {
        Ok(Self {
            security: Arc::new(SecurityContext::try_permissive()?),
            hooks: None,
            session_id: None,
            session_manager: None,
            session_scope: None,
            progress_tx: None,
            cancel_token: None,
        })
    }

    pub fn with_hooks(mut self, hooks: HookManager, session_id: impl Into<String>) -> Self {
        self.hooks = Some(hooks);
        self.session_id = Some(session_id.into());
        self
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Attach a progress channel for tool sub-step events.
    pub(crate) fn with_progress(mut self, tx: ProgressSender) -> Self {
        self.progress_tx = Some(tx);
        self
    }

    /// Attach a cancellation token for cooperative tool cancellation.
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel_token = Some(token);
        self
    }

    /// Returns the cancellation token, if one was attached.
    pub fn cancel_token(&self) -> Option<&CancellationToken> {
        self.cancel_token.as_ref()
    }

    /// Create a progress builder for the given step name.
    ///
    /// Progress events appear in the agent event stream as `AgentEvent::ToolProgress`
    /// between `ToolStart` and `ToolComplete`. Optional — tools that don't call
    /// this method produce no progress events.
    ///
    /// Progress events **must** be emitted within the `execute()` call lifetime.
    /// Events from spawned background tasks after `execute()` returns may be lost.
    ///
    /// # Example
    ///
    /// ```ignore
    /// ctx.progress("parsing").started();
    /// // ... do work ...
    /// ctx.progress("parsing").completed(elapsed_ms);
    /// ```
    pub fn progress(&self, step: &str) -> ProgressBuilder<'_> {
        ProgressBuilder {
            ctx: self,
            step: step.to_string(),
        }
    }

    pub fn with_session_manager(mut self, manager: SessionManager) -> Self {
        self.session_manager = Some(manager);
        self
    }

    pub fn with_session_scope(mut self, scope: SessionAccessScope) -> Self {
        self.session_scope = Some(scope);
        self
    }

    pub fn session_manager(&self) -> Option<&SessionManager> {
        self.session_manager.as_ref()
    }

    pub fn session_scope_ref(&self) -> Option<&SessionAccessScope> {
        self.session_scope.as_ref()
    }

    pub async fn persist_tool_state(&self, state: &ToolState) -> crate::Result<()> {
        let Some(manager) = self.session_manager.as_ref() else {
            return Ok(());
        };
        let session = state.session().await;
        Ok(manager
            .persist_snapshot(&session, self.session_scope.as_ref())
            .await?)
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
            .tool_policy
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

    #[cfg(feature = "coding-tools")]
    pub fn analyze_bash(&self, command: &str) -> BashAnalysis {
        self.security.bash.analyze(command)
    }

    #[cfg(feature = "coding-tools")]
    pub fn validate_bash(&self, command: &str) -> Result<BashAnalysis, String> {
        self.security.bash.validate(command)
    }

    #[cfg(feature = "coding-tools")]
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

    #[cfg(feature = "coding-tools")]
    pub fn sanitized_env_with_sandbox(&self) -> SanitizedEnv {
        let sandbox_env = self.sandbox_env();
        self.sanitized_env().with_vars(sandbox_env)
    }

    pub fn check_tool_policy(&self, tool_name: &str, input: &serde_json::Value) -> ToolDecision {
        self.security.policy.tool_policy.check(tool_name, input)
    }

    pub fn check_explicit_skill_permission(&self, input: &serde_json::Value) -> ToolDecision {
        self.security.policy.tool_policy.check_explicit_skill(input)
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
            .or_else(|_| SecurityContext::try_permissive())
            .expect("failed to create security context");
        Self::new(security)
    }
}

/// Builder for emitting a progress event for a specific step.
///
/// Obtained via [`ExecutionContext::progress()`]. Each method consumes the
/// builder and sends the event through the progress channel (if attached).
#[must_use = "progress builder does nothing if not consumed; call .started(), .completed(), or .failed()"]
pub struct ProgressBuilder<'a> {
    ctx: &'a ExecutionContext,
    step: String,
}

impl<'a> ProgressBuilder<'a> {
    /// Emit a "started" progress event.
    pub fn started(self) {
        self.emit(ProgressStatus::Started, None, None);
    }

    /// Emit a "completed" progress event with duration.
    pub fn completed(self, duration_ms: u64) {
        self.emit(ProgressStatus::Completed, Some(duration_ms), None);
    }

    /// Emit a "failed" progress event with duration.
    pub fn failed(self, duration_ms: u64) {
        self.emit(ProgressStatus::Failed, Some(duration_ms), None);
    }

    /// Emit a "completed" progress event with duration and metadata.
    pub fn completed_with(self, duration_ms: u64, metadata: serde_json::Value) {
        self.emit(ProgressStatus::Completed, Some(duration_ms), Some(metadata));
    }

    /// Emit a "failed" progress event with duration and metadata.
    pub fn failed_with(self, duration_ms: u64, metadata: serde_json::Value) {
        self.emit(ProgressStatus::Failed, Some(duration_ms), Some(metadata));
    }

    fn emit(
        self,
        status: ProgressStatus,
        duration_ms: Option<u64>,
        metadata: Option<serde_json::Value>,
    ) {
        if let Some(tx) = &self.ctx.progress_tx
            && let Err(e) = tx.try_send(ProgressEvent {
                step: self.step,
                status,
                timestamp: chrono::Utc::now(),
                duration_ms,
                metadata,
            })
        {
            tracing::debug!(error = %e, "progress event dropped");
        }
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
        let context = ExecutionContext::try_permissive().unwrap();
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

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_analyze_bash() {
        let context = ExecutionContext::default();
        let analysis = context.analyze_bash("cat /etc/passwd");
        assert!(analysis.paths.iter().any(|p| p.path == "/etc/passwd"));
    }
}
