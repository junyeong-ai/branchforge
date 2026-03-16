//! Agent core structure and construction.

use std::sync::Arc;

use tokio::sync::RwLock;

use super::config::AgentConfig;
use crate::Client;
use crate::authorization::ExecutionMode;
use crate::budget::{BudgetTracker, TenantBudget};
use crate::context::PromptOrchestrator;
use crate::events::EventBus;
use crate::hooks::HookManager;
use crate::session::{SessionAccessScope, SessionManager, ToolState};
use crate::tools::{ToolRegistry, ToolSearchManager};
use crate::types::Message;

pub struct Agent {
    pub(crate) client: Arc<Client>,
    pub(crate) config: Arc<AgentConfig>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) hooks: Arc<HookManager>,
    pub(crate) session_id: Arc<str>,
    pub(crate) state: ToolState,
    pub(crate) orchestrator: Option<Arc<RwLock<PromptOrchestrator>>>,
    pub(crate) initial_messages: Option<Vec<Message>>,
    pub(crate) budget_tracker: Arc<BudgetTracker>,
    pub(crate) tenant_budget: Option<Arc<TenantBudget>>,
    pub(crate) mcp_manager: Option<Arc<crate::mcp::McpManager>>,
    pub(crate) tool_search_manager: Option<Arc<ToolSearchManager>>,
    pub(crate) session_manager: Option<SessionManager>,
    pub(crate) session_scope: Option<SessionAccessScope>,
    pub(crate) event_bus: Option<Arc<EventBus>>,
    pub(crate) execution_mode: ExecutionMode,
}

impl Agent {
    pub fn new(client: Client, mut config: AgentConfig) -> Self {
        let model_config = &client.config().models;
        let resolved_primary = model_config.resolve_alias(&config.model.primary);
        if resolved_primary != config.model.primary {
            config.model.primary = resolved_primary.to_string();
        }
        let resolved_small = model_config.resolve_alias(&config.model.small);
        if resolved_small != config.model.small {
            config.model.small = resolved_small.to_string();
        }

        let tools = ToolRegistry::default_tools(
            config.security.tool_surface.clone(),
            config.working_dir.clone(),
            Some(config.security.authorization_policy.clone()),
        );
        Self::from_parts(
            Arc::new(client),
            Arc::new(config),
            Arc::new(tools),
            Arc::new(HookManager::new()),
            None,
        )
    }

    pub(crate) fn from_orchestrator(
        client: Client,
        config: AgentConfig,
        tools: Arc<ToolRegistry>,
        hooks: HookManager,
        orchestrator: PromptOrchestrator,
    ) -> Self {
        Self::from_parts(
            Arc::new(client),
            Arc::new(config),
            tools,
            Arc::new(hooks),
            Some(Arc::new(RwLock::new(orchestrator))),
        )
    }

    pub(crate) fn from_parts(
        client: Arc<Client>,
        config: Arc<AgentConfig>,
        tools: Arc<ToolRegistry>,
        hooks: Arc<HookManager>,
        orchestrator: Option<Arc<RwLock<PromptOrchestrator>>>,
    ) -> Self {
        let budget_tracker = match config.budget.max_cost_usd {
            Some(max) => BudgetTracker::new(max),
            None => BudgetTracker::unlimited(),
        };

        let state = tools
            .tool_state()
            .cloned()
            .unwrap_or_else(|| ToolState::new(crate::session::SessionId::new()));
        let session_id: Arc<str> = state.session_id().to_string().into();

        Self {
            client,
            config,
            tools,
            hooks,
            session_id,
            state,
            orchestrator,
            initial_messages: None,
            budget_tracker: Arc::new(budget_tracker),
            tenant_budget: None,
            mcp_manager: None,
            tool_search_manager: None,
            session_manager: None,
            session_scope: None,
            event_bus: None,
            execution_mode: ExecutionMode::Auto,
        }
    }

    pub(crate) fn tenant_budget(mut self, budget: Arc<TenantBudget>) -> Self {
        self.tenant_budget = Some(budget);
        self
    }

    pub(crate) fn mcp_manager(mut self, manager: Arc<crate::mcp::McpManager>) -> Self {
        self.mcp_manager = Some(manager);
        self
    }

    pub(crate) fn tool_search_manager(mut self, manager: Arc<ToolSearchManager>) -> Self {
        self.tool_search_manager = Some(manager);
        self
    }

    pub(crate) fn session_persistence(
        mut self,
        manager: SessionManager,
        scope: Option<SessionAccessScope>,
    ) -> Self {
        self.session_manager = Some(manager);
        self.session_scope = scope;
        self
    }

    /// Attach an [`EventBus`] for non-blocking observability events.
    pub fn with_event_bus(mut self, bus: Arc<EventBus>) -> Self {
        self.event_bus = Some(bus);
        self
    }

    /// Returns the event bus, if one was configured.
    #[must_use]
    pub fn event_bus(&self) -> Option<&Arc<EventBus>> {
        self.event_bus.as_ref()
    }

    pub(crate) fn initial_messages(mut self, messages: Vec<Message>) -> Self {
        self.initial_messages = Some(messages);
        self
    }

    pub(crate) fn session_id(mut self, id: impl Into<String>) -> Self {
        self.session_id = id.into().into();
        self
    }

    #[must_use]
    pub fn builder() -> super::AgentBuilder {
        super::AgentBuilder::new()
    }

    /// Shortcut for `Agent::builder().model(model)`.
    pub fn model(model: impl Into<String>) -> super::AgentBuilder {
        super::AgentBuilder::new().model(model)
    }

    pub async fn default_agent() -> crate::Result<Self> {
        Self::builder().build().await
    }

    #[must_use]
    pub fn hooks(&self) -> &Arc<HookManager> {
        &self.hooks
    }

    #[must_use]
    pub fn get_session_id(&self) -> &str {
        &self.session_id
    }

    #[must_use]
    pub fn client(&self) -> &Arc<Client> {
        &self.client
    }

    pub(crate) async fn persist_session_state(&self) -> crate::Result<()> {
        let Some(manager) = self.session_manager.as_ref() else {
            return Ok(());
        };
        let session = self.state.session().await;
        manager
            .persist_snapshot(&session, self.session_scope.as_ref())
            .await
            .map_err(|e| crate::Error::Session(e.to_string()))
    }

    pub fn orchestrator(&self) -> Option<&Arc<RwLock<PromptOrchestrator>>> {
        self.orchestrator.as_ref()
    }

    #[must_use]
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    #[must_use]
    pub fn tools(&self) -> &Arc<ToolRegistry> {
        &self.tools
    }

    #[must_use]
    pub fn state(&self) -> &ToolState {
        &self.state
    }
}
