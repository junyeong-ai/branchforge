//! Shared agent runtime infrastructure.

use std::sync::Arc;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::recovery_recipes::RecipeRegistry;
use crate::authorization::{ApprovalSender, ExecutionMode};
use crate::budget::{BudgetTracker, TenantBudget};
use crate::client::LlmCall;
use crate::context::PromptOrchestrator;
use crate::context_scope::SharedContextScope;
use crate::events::EventBus;
use crate::hooks::HookRegistry;
use crate::orchestration::{AgentDirectory, Coordination};
use crate::session::compact::CompactionChain;
use crate::tools::{ToolRegistry, ToolSearchEngine};

use super::config::AgentConfig;

/// Shared, immutable infrastructure for agent execution.
///
/// `AgentRuntime` holds everything that can be shared across multiple
/// [`Agent`](super::Agent) instances (sessions). Create one runtime
/// and spawn multiple agents from it for server environments.
///
/// # Field groups
///
/// - **Core call surface** — `llm`, `config`, `tools`, `hooks`. Always
///   present; every iteration of the execution loop touches these.
/// - **Operations** — `event_bus`, `execution_mode`, `context_scope`,
///   `shutdown`. Cross-cutting observability and control.
/// - **Resource accounting** — `budget_tracker`, `tenant_budget`,
///   `mcp_manager`, `tool_search_manager`. External services and budgets
///   that influence what tools and how many tokens the agent may consume.
/// - **Session lifecycle** — `compaction_chain`, `recovery_recipes`.
///   Hooks that fire on session compaction and recovery. The recipe
///   registry is always present (defaults to the canonical builtin
///   set) so the recovery loop has a single, uniform decision
///   surface.
/// - **Multi-agent coordination** — `orchestrator`, `coordination`,
///   `agent_directory`. All `Option` so single-agent execution pays
///   no cost.
pub struct AgentRuntime {
    // ── Core call surface ────────────────────────────────────────────
    /// IR-native LLM call surface. All model invocations go through this.
    pub(crate) llm: Arc<dyn LlmCall>,
    pub(crate) config: Arc<AgentConfig>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) hooks: Arc<HookRegistry>,

    // ── Operations ───────────────────────────────────────────────────
    pub(crate) event_bus: Option<Arc<EventBus>>,
    pub(crate) execution_mode: ExecutionMode,
    pub(crate) approval_sender: Option<ApprovalSender>,
    pub(crate) context_scope: Option<SharedContextScope>,
    pub(crate) shutdown: CancellationToken,
    pub(crate) _shutdown_guard: tokio_util::sync::DropGuard,

    // ── Resource accounting ──────────────────────────────────────────
    pub(crate) budget_tracker: Arc<BudgetTracker>,
    pub(crate) tenant_budget: Option<Arc<TenantBudget>>,
    pub(crate) mcp_manager: Option<Arc<crate::mcp::McpManager>>,
    pub(crate) tool_search_manager: Option<Arc<ToolSearchEngine>>,

    // ── Session lifecycle ────────────────────────────────────────────
    pub(crate) compaction_chain: Option<Arc<CompactionChain>>,
    pub(crate) recovery_recipes: Arc<RecipeRegistry>,

    // ── Multi-agent coordination ─────────────────────────────────────
    pub(crate) orchestrator: Option<Arc<RwLock<PromptOrchestrator>>>,
    pub(crate) coordination: Option<Arc<dyn Coordination>>,
    pub(crate) agent_directory: Option<Arc<AgentDirectory>>,
}

impl AgentRuntime {
    /// Returns a reference to the LLM call surface.
    #[must_use]
    pub fn llm(&self) -> &Arc<dyn LlmCall> {
        &self.llm
    }

    /// Returns the agent configuration.
    #[must_use]
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Returns a reference to the tool registry.
    #[must_use]
    pub fn tools(&self) -> &Arc<ToolRegistry> {
        &self.tools
    }

    /// Returns a reference to the hook manager.
    #[must_use]
    pub fn hooks(&self) -> &Arc<HookRegistry> {
        &self.hooks
    }

    /// Returns a reference to the prompt orchestrator, if configured.
    #[must_use]
    pub fn orchestrator(&self) -> Option<&Arc<RwLock<PromptOrchestrator>>> {
        self.orchestrator.as_ref()
    }

    /// Returns the event bus, if one was configured.
    #[must_use]
    pub fn event_bus(&self) -> Option<&Arc<EventBus>> {
        self.event_bus.as_ref()
    }

    /// Returns the context scope, if one was configured.
    #[must_use]
    pub fn context_scope(&self) -> Option<&SharedContextScope> {
        self.context_scope.as_ref()
    }

    /// Invalidate cached context after compaction so subsequent iterations
    /// rebuild prompts from the compacted session state.
    pub(crate) async fn invalidate_caches_after_compact(&self) {
        if let Some(ref orchestrator) = self.orchestrator {
            let mut orch = orchestrator.write().await;
            orch.invalidate_static_cache();
        }
    }

    /// Signal graceful shutdown.
    ///
    /// Running execution loops will finish their current iteration,
    /// persist session state, and then stop.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// Returns a clone of the [`CancellationToken`] so callers can
    /// monitor or propagate the shutdown signal.
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }
}
