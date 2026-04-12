//! Execution context for tool operations.

#![allow(missing_docs)]

#[cfg(feature = "local-fs")]
use std::path::Path;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::common::Extensions;
use crate::hooks::{HookContext, HookEvent, HookInput, HookRegistry};
use crate::session::{SessionAccessScope, SessionHandle, SessionManager};

#[cfg(feature = "local-fs")]
use std::collections::HashMap;

#[cfg(feature = "local-fs")]
use crate::authorization::{PermissionDecision, ToolLimits};
#[cfg(feature = "coding-tools")]
use crate::security::bash::{BashAnalysis, SanitizedEnv};
#[cfg(feature = "local-fs")]
use crate::security::fs::SecureFileHandle;
#[cfg(feature = "local-fs")]
use crate::security::guard::SecurityGuard;
#[cfg(feature = "local-fs")]
use crate::security::path::SafePath;
#[cfg(feature = "local-fs")]
use crate::security::sandbox::SandboxResult;
#[cfg(feature = "local-fs")]
use crate::security::{ResourceLimits, SecurityContext, SecurityError, SecurityExtension};

// `DomainCheck` is Layer 1 (lives in `crate::network_sandbox`) but a helper
// that returns it (`ExecutionContext::check_domain`) is feature-gated because
// the data source currently flows through `self.security().network`. Once the
// NetworkSandbox handle becomes a direct ExecutionContext field this gate can
// go away — tracked as a Phase 2 follow-up.
#[cfg(feature = "local-fs")]
use crate::network_sandbox::DomainCheck;

/// Step lifecycle status for tool progress events.
#[non_exhaustive]
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

/// Scope carrier for tool execution.
///
/// Despite the `Context` suffix, this is a TypeMap-backed scope carrier —
/// not a `naming.md` "Context pattern" (which would imply mutable shared
/// state). Tools receive a shared `&ExecutionContext` and read extensions
/// from it; they do not mutate it.
///
/// The `extensions` field carries feature-gated and user-provided values
/// (security handles, HITL handler, workspace root, telemetry sinks,
/// tenant ids, …) without widening the struct surface.
#[derive(Clone)]
pub struct ExecutionContext {
    hooks: Option<HookRegistry>,
    session_id: Option<String>,
    session_manager: Option<SessionManager>,
    session_scope: Option<SessionAccessScope>,
    progress_tx: Option<ProgressSender>,
    cancel_token: Option<CancellationToken>,
    /// Type-keyed heterogeneous storage for feature-gated and user-provided
    /// context values. The Layer 2a/2b filesystem/shell security handle
    /// lives here (from a feature-gated struct field) so `ExecutionContext`'s
    /// byte layout is feature-invariant. See [`crate::common::Extensions`]
    /// and `docs/architecture/layering.md` §4 for the rationale.
    extensions: Extensions,
}

impl ExecutionContext {
    /// Construct an empty Layer 1 execution context carrying only the
    /// always-on core fields: hooks, session wiring, progress channel,
    /// cancellation, and the [`Extensions`] TypeMap (initially empty).
    ///
    /// This is the pure-core constructor. It has no knowledge of a
    /// filesystem, workspace, or security policy. Tests, custom tools, and
    /// Layer 1 callers that do not need filesystem access should prefer
    /// this over the `local-fs`-gated constructors.
    ///
    /// Layer 2a builders (such as the filesystem tools registered by the
    /// `local-fs` feature) inject a `SecureFs` handle on top of this via
    /// the [`Extensions`] mechanism.
    pub fn empty() -> Self {
        #[allow(unused_mut)]
        let mut ctx = Self {
            hooks: None,
            session_id: None,
            session_manager: None,
            session_scope: None,
            progress_tx: None,
            cancel_token: None,
            extensions: Extensions::new(),
        };
        // Layer 2a builds seed a permissive security extension so tools
        // invoked against an "empty" context still have fs/network/sandbox
        // primitives to call. Pure Layer 1 builds compile without this
        // block and tools that need security simply don't exist.
        #[cfg(feature = "local-fs")]
        {
            let permissive =
                SecurityContext::try_permissive().expect("try_permissive never fails in practice");
            ctx.extensions.insert(SecurityExtension::new(permissive));
        }
        ctx
    }

    /// Layer 2a constructor: build an execution context around an existing
    /// [`SecurityContext`]. Available only under the `local-fs` feature.
    #[cfg(feature = "local-fs")]
    pub fn new(security: SecurityContext) -> Self {
        let mut ctx = Self {
            hooks: None,
            session_id: None,
            session_manager: None,
            session_scope: None,
            progress_tx: None,
            cancel_token: None,
            extensions: Extensions::new(),
        };
        ctx.extensions.insert(SecurityExtension::new(security));
        ctx
    }

    /// Layer 2a constructor: build an execution context rooted at `root`,
    /// constructing a fresh [`SecurityContext`] internally. Available only
    /// under the `local-fs` feature.
    #[cfg(feature = "local-fs")]
    pub fn from_path(root: impl AsRef<Path>) -> Result<Self, SecurityError> {
        let security = SecurityContext::new(root)?;
        Ok(Self::new(security))
    }

    /// Layer 2a constructor: build a permissive execution context that
    /// allows all filesystem operations. Intended for tests and trusted
    /// embedding scenarios.
    #[cfg(feature = "local-fs")]
    pub fn try_permissive() -> Result<Self, SecurityError> {
        let security = SecurityContext::try_permissive()?;
        Ok(Self::new(security))
    }

    pub fn with_hooks(mut self, hooks: HookRegistry, session_id: impl Into<String>) -> Self {
        self.hooks = Some(hooks);
        self.session_id = Some(session_id.into());
        self
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    // =========================================================================
    // Extensions — type-keyed heterogeneous storage.
    //
    // This is the mechanism by which Layer 2 concerns (workspace, security,
    // git state) and user-provided context (telemetry, multi-tenant markers)
    // attach themselves to the execution context without hard-coding those
    // concepts into Layer 1 fields. See `docs/architecture/layering.md` §4.
    // =========================================================================

    /// Read-only access to the extensions container.
    ///
    /// Tools retrieve their feature-specific context via
    /// `ctx.extensions().get::<MyExtension>()`. Layer 1 tools that do not
    /// depend on any extension should ignore this method entirely.
    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    /// Mutable access to the extensions container.
    ///
    /// Prefer the builder-style [`with_extension`][Self::with_extension] and
    /// [`insert_extension`][Self::insert_extension] for insertion; this method
    /// exists for tools that need to mutate an in-place extension value (for
    /// example a per-turn counter).
    pub fn extensions_mut(&mut self) -> &mut Extensions {
        &mut self.extensions
    }

    /// Insert an extension, builder style.
    ///
    /// Consumes `self`, inserts the value, and returns the updated context.
    /// If an extension of the same type was already present it is replaced.
    ///
    /// # Example
    /// ```ignore
    /// let ctx = ExecutionContext::try_permissive()?
    ///     .with_extension(TenantId("acme".into()))
    ///     .with_extension(TraceId(42));
    /// ```
    #[must_use]
    pub fn with_extension<T>(mut self, value: T) -> Self
    where
        T: Clone + Send + Sync + 'static,
    {
        self.extensions.insert(value);
        self
    }

    /// Insert an extension in place, returning the previous value if any.
    ///
    /// Use this when you already hold a `&mut ExecutionContext` and cannot
    /// consume it. Callers constructing a new context prefer
    /// [`with_extension`][Self::with_extension] for clarity.
    pub fn insert_extension<T>(&mut self, value: T) -> Option<T>
    where
        T: Clone + Send + Sync + 'static,
    {
        self.extensions.insert(value)
    }

    /// Convenience accessor: get an extension by type.
    ///
    /// Equivalent to `self.extensions().get::<T>()`. Provided for ergonomic
    /// call sites that do not need to reach into the `Extensions` API.
    pub fn extension<T>(&self) -> Option<&T>
    where
        T: Send + Sync + 'static,
    {
        self.extensions.get::<T>()
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

    pub async fn persist_session_handle(&self, state: &SessionHandle) -> crate::Result<()> {
        let Some(manager) = self.session_manager.as_ref() else {
            return Ok(());
        };
        let session = state.session().await;
        Ok(manager
            .persist_snapshot(&session, self.session_scope.as_ref())
            .await?)
    }

    /// Returns the current workspace root as an owned `PathBuf`, or the
    /// process working directory if no [`crate::Workspace`] extension has
    /// been inserted. Layer 1 helper — safe to call in any build.
    pub fn workspace_root_buf(&self) -> PathBuf {
        self.extensions
            .get::<crate::Workspace>()
            .map(crate::Workspace::root_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }

    pub async fn fire_hook(&self, event: HookEvent, input: HookInput) {
        if let Some(ref hooks) = self.hooks {
            let cwd = self.workspace_root_buf();
            let context = HookContext::new(input.session_id.clone()).cwd(cwd);
            if let Err(e) = hooks.execute(event, input, &context).await {
                tracing::warn!(error = %e, "Hook execution failed");
            }
        }
    }

    // =========================================================================
    // Layer 2a / Layer 2b helpers.
    //
    // Everything below this line is gated behind `local-fs` (or, where the
    // underlying resource is bash-specific, `coding-tools`). A pure Layer 1
    // build has none of these methods — feature-gated tools that need them
    // are themselves feature-gated, so the gate alignment holds.
    //
    // Phase G-4 moved the `SecurityContext` handle from a feature-gated
    // struct field into the [`Extensions`] type-map, so these helpers now
    // read through `security()` rather than `self.security`. The external
    // API is unchanged — tool authors keep calling `ctx.open_read(path)`,
    // `ctx.analyze_bash(cmd)`, etc. New tool authors who need their own
    // optional context concerns should register their own extension type
    // (see `HumanInteractionExtension` / `SecurityExtension` as patterns)
    // and read it back via `ctx.extensions().get::<YourExtension>()`.
    // =========================================================================

    /// Phase G-4: internal accessor for the [`SecurityContext`] stored in
    /// the [`Extensions`] type-map. Every Layer 2a constructor
    /// (`empty`, `new`, `from_path`, `try_permissive`) registers a
    /// `SecurityExtension`, so absence here is an invariant violation —
    /// it means the caller constructed an `ExecutionContext` by hand
    /// without going through a supported path. The `expect` message
    /// names the remediation explicitly.
    #[cfg(feature = "local-fs")]
    fn security(&self) -> &SecurityContext {
        self.extensions
            .get::<SecurityExtension>()
            .map(|ext| ext.context())
            .expect(
                "SecurityExtension not registered on Layer 2a ExecutionContext — \
                 construct via ExecutionContext::new / from_path / try_permissive, \
                 or insert a SecurityExtension into the Extensions type-map manually",
            )
    }

    #[cfg(feature = "local-fs")]
    pub fn root(&self) -> &Path {
        self.security().root()
    }

    #[cfg(feature = "local-fs")]
    pub fn limits_for(&self, tool_name: &str) -> ToolLimits {
        self.security()
            .policy
            .tool_policy
            .limits(tool_name)
            .cloned()
            .unwrap_or_default()
    }

    #[cfg(feature = "local-fs")]
    pub fn resolve(&self, input: &str) -> Result<SafePath, SecurityError> {
        self.security().fs.resolve(input)
    }

    #[cfg(feature = "local-fs")]
    pub fn resolve_with_limits(
        &self,
        input: &str,
        limits: &ToolLimits,
    ) -> Result<SafePath, SecurityError> {
        self.security().fs.resolve_with_limits(input, limits)
    }

    #[cfg(feature = "local-fs")]
    pub fn resolve_for(&self, tool_name: &str, path: &str) -> Result<SafePath, SecurityError> {
        let limits = self.limits_for(tool_name);
        self.resolve_with_limits(path, &limits)
    }

    /// Tool helper: resolve a path or short-circuit with a `ToolResult::error`.
    ///
    /// The `Err` variant is intentionally `ToolResult` (not boxed) so tool
    /// implementations can write `let p = ctx.try_resolve_for(...)?;` and
    /// return the error directly. The size lint is suppressed because
    /// boxing here would force every call site to dereference manually.
    #[cfg(feature = "local-fs")]
    #[allow(clippy::result_large_err)]
    pub fn try_resolve_for(
        &self,
        tool_name: &str,
        path: &str,
    ) -> Result<SafePath, crate::types::ToolResult> {
        self.resolve_for(tool_name, path)
            .map_err(|e| crate::types::ToolResult::error(e.to_string()))
    }

    /// Same as [`Self::try_resolve_for`] but allows an absent path that
    /// resolves to the sandbox root. Same boxing rationale applies.
    #[cfg(feature = "local-fs")]
    #[allow(clippy::result_large_err)]
    pub fn try_resolve_or_root_for(
        &self,
        tool_name: &str,
        path: Option<&str>,
    ) -> Result<std::path::PathBuf, crate::types::ToolResult> {
        let limits = self.limits_for(tool_name);
        self.resolve_or_root(path, &limits)
            .map_err(|e| crate::types::ToolResult::error(e.to_string()))
    }

    #[cfg(feature = "local-fs")]
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

    #[cfg(feature = "local-fs")]
    pub fn open_read(&self, input: &str) -> Result<SecureFileHandle, SecurityError> {
        self.security().fs.open_read(input)
    }

    #[cfg(feature = "local-fs")]
    pub fn open_write(&self, input: &str) -> Result<SecureFileHandle, SecurityError> {
        self.security().fs.open_write(input)
    }

    #[cfg(feature = "local-fs")]
    pub fn is_within(&self, path: &Path) -> bool {
        self.security().fs.is_within(path)
    }

    #[cfg(feature = "coding-tools")]
    pub fn analyze_bash(&self, command: &str) -> BashAnalysis {
        self.security().bash.analyze(command)
    }

    #[cfg(feature = "coding-tools")]
    pub fn validate_bash(&self, command: &str) -> Result<BashAnalysis, String> {
        self.security().bash.validate(command)
    }

    #[cfg(feature = "coding-tools")]
    fn sanitized_env(&self) -> SanitizedEnv {
        SanitizedEnv::from_current().working_dir(self.root())
    }

    #[cfg(feature = "local-fs")]
    pub fn resource_limits(&self) -> &ResourceLimits {
        &self.security().limits
    }

    #[cfg(feature = "local-fs")]
    pub fn check_domain(&self, domain: &str) -> DomainCheck {
        self.security().network.check(domain)
    }

    #[cfg(feature = "local-fs")]
    pub fn can_bypass_sandbox(&self) -> bool {
        self.security().policy.can_bypass_sandbox()
    }

    #[cfg(feature = "local-fs")]
    pub fn is_sandboxed(&self) -> bool {
        self.security().is_sandboxed()
    }

    #[cfg(feature = "local-fs")]
    pub fn should_auto_allow_bash(&self) -> bool {
        self.security().should_auto_allow_bash()
    }

    #[cfg(feature = "local-fs")]
    pub fn wrap_command(&self, command: &str) -> SandboxResult<String> {
        self.security().sandbox.wrap_command(command)
    }

    #[cfg(feature = "local-fs")]
    pub fn sandbox_env(&self) -> HashMap<String, String> {
        self.security().sandbox.environment_vars()
    }

    #[cfg(feature = "coding-tools")]
    pub fn sanitized_env_with_sandbox(&self) -> SanitizedEnv {
        let sandbox_env = self.sandbox_env();
        self.sanitized_env().with_vars(sandbox_env)
    }

    /// Phase D Workstream A-1: the tool is the single source of truth
    /// for subject extraction. Callers pass the result of
    /// [`crate::tools::Tool::permission_subjects`] directly — the
    /// authorization module no longer maintains a parallel extractor
    /// registry.
    #[cfg(feature = "local-fs")]
    pub fn check_tool_policy(&self, tool_name: &str, subjects: &[String]) -> PermissionDecision {
        self.security()
            .policy
            .tool_policy
            .check(tool_name, subjects)
    }

    #[cfg(feature = "local-fs")]
    pub fn check_explicit_skill_permission(&self, subjects: &[String]) -> PermissionDecision {
        self.security()
            .policy
            .tool_policy
            .check_explicit_skill(subjects)
    }

    #[cfg(feature = "local-fs")]
    pub fn validate_security(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Result<(), String> {
        SecurityGuard::validate(self.security(), tool_name, input).map_err(|e| e.to_string())
    }
}

impl Default for ExecutionContext {
    /// Default execution context.
    ///
    /// In a pure Layer 1 build (`--no-default-features`) this delegates to
    /// [`ExecutionContext::empty()`]. When the `local-fs` feature is active
    /// it additionally attempts to attach a permissive `SecurityContext`
    /// — handy for tests and trusted embedders, but not something you want
    /// in production code (production callers should construct an
    /// explicitly-scoped context via the builder or a tool-specific helper).
    fn default() -> Self {
        #[cfg(feature = "local-fs")]
        {
            if let Ok(security) = SecurityContext::builder().build() {
                return Self::new(security);
            }
            if let Ok(security) = SecurityContext::try_permissive() {
                return Self::new(security);
            }
        }
        Self::empty()
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

    // ---- Pure Layer 1 tests ------------------------------------------------

    #[test]
    fn test_empty_has_no_hooks_or_session() {
        let ctx = ExecutionContext::empty();
        assert!(ctx.session_id().is_none());
        assert!(ctx.cancel_token().is_none());
        assert!(ctx.session_manager().is_none());
        // Phase G-4: pure Layer 1 builds start with an empty type-map;
        // Layer 2a builds seed `SecurityExtension` so tool dispatchers
        // (`open_read`, `analyze_bash`, …) work against a permissive
        // default security context without explicit wiring.
        #[cfg(not(feature = "local-fs"))]
        assert!(ctx.extensions().is_empty());
        #[cfg(feature = "local-fs")]
        assert!(
            ctx.extensions()
                .get::<crate::security::SecurityExtension>()
                .is_some(),
            "Layer 2a empty() must seed SecurityExtension"
        );
    }

    #[test]
    fn test_empty_workspace_root_buf_falls_back_to_cwd() {
        // With no Workspace extension inserted, workspace_root_buf() returns
        // the process working directory. The exact value depends on the
        // test runner, so we only verify it is non-empty (cwd is always set
        // in a cargo test environment).
        let ctx = ExecutionContext::empty();
        let root = ctx.workspace_root_buf();
        assert!(!root.as_os_str().is_empty());
    }

    #[test]
    fn test_workspace_extension_drives_workspace_root() {
        let mut ctx = ExecutionContext::empty();
        ctx.insert_extension(crate::Workspace::new("/tmp/unit-test-root"));
        assert_eq!(
            ctx.workspace_root_buf(),
            std::path::PathBuf::from("/tmp/unit-test-root")
        );
    }

    #[test]
    fn test_extensions_type_keyed_lookup() {
        #[derive(Clone, Debug, PartialEq)]
        struct TenantId(&'static str);

        let ctx = ExecutionContext::empty().with_extension(TenantId("acme"));
        assert_eq!(ctx.extension::<TenantId>(), Some(&TenantId("acme")));
    }

    // ---- Layer 2a (local-fs) tests -----------------------------------------

    #[cfg(feature = "local-fs")]
    #[test]
    fn test_execution_context_new() {
        let dir = tempfile::tempdir().unwrap();
        let context = ExecutionContext::from_path(dir.path()).unwrap();
        assert!(context.is_within(&std::fs::canonicalize(dir.path()).unwrap()));
    }

    #[cfg(feature = "local-fs")]
    #[test]
    fn test_permissive_context() {
        let context = ExecutionContext::try_permissive().unwrap();
        assert!(context.can_bypass_sandbox());
    }

    #[cfg(feature = "local-fs")]
    #[test]
    fn test_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test.txt"), "content").unwrap();

        let context = ExecutionContext::from_path(&root).unwrap();
        let path = context.resolve("test.txt").unwrap();
        assert_eq!(path.as_path(), root.join("test.txt"));
    }

    #[cfg(feature = "local-fs")]
    #[test]
    fn test_path_escape_blocked() {
        let dir = tempfile::tempdir().unwrap();
        let context = ExecutionContext::from_path(dir.path()).unwrap();
        let result = context.resolve("../../../etc/passwd");
        assert!(result.is_err());
    }

    // ---- Layer 2b (coding-tools) tests -------------------------------------

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_analyze_bash() {
        let context = ExecutionContext::default();
        let analysis = context.analyze_bash("cat /etc/passwd");
        assert!(analysis.paths.iter().any(|p| p.path == "/etc/passwd"));
    }
}
