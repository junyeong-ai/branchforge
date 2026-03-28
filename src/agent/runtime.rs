//! Shared agent runtime infrastructure.

use std::sync::Arc;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::authorization::ExecutionMode;
use crate::budget::{BudgetTracker, TenantBudget};
use crate::client::Client;
use crate::context::PromptOrchestrator;
use crate::context_scope::SharedContextScope;
use crate::events::EventBus;
use crate::hooks::HookManager;
use crate::tools::{ToolRegistry, ToolSearchManager};

use super::config::AgentConfig;

/// Shared, immutable infrastructure for agent execution.
///
/// `AgentRuntime` holds everything that can be shared across multiple
/// [`Agent`](super::Agent) instances (sessions). Create one runtime
/// and spawn multiple agents from it for server environments.
pub struct AgentRuntime {
    pub(crate) client: Arc<Client>,
    pub(crate) config: Arc<AgentConfig>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) hooks: Arc<HookManager>,
    pub(crate) budget_tracker: Arc<BudgetTracker>,
    pub(crate) tenant_budget: Option<Arc<TenantBudget>>,
    pub(crate) mcp_manager: Option<Arc<crate::mcp::McpManager>>,
    pub(crate) tool_search_manager: Option<Arc<ToolSearchManager>>,
    pub(crate) event_bus: Option<Arc<EventBus>>,
    pub(crate) execution_mode: ExecutionMode,
    pub(crate) context_scope: Option<SharedContextScope>,
    pub(crate) orchestrator: Option<Arc<RwLock<PromptOrchestrator>>>,
    pub(crate) shutdown: CancellationToken,
}

impl AgentRuntime {
    /// Returns a reference to the underlying [`Client`].
    #[must_use]
    pub fn client(&self) -> &Arc<Client> {
        &self.client
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
    pub fn hooks(&self) -> &Arc<HookManager> {
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
