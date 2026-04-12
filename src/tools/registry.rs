//! Tool registry for managing and executing tools.

#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

/// Default timeout for tool execution in milliseconds (2 minutes).
const DEFAULT_TOOL_TIMEOUT_MS: u64 = 120_000;

#[cfg(feature = "coding-tools")]
use super::ProcessScheduler;
use super::builder::ToolRegistryBuilder;
use super::context::ExecutionContext;
use super::env::ToolExecutionEnv;
use super::surface::ToolSurface;
use super::traits::Tool;
use crate::agent::TaskTracker;
use crate::authorization::ToolPolicy;
use crate::session::MemoryPersistence;
use crate::types::{ToolOutput, ToolResult, ToolSpec};
use std::path::PathBuf;

/// Default preview budget in **bytes** for spilled tool results.
/// Kept small on purpose — the whole point of spilling is to
/// remove large payloads from the prompt cache. [`preview_of`]
/// walks back to a UTF-8 char boundary if the budget lands
/// mid-sequence, so multi-byte characters are never split.
const DEFAULT_OVERFLOW_PREVIEW_BYTES: usize = 4_096;

#[derive(Clone)]
pub struct ToolRegistry {
    tools: DashMap<String, Arc<dyn Tool>>,
    task_tracker: TaskTracker,
    env: ToolExecutionEnv,
    /// Phase C-7: optional spill backend for oversized tool results.
    /// `None` disables spilling entirely (the hard `max_output_size`
    /// policy still applies as a safety net). Set at build time via
    /// [`super::builder::ToolRegistryBuilder::overflow_store`].
    overflow_store: Option<Arc<dyn super::OverflowStore>>,
}

impl ToolRegistry {
    pub(crate) fn from_env(task_tracker: TaskTracker, env: ToolExecutionEnv) -> Self {
        Self {
            tools: DashMap::new(),
            task_tracker,
            env,
            overflow_store: None,
        }
    }

    pub(crate) fn set_overflow_store(&mut self, store: Arc<dyn super::OverflowStore>) {
        self.overflow_store = Some(store);
    }

    /// Accessor for the configured overflow store. Consumers (diff
    /// tools, search re-rankers, UI) call this to resolve an
    /// [`super::OverflowRef`] back to its full content.
    pub fn overflow_store(&self) -> Option<&Arc<dyn super::OverflowStore>> {
        self.overflow_store.as_ref()
    }

    /// Start building a [`ToolRegistry`] with custom configuration.
    pub fn builder() -> ToolRegistryBuilder {
        ToolRegistryBuilder::new()
    }


    pub fn default_tools(
        access: ToolSurface,
        working_dir: Option<PathBuf>,
        policy: Option<ToolPolicy>,
    ) -> Self {
        let mut builder = ToolRegistryBuilder::new().access(access);
        if let Some(dir) = working_dir {
            builder = builder.working_dir(dir);
        }
        if let Some(p) = policy {
            builder = builder.policy(p);
        }
        builder.build()
    }

    #[inline]
    pub fn context(&self) -> &ExecutionContext {
        self.env.context()
    }

    #[inline]
    pub fn session_handle(&self) -> Option<&crate::session::session_handle::SessionHandle> {
        self.env.session_handle()
    }

    #[cfg(feature = "coding-tools")]
    #[inline]
    pub fn process_manager(&self) -> Option<&Arc<ProcessScheduler>> {
        self.env.process_manager()
    }

    #[inline]
    pub fn env(&self) -> &ToolExecutionEnv {
        &self.env
    }

    #[inline]
    pub fn task_tracker(&self) -> &TaskTracker {
        &self.task_tracker
    }

    pub fn register(&self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    #[inline]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).map(|r| Arc::clone(r.value()))
    }

    pub async fn execute(&self, name: &str, input: serde_json::Value) -> ToolResult {
        self.execute_with_progress(name, input, None, None).await
    }

    /// Execute a tool with cancellation support.
    ///
    /// Pass a [`CancellationToken`] from `runtime.shutdown.child_token()`
    /// so that graceful shutdown propagates into the tool's `tokio::select!`
    /// race. Tools that explicitly check `ctx.cancel_token()` can also
    /// abort cooperatively.
    pub async fn execute_with_cancel(
        &self,
        name: &str,
        input: serde_json::Value,
        cancel_token: CancellationToken,
    ) -> ToolResult {
        self.execute_with_progress(name, input, None, Some(cancel_token))
            .await
    }

    /// Execute a tool with optional progress channel and cancellation token.
    ///
    /// If `progress_tx` is provided, the tool can call `ctx.progress()`
    /// and progress events will be collected in the channel. The caller
    /// (typically the streaming pipeline) drains these and converts them
    /// to `AgentEvent::ToolProgress` events.
    ///
    /// If `cancel_token` is provided, tool execution races against
    /// cancellation. Tools that opt in can also check the token cooperatively.
    pub(crate) async fn execute_with_progress(
        &self,
        name: &str,
        input: serde_json::Value,
        progress_tx: Option<super::context::ProgressSender>,
        cancel_token: Option<CancellationToken>,
    ) -> ToolResult {
        use tracing::{Instrument, Level, field, span};

        let tool_start = std::time::Instant::now();
        // Per-tool-call observability span. Stable attribute names
        // (`tool.name`, `tool.duration_ms`, `tool.error`,
        // `error.category`) so downstream OTel / Honeycomb / Grafana
        // dashboards can group by tool name without parsing text logs.
        // Attributes start `Empty` and are recorded as the call
        // progresses — same pattern as `ApiCallSpan`.
        let tool_span = span!(
            Level::INFO,
            "tool.execute",
            otel.name = "tool.execute",
            "tool.name" = name,
            "tool.duration_ms" = field::Empty,
            "tool.error" = field::Empty,
            "error.category" = field::Empty,
            "otel.status_code" = field::Empty,
        );

        let result = self
            .execute_with_progress_inner(name, input, progress_tx, cancel_token)
            .instrument(tool_span.clone())
            .await;

        let duration_ms = tool_start.elapsed().as_millis() as u64;
        tool_span.record("tool.duration_ms", duration_ms);
        if result.is_error() {
            tool_span.record("tool.error", true);
            tool_span.record("otel.status_code", "ERROR");
            // Coarse category: every tool-level failure rolls up to
            // `ToolRuntime` in the FailureCategory vocabulary. A more
            // fine-grained classification (timeout, authorization
            // denied, unknown tool) can be derived from the ToolResult
            // variant in a future span-to-metric bridge.
            tool_span.record(
                "error.category",
                crate::FailureCategory::ToolRuntime.as_str(),
            );
        }

        result
    }

    async fn execute_with_progress_inner(
        &self,
        name: &str,
        input: serde_json::Value,
        progress_tx: Option<super::context::ProgressSender>,
        cancel_token: Option<CancellationToken>,
    ) -> ToolResult {
        let tool = match self.tools.get(name) {
            Some(t) => Arc::clone(t.value()),
            None => return ToolResult::unknown_tool(name),
        };

        // Security validation first — catches structural violations
        // regardless of tool policy. This entire block is Layer 2a
        // (filesystem security) and is only compiled when the relevant
        // feature is active. In pure Layer 1 builds no filesystem tools
        // exist, so there is nothing to validate.
        #[cfg(feature = "local-fs")]
        if let Err(e) = self.env.context().validate_security(name, &input) {
            return ToolResult::security_error(e);
        }

        #[cfg(feature = "local-fs")]
        {
            // Phase D A-1: the tool is the single source of truth for
            // subject extraction. Ask it, then hand the resulting
            // subjects to the policy engine.
            let subjects = tool.permission_subjects(&input);
            let decision = self.env.context().check_tool_policy(name, &subjects);
            if decision.is_denied() {
                return ToolResult::authorization_denied(name, decision.reason());
            }
        }

        #[cfg(feature = "local-fs")]
        let limits = self.env.context().limits_for(name);
        #[cfg(not(feature = "local-fs"))]
        let limits = crate::authorization::ToolLimits::default();

        let timeout_ms = limits.timeout_ms.unwrap_or(DEFAULT_TOOL_TIMEOUT_MS);

        // Create a context with progress channel and cancel token if provided
        let mut ctx = self.env.context().clone();
        if let Some(ptx) = progress_tx {
            ctx = ctx.with_progress(ptx);
        }
        if let Some(ref token) = cancel_token {
            ctx = ctx.with_cancel_token(token.clone());
        }

        let result = tokio::select! {
            timeout_result = tokio::time::timeout(
                Duration::from_millis(timeout_ms),
                tool.execute(input, &ctx),
            ) => {
                match timeout_result {
                    Ok(tool_result) => tool_result,
                    Err(_) => return ToolResult::timeout(timeout_ms),
                }
            }
            _ = async {
                if let Some(ref token) = cancel_token {
                    token.cancelled().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                return ToolResult::error("Tool execution cancelled");
            }
        };

        // Phase C-7: result-size spill. Runs BEFORE the hard policy
        // truncate so the original full content lands in the overflow
        // store intact. After spilling, the inline payload is a
        // short preview and `result.overflow` points at the stored
        // blob; the policy truncate below is a no-op on short previews.
        let result = self.maybe_spill_oversized(&tool, result).await;

        self.apply_output_limits(result, &limits)
    }

    /// Phase C-7: spill oversized results to the configured
    /// [`super::OverflowStore`]. No-op when either the store is
    /// unset, the tool's self-declared budget is not exceeded, or
    /// the output is not a `Success(String)` (errors and empty
    /// results are never spilled — they are cheap by construction).
    async fn maybe_spill_oversized(
        &self,
        tool: &Arc<dyn Tool>,
        mut result: ToolResult,
    ) -> ToolResult {
        let Some(store) = self.overflow_store.as_ref() else {
            return result;
        };
        let budget = tool.max_result_size_bytes();

        // Only `Success(String)` is ever spilled. Errors are small
        // by construction, and `Empty` / `SuccessBlocks` aren't
        // text-shaped enough to justify spill without a richer
        // trait. Borrow the content to test the budget so we can
        // bail out without mutating `result` on the hot path.
        let needs_spill = matches!(
            &result.output,
            ToolOutput::Success(s) if s.len() > budget
        );
        if !needs_spill {
            return result;
        }

        // Extract the full content by swapping in `Empty`. At this
        // point we've already proved the output is oversized
        // `Success`, so this move always succeeds. The fallback
        // case is unreachable but kept as a defensive restore.
        let full = match std::mem::replace(&mut result.output, ToolOutput::Empty) {
            ToolOutput::Success(s) => s,
            other => {
                result.output = other;
                return result;
            }
        };

        let preview = super::preview_of(&full, DEFAULT_OVERFLOW_PREVIEW_BYTES);
        // Clone the content for the store so we can restore the
        // original `ToolOutput::Success(full)` on failure instead
        // of discarding data. `MemoryOverflowStore` never errors;
        // the clone is the cost of correctness for pluggable
        // remote stores (S3, Postgres) that can.
        match store.store(full.clone(), preview.clone()).await {
            Ok(overflow_ref) => {
                result.output = ToolOutput::Success(preview);
                result.overflow = Some(overflow_ref);
            }
            Err(e) => {
                tracing::warn!(
                    tool = %tool.name(),
                    error = %e,
                    "Overflow store rejected spill — falling back to inline result"
                );
                // Fail-safe: restore the original content inline.
                // The policy-level `max_output_size` truncate still
                // caps the on-the-wire size, so the session graph
                // stays bounded even when spill is unavailable.
                result.output = ToolOutput::Success(full);
            }
        }
        result
    }

    fn apply_output_limits(
        &self,
        mut result: ToolResult,
        limits: &crate::authorization::ToolLimits,
    ) -> ToolResult {
        if let Some(max_size) = limits.max_output_size
            && let ToolOutput::Success(ref content) = result.output
            && content.len() > max_size
        {
            let truncated = format!(
                "{}...\n(output truncated at {} bytes)",
                &content[..content.floor_char_boundary(max_size)],
                max_size
            );
            result.output = ToolOutput::Success(truncated);
        }
        result
    }

    /// Returns tool definitions sorted by name for prompt cache stability.
    pub fn definitions(&self) -> Vec<ToolSpec> {
        let mut defs: Vec<_> = self.tools.iter().map(|r| r.value().definition()).collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }

    /// Invoke `f` on every registered tool in deterministic
    /// (name-sorted) order. Used by `AgentEvent::Init` assembly
    /// and by diagnostic tooling that needs to inspect metadata
    /// (aliases, search hint, interrupt behavior) beyond what
    /// `ToolSpec` carries.
    pub fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(&Arc<dyn Tool>),
    {
        let mut names: Vec<_> = self.tools.iter().map(|r| r.key().clone()).collect();
        names.sort();
        for name in names {
            if let Some(entry) = self.tools.get(&name) {
                f(entry.value());
            }
        }
    }

    /// Returns tool names sorted alphabetically for prompt cache stability.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.tools.iter().map(|r| r.key().clone()).collect();
        names.sort();
        names
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn register_dynamic(&self, tool: Arc<dyn Tool>) -> crate::Result<()> {
        let name = tool.name().to_string();
        if self.tools.contains_key(&name) {
            return Err(crate::Error::Config(format!(
                "Tool already registered: {}",
                name
            )));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    pub fn register_or_replace(&self, tool: Arc<dyn Tool>) -> Option<Arc<dyn Tool>> {
        let name = tool.name().to_string();
        self.tools.insert(name, tool)
    }

    pub fn unregister(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.remove(name).map(|(_, v)| v)
    }
}

impl Default for ToolRegistry {
    /// Empty registry with no tools and a minimal execution environment.
    /// Use [`Self::builder`] to construct a production-ready registry with
    /// default tools and security context.
    fn default() -> Self {
        Self {
            tools: DashMap::new(),
            task_tracker: TaskTracker::new(Arc::new(MemoryPersistence::new())),
            env: ToolExecutionEnv::new(ExecutionContext::empty()),
            overflow_store: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "coding-tools")]
    use crate::tools::surface::ToolSurface;

    #[test]
    fn test_tool_output() {
        assert!(!ToolOutput::success("ok").is_error());
        assert!(ToolOutput::error("fail").is_error());
        assert!(!ToolOutput::empty().is_error());
    }

    // ── Phase C-7: result-size spill tests ──────────────────────
    //
    // The shared `BigTool` emits an oversized payload with a tiny
    // self-declared budget. Tests construct registries via the
    // builder with `ToolPolicy::permissive()` so they exercise the
    // real `execute_with_progress_inner` pipeline under BOTH feature
    // configurations (pure Layer 1 and `local-fs`).

    #[cfg(test)]
    struct BigTool;

    #[cfg(test)]
    #[async_trait::async_trait]
    impl Tool for BigTool {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn name(&self) -> &str {
            "Big"
        }
        fn description(&self) -> &str {
            "emits an oversized payload for spill tests"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn max_result_size_bytes(&self) -> usize {
            // Tiny budget so a 10 KiB payload trips the spill.
            256
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &super::ExecutionContext,
        ) -> ToolResult {
            ToolResult::success("a".repeat(10_000))
        }
    }

    /// When a tool returns content larger than its
    /// [`Tool::max_result_size_bytes`], the registry spills the
    /// full payload into the configured [`super::OverflowStore`]
    /// and replaces the inline `ToolOutput` with a short preview +
    /// an [`super::OverflowRef`]. Runs under both pure-core and
    /// `local-fs` because the registry is built through
    /// `ToolRegistryBuilder` with a permissive policy.
    #[tokio::test]
    async fn test_overflow_spill_replaces_oversized_success() {
        use super::super::{MemoryOverflowStore, OverflowStore};
        use crate::authorization::ToolPolicy;

        let store = Arc::new(MemoryOverflowStore::new()) as Arc<dyn OverflowStore>;
        let registry = ToolRegistry::builder()
            .access(super::ToolSurface::only(["Big"]))
            .policy(ToolPolicy::permissive())
            .overflow_store(Arc::clone(&store))
            .custom_tool(Arc::new(BigTool))
            .build();

        let result = registry.execute("Big", serde_json::json!({})).await;
        assert!(!result.is_error(), "spill must not surface as error");
        let inline_len = result.output.text().len();
        assert!(
            inline_len < 10_000,
            "inline payload must be replaced with a preview; got {inline_len}"
        );

        let overflow = result.overflow.expect("overflow ref must be set");
        assert_eq!(overflow.store, "memory");
        assert_eq!(overflow.size_bytes, 10_000);

        // The ref round-trips back to the full content.
        let full = store.load(&overflow.id).await.unwrap().unwrap();
        assert_eq!(full.len(), 10_000);
    }

    /// Backwards-compatible default: no overflow store configured
    /// means the registry hands the full payload back inline. The
    /// agent runtime falls back to the hard `ToolLimits.max_output_size`
    /// policy cap for bound safety.
    #[tokio::test]
    async fn test_overflow_noop_without_store_configured() {
        use crate::authorization::ToolPolicy;

        let registry = ToolRegistry::builder()
            .access(super::ToolSurface::only(["Big"]))
            .policy(ToolPolicy::permissive())
            .custom_tool(Arc::new(BigTool))
            .build();
        let result = registry.execute("Big", serde_json::json!({})).await;
        assert!(result.overflow.is_none(), "no store → no spill");
        assert_eq!(result.output.text().len(), 10_000);
    }

    /// Fail-safe: when the configured [`super::OverflowStore`]
    /// returns an error, the registry must restore the original
    /// content inline instead of replacing it with an error
    /// string. The policy-level `max_output_size` truncate still
    /// bounds the on-the-wire payload.
    #[tokio::test]
    async fn test_overflow_spill_failure_preserves_content() {
        use super::super::OverflowStore;
        use crate::authorization::ToolPolicy;
        use async_trait::async_trait;

        #[derive(Debug)]
        struct FailingStore;
        #[async_trait]
        impl OverflowStore for FailingStore {
            fn name(&self) -> &str {
                "failing"
            }
            async fn store(
                &self,
                _content: String,
                _preview: String,
            ) -> crate::Result<super::super::OverflowRef> {
                Err(crate::Error::Config("simulated backend failure".into()))
            }
            async fn load(&self, _id: &str) -> crate::Result<Option<String>> {
                Ok(None)
            }
        }

        let store = Arc::new(FailingStore) as Arc<dyn OverflowStore>;
        let registry = ToolRegistry::builder()
            .access(super::ToolSurface::only(["Big"]))
            .policy(ToolPolicy::permissive())
            .overflow_store(store)
            .custom_tool(Arc::new(BigTool))
            .build();

        let result = registry.execute("Big", serde_json::json!({})).await;
        assert!(
            !result.is_error(),
            "spill failure must not turn the result into an error"
        );
        assert!(result.overflow.is_none(), "no overflow ref on failure");
        assert_eq!(
            result.output.text().len(),
            10_000,
            "original content must be preserved inline when spill fails"
        );
    }

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_default_tools_count() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        assert!(registry.contains("Read"));
        assert!(registry.contains("Write"));
        assert!(registry.contains("Edit"));
        assert!(registry.contains("Glob"));
        assert!(registry.contains("Grep"));
        assert!(registry.contains("Bash"));
        assert!(registry.contains("KillShell"));
        assert!(registry.contains("Task"));
        assert!(registry.contains("TaskOutput"));
        assert!(registry.contains("TodoWrite"));
        assert!(registry.contains("Plan"));
        assert!(registry.contains("Skill"));
        assert!(registry.contains("AskUserQuestion"));
        assert!(!registry.contains("GraphHistory"));
    }

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_tool_surface_filtering() {
        let registry =
            ToolRegistry::default_tools(ToolSurface::only(["Read", "Write"]), None, None);
        assert!(registry.contains("Read"));
        assert!(registry.contains("Write"));
        assert!(!registry.contains("Bash"));
    }

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_register_dynamic() {
        let registry = ToolRegistry::default();
        let tool: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);

        assert!(registry.register_dynamic(tool.clone()).is_ok());
        assert!(registry.contains("Read"));

        let result = registry.register_dynamic(tool);
        assert!(result.is_err());
    }

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_register_or_replace() {
        let registry = ToolRegistry::default();
        let tool1: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);
        let tool2: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);

        let old = registry.register_or_replace(tool1);
        assert!(old.is_none());

        let old = registry.register_or_replace(tool2);
        assert!(old.is_some());
    }

    #[test]
    fn test_execution_mode_plan_allows_and_blocks() {
        use crate::authorization::ExecutionMode;

        let mode = ExecutionMode::Plan;

        // Plan mode allows exploration tools
        assert!(mode.allows_tool("Read"));
        assert!(mode.allows_tool("Glob"));
        assert!(mode.allows_tool("Grep"));
        assert!(mode.allows_tool("TodoWrite"));
        assert!(mode.allows_tool("Plan"));
        assert!(mode.allows_tool("GraphHistory"));

        // Plan mode blocks mutation tools
        assert!(!mode.allows_tool("Write"));
        assert!(!mode.allows_tool("Edit"));
        assert!(!mode.allows_tool("Bash"));

        // Non-plan modes allow all tools
        let auto = ExecutionMode::Auto;
        assert!(auto.allows_tool("Write"));
        assert!(auto.allows_tool("Bash"));
    }

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_unregister() {
        let registry = ToolRegistry::default();
        let tool: Arc<dyn Tool> = Arc::new(crate::tools::ReadTool);

        registry.register(tool);
        assert!(registry.contains("Read"));

        let removed = registry.unregister("Read");
        assert!(removed.is_some());
        assert!(!registry.contains("Read"));

        let removed = registry.unregister("NonExistent");
        assert!(removed.is_none());
    }
}
