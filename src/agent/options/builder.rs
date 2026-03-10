//! Fluent builder for configuring and constructing [`crate::Agent`] instances.
//!
//! # Overview
//!
//! `AgentBuilder` provides a chainable API for configuring all aspects of an agent:
//! model selection, tool access, security policies, budget limits, and more.
//!
//! # Example
//!
//! ```rust,no_run
//! use claude_agent::{Agent, ToolAccess, Auth};
//!
//! # async fn example() -> claude_agent::Result<()> {
//! let agent = Agent::builder()
//!     .auth(Auth::from_env()).await?
//!     .model("claude-sonnet-4-5")
//!     .tools(ToolAccess::all())
//!     .working_dir("./project")
//!     .max_iterations(50)
//!     .build()
//!     .await?;
//! # Ok(())
//! # }
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;

use crate::auth::{Credential, OAuthConfig};
use crate::budget::TenantBudgetManager;
use crate::client::{CloudProvider, FallbackConfig, ModelConfig, ProviderConfig};
use crate::common::IndexRegistry;
use crate::context::{LeveledMemoryProvider, RuleIndex};
use crate::hooks::{Hook, HookManager};
use crate::output_style::OutputStyle;
use crate::permissions::{PermissionMode, PermissionPolicy, PermissionRule};
use crate::skills::SkillIndex;
use crate::subagents::{SubagentIndex, builtin_subagents};
use crate::tools::{Tool, ToolAccess};

use crate::agent::config::{AgentConfig, CacheConfig, SystemPromptMode};

/// Default number of messages to preserve during context compaction.
pub const DEFAULT_COMPACT_KEEP_MESSAGES: usize = 4;

/// Fluent builder for constructing [`crate::Agent`] instances with custom configuration.
///
/// Use [`crate::Agent::builder()`] to create a new builder instance.
#[derive(Default)]
pub struct AgentBuilder {
    pub(super) config: AgentConfig,
    pub(super) credential: Option<Credential>,
    pub(super) auth_type: Option<crate::auth::Auth>,
    pub(super) oauth_config: Option<OAuthConfig>,
    pub(super) cloud_provider: Option<CloudProvider>,
    pub(super) model_config: Option<ModelConfig>,
    pub(super) provider_config: Option<ProviderConfig>,
    pub(super) skill_registry: Option<IndexRegistry<SkillIndex>>,
    pub(super) subagent_registry: Option<IndexRegistry<SubagentIndex>>,
    pub(super) rule_indices: Vec<RuleIndex>,
    pub(super) hooks: HookManager,
    pub(super) custom_tools: Vec<Arc<dyn Tool>>,
    pub(super) memory_provider: Option<LeveledMemoryProvider>,
    pub(super) sandbox_settings: Option<crate::config::SandboxSettings>,
    pub(super) initial_messages: Option<Vec<crate::types::Message>>,
    pub(super) resume_session_id: Option<String>,
    pub(super) resumed_session: Option<crate::session::Session>,
    pub(super) tenant_budget_manager: Option<TenantBudgetManager>,
    pub(super) fallback_config: Option<FallbackConfig>,
    pub(super) output_style_name: Option<String>,
    pub(super) mcp_configs: std::collections::HashMap<String, crate::mcp::McpServerConfig>,
    pub(super) mcp_manager: Option<std::sync::Arc<crate::mcp::McpManager>>,
    pub(super) mcp_toolset_registry: Option<crate::mcp::McpToolsetRegistry>,
    pub(super) tool_search_config: Option<crate::tools::ToolSearchConfig>,
    pub(super) tool_search_manager: Option<std::sync::Arc<crate::tools::ToolSearchManager>>,
    pub(super) session_manager: Option<crate::session::SessionManager>,

    // Resource level flags - loaded in fixed order during build()
    // Order: Enterprise → User → Project → Local (later overrides earlier)
    pub(super) load_enterprise: bool,
    pub(super) load_user: bool,
    pub(super) load_project: bool,
    pub(super) load_local: bool,

    #[cfg(feature = "aws")]
    pub(super) aws_region: Option<String>,
    #[cfg(feature = "gcp")]
    pub(super) gcp_project: Option<String>,
    #[cfg(feature = "gcp")]
    pub(super) gcp_region: Option<String>,
    #[cfg(feature = "azure")]
    pub(super) azure_resource: Option<String>,

    #[cfg(feature = "plugins")]
    pub(super) plugin_dirs: Vec<PathBuf>,
}

impl AgentBuilder {
    /// Creates a new builder with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    // =========================================================================
    // Configuration
    // =========================================================================

    /// Sets the complete agent configuration, replacing all defaults.
    pub fn agent_config(mut self, config: AgentConfig) -> Self {
        self.config = config;
        self
    }

    /// Sets the API provider configuration (timeouts, beta features, etc.).
    pub fn provider_config(mut self, config: ProviderConfig) -> Self {
        self.provider_config = Some(config);
        self
    }

    // =========================================================================
    // Authentication
    // =========================================================================

    /// Configures authentication for the API.
    ///
    /// # Supported Methods
    /// - `Auth::from_env()` - Uses `ANTHROPIC_API_KEY` environment variable
    /// - `Auth::api_key("sk-...")` - Explicit API key
    /// - `Auth::claude_cli()` - Uses Claude CLI OAuth (requires `cli-integration` feature)
    /// - `Auth::bedrock("region")` - AWS Bedrock (requires `aws` feature)
    /// - `Auth::vertex("project", "region")` - GCP Vertex AI (requires `gcp` feature)
    ///
    /// # Example
    /// ```rust,no_run
    /// # use claude_agent::{Agent, Auth};
    /// # async fn example() -> claude_agent::Result<()> {
    /// let agent = Agent::builder()
    ///     .auth(Auth::from_env()).await?
    ///     .build().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn auth(mut self, auth: impl Into<crate::auth::Auth>) -> crate::Result<Self> {
        let auth = auth.into();

        #[allow(unreachable_patterns)]
        match &auth {
            #[cfg(feature = "aws")]
            crate::auth::Auth::Bedrock { region } => {
                self.cloud_provider = Some(CloudProvider::Bedrock);
                self.aws_region = Some(region.clone());
                self.model_config = Some(ModelConfig::bedrock());
                self = self.apply_provider_models();
            }
            #[cfg(feature = "gcp")]
            crate::auth::Auth::Vertex { project, region } => {
                self.cloud_provider = Some(CloudProvider::Vertex);
                self.gcp_project = Some(project.clone());
                self.gcp_region = Some(region.clone());
                self.model_config = Some(ModelConfig::vertex());
                self = self.apply_provider_models();
            }
            #[cfg(feature = "azure")]
            crate::auth::Auth::Foundry { resource } => {
                self.cloud_provider = Some(CloudProvider::Foundry);
                self.azure_resource = Some(resource.clone());
                self.model_config = Some(ModelConfig::foundry());
                self = self.apply_provider_models();
            }
            _ => {}
        }

        let credential = auth.resolve().await?;
        if !credential.is_placeholder() {
            self.credential = Some(credential);
        }

        self.auth_type = Some(auth);

        if self.supports_server_tools() {
            self.config.server_tools = crate::agent::config::ServerToolsConfig::all();
        }

        Ok(self)
    }

    /// Sets OAuth configuration for token refresh.
    pub fn oauth_config(mut self, config: OAuthConfig) -> Self {
        self.oauth_config = Some(config);
        self
    }

    /// Returns whether server-side tools should be enabled.
    ///
    /// Requires both auth support and ToolAccess allowing "WebSearch" or "WebFetch".
    pub fn supports_server_tools(&self) -> bool {
        let auth_supports = self
            .auth_type
            .as_ref()
            .map(|a| a.supports_server_tools())
            .unwrap_or(true);

        let access_allows = self.config.security.tool_access.is_allowed("WebSearch")
            || self.config.security.tool_access.is_allowed("WebFetch");

        auth_supports && access_allows
    }

    // =========================================================================
    // Model Configuration
    // =========================================================================

    /// Sets both primary and small model configurations.
    pub fn models(mut self, config: ModelConfig) -> Self {
        self.model_config = Some(config.clone());
        self.config.model.primary = config.primary;
        self.config.model.small = config.small;
        self
    }

    #[cfg(any(feature = "aws", feature = "gcp", feature = "azure"))]
    fn apply_provider_models(mut self) -> Self {
        if let Some(ref config) = self.model_config {
            if self.config.model.primary
                == crate::agent::config::AgentModelConfig::default().primary
            {
                self.config.model.primary = config.primary.clone();
            }
            if self.config.model.small == crate::agent::config::AgentModelConfig::default().small {
                self.config.model.small = config.small.clone();
            }
        }
        self
    }

    /// Sets the primary model for main operations.
    ///
    /// Default: `claude-sonnet-4-5-20250514`
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.config.model.primary = model.into();
        self
    }

    /// Sets the smaller model for quick operations (e.g., subagents).
    ///
    /// Default: `claude-haiku-4-5-20251001`
    pub fn small_model(mut self, model: impl Into<String>) -> Self {
        self.config.model.small = model.into();
        self
    }

    /// Sets the maximum tokens per response.
    ///
    /// Default: 8192. Values exceeding this require the 128k beta feature,
    /// which is automatically enabled when using `ProviderConfig::with_max_tokens`.
    pub fn max_tokens(mut self, tokens: u32) -> Self {
        self.config.model.max_tokens = tokens;
        self
    }

    /// Enables extended context window (1M tokens for supported models).
    ///
    /// Requires the `context-1m-2025-08-07` beta feature.
    /// Currently supported: `claude-sonnet-4-5-20250929`
    pub fn extended_context(mut self, enabled: bool) -> Self {
        self.config.model.extended_context = enabled;
        self
    }

    // =========================================================================
    // Tools
    // =========================================================================

    /// Sets tool access policy.
    ///
    /// # Options
    /// - `ToolAccess::all()` - Enable all built-in tools
    /// - `ToolAccess::none()` - Disable all tools
    /// - `ToolAccess::only(["Read", "Write"])` - Enable specific tools
    /// - `ToolAccess::except(["Bash"])` - Enable all except specific tools
    pub fn tools(mut self, access: ToolAccess) -> Self {
        self.config.security.tool_access = access;
        self
    }

    /// Registers a custom tool implementation.
    pub fn tool<T: Tool + 'static>(mut self, tool: T) -> Self {
        self.custom_tools.push(Arc::new(tool));
        self
    }

    // =========================================================================
    // Execution
    // =========================================================================

    /// Sets the working directory for file operations.
    pub fn working_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.working_dir = Some(path.into());
        self
    }

    /// Sets the maximum number of agentic loop iterations.
    ///
    /// Default: `100`
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.config.execution.max_iterations = max;
        self
    }

    /// Sets the overall execution timeout.
    ///
    /// Default: `300 seconds`
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.config.execution.timeout = Some(timeout);
        self
    }

    /// Sets the timeout between streaming chunks.
    ///
    /// This timeout detects stalled connections when no data is received
    /// for the specified duration during streaming responses.
    ///
    /// Default: `60 seconds`
    ///
    /// For large projects or slow network conditions, consider increasing
    /// this value (e.g., 180 seconds).
    pub fn chunk_timeout(mut self, timeout: Duration) -> Self {
        self.config.execution.chunk_timeout = timeout;
        self
    }

    /// Enables or disables automatic context compaction.
    ///
    /// Default: `true`
    pub fn auto_compact(mut self, enabled: bool) -> Self {
        self.config.execution.auto_compact = enabled;
        self
    }

    // =========================================================================
    // Caching
    // =========================================================================

    /// Configures prompt caching strategy.
    ///
    /// # Options
    /// - `CacheConfig::default()` - Static, tools, and conversation
    /// - `CacheConfig::static_only()` - Static context only (1h TTL)
    /// - `CacheConfig::tools_only()` - Tool metadata only (1h TTL)
    /// - `CacheConfig::conversation_only()` - Conversation only (5m TTL)
    /// - `CacheConfig::disabled()` - No caching
    pub fn cache(mut self, config: CacheConfig) -> Self {
        self.config.cache = config;
        self
    }

    // =========================================================================
    // Prompts
    // =========================================================================

    /// Sets a custom system prompt, replacing the default.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.config.prompt.system_prompt = Some(prompt.into());
        self
    }

    /// Sets how the system prompt is applied.
    pub fn system_prompt_mode(mut self, mode: SystemPromptMode) -> Self {
        self.config.prompt.system_prompt_mode = mode;
        self
    }

    /// Appends to the default system prompt instead of replacing it.
    pub fn append_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.config.prompt.system_prompt_mode = SystemPromptMode::Append;
        self.config.prompt.system_prompt = Some(prompt.into());
        self
    }

    /// Sets the output style for response formatting.
    pub fn output_style(mut self, style: OutputStyle) -> Self {
        self.config.prompt.output_style = Some(style);
        self
    }

    /// Sets the output style by name (loaded from configuration).
    pub fn output_style_name(mut self, name: impl Into<String>) -> Self {
        self.output_style_name = Some(name.into());
        self
    }

    /// Sets a JSON schema for structured output.
    pub fn output_schema(mut self, schema: serde_json::Value) -> Self {
        self.config.prompt.output_schema = Some(schema);
        self
    }

    /// Enables structured output with automatic schema generation.
    pub fn structured_output<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.config.prompt.output_schema = serde_json::to_value(schema).ok();
        self
    }

    // =========================================================================
    // Permissions
    // =========================================================================

    /// Sets the complete permission policy.
    pub fn permission_policy(mut self, policy: PermissionPolicy) -> Self {
        self.config.security.permission_policy = policy;
        self
    }

    /// Sets the permission mode (permissive, default, or strict).
    pub fn permission_mode(mut self, mode: PermissionMode) -> Self {
        self.config.security.permission_policy.mode = mode;
        self
    }

    /// Adds a rule to allow a tool or pattern (e.g., `"Read"` or `"Bash(git:*)"`)
    pub fn allow_tool(mut self, pattern: impl Into<String>) -> Self {
        self.config
            .security
            .permission_policy
            .rules
            .push(PermissionRule::allow_pattern(pattern));
        self
    }

    /// Adds a rule to deny a tool or pattern (e.g., `"Write"` or `"Bash(rm:*)"`)
    pub fn deny_tool(mut self, pattern: impl Into<String>) -> Self {
        self.config
            .security
            .permission_policy
            .rules
            .push(PermissionRule::deny_pattern(pattern));
        self
    }

    // =========================================================================
    // Environment
    // =========================================================================

    /// Sets an environment variable for tool execution.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.security.env.insert(key.into(), value.into());
        self
    }

    /// Sets multiple environment variables for tool execution.
    pub fn envs(
        mut self,
        vars: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        for (k, v) in vars {
            self.config.security.env.insert(k.into(), v.into());
        }
        self
    }

    // =========================================================================
    // Sandbox & Network
    // =========================================================================

    /// Adds a domain to the network allowlist.
    pub fn allow_domain(mut self, domain: impl Into<String>) -> Self {
        self.sandbox_settings
            .get_or_insert_with(crate::config::SandboxSettings::default)
            .network
            .allowed_domains
            .insert(domain.into());
        self
    }

    /// Adds a domain to the network blocklist.
    pub fn deny_domain(mut self, domain: impl Into<String>) -> Self {
        self.sandbox_settings
            .get_or_insert_with(crate::config::SandboxSettings::default)
            .network
            .blocked_domains
            .insert(domain.into());
        self
    }

    /// Enables or disables sandbox isolation.
    pub fn sandbox_enabled(mut self, enabled: bool) -> Self {
        self.sandbox_settings
            .get_or_insert_with(crate::config::SandboxSettings::default)
            .enabled = enabled;
        self
    }

    /// Excludes a command from sandbox restrictions.
    pub fn exclude_command(mut self, command: impl Into<String>) -> Self {
        self.sandbox_settings
            .get_or_insert_with(crate::config::SandboxSettings::default)
            .excluded_commands
            .push(command.into());
        self
    }

    // =========================================================================
    // Budget
    // =========================================================================

    /// Sets the maximum budget in USD.
    pub fn max_budget_usd(mut self, amount: Decimal) -> Self {
        self.config.budget.max_cost_usd = Some(amount);
        self
    }

    /// Sets the tenant ID for multi-tenant budget tracking.
    pub fn tenant_id(mut self, id: impl Into<String>) -> Self {
        self.config.identity.tenant_id = Some(id.into());
        self
    }

    /// Sets the principal ID for session ownership and request metadata.
    pub fn principal_id(mut self, id: impl Into<String>) -> Self {
        self.config.identity.principal_id = Some(id.into());
        self
    }

    /// Sets a shared tenant budget manager.
    pub fn tenant_budget_manager(mut self, manager: TenantBudgetManager) -> Self {
        self.tenant_budget_manager = Some(manager);
        self
    }

    /// Sets the model to fall back to when budget is exceeded.
    pub fn fallback_model(mut self, model: impl Into<String>) -> Self {
        self.config.budget.fallback_model = Some(model.into());
        self
    }

    /// Sets the complete fallback configuration.
    pub fn fallback(mut self, config: FallbackConfig) -> Self {
        self.fallback_config = Some(config);
        self
    }

    // =========================================================================
    // Session
    // =========================================================================

    /// Sets a custom session manager for persistence.
    pub fn session_manager(mut self, manager: crate::session::SessionManager) -> Self {
        self.session_manager = Some(manager);
        self
    }

    /// Forks an existing session, creating a new branch.
    pub async fn fork_session(mut self, session_id: impl Into<String>) -> crate::Result<Self> {
        let manager = self.session_manager.take().unwrap_or_default();
        let session_id_str: String = session_id.into();
        let original_id = crate::session::SessionId::from(session_id_str);
        let forked = manager
            .fork(&original_id)
            .await
            .map_err(|e| crate::Error::Session(e.to_string()))?;

        self.initial_messages = Some(forked.to_api_messages());
        self.resume_session_id = Some(forked.id.to_string());
        self.session_manager = Some(manager);
        Ok(self)
    }

    pub async fn fork_session_from_node(
        mut self,
        session_id: impl Into<String>,
        from_node: crate::graph::NodeId,
    ) -> crate::Result<Self> {
        let manager = self.session_manager.take().unwrap_or_default();
        let original_id = crate::session::SessionId::from(session_id.into());
        let forked = manager
            .fork_from_node(&original_id, from_node)
            .await
            .map_err(|e| crate::Error::Session(e.to_string()))?;

        self.initial_messages = Some(forked.to_api_messages());
        self.resume_session_id = Some(forked.id.to_string());
        self.session_manager = Some(manager);
        Ok(self)
    }

    /// Resumes an existing session by ID.
    pub async fn resume_session(mut self, session_id: impl Into<String>) -> crate::Result<Self> {
        let session_id_str: String = session_id.into();
        let id = crate::session::SessionId::from(session_id_str);
        let manager = self.session_manager.take().unwrap_or_default();
        let session = manager.get(&id).await?;

        let messages: Vec<crate::types::Message> = session
            .messages
            .iter()
            .map(|m| crate::types::Message {
                role: m.role,
                content: m.content.clone(),
            })
            .collect();

        self.initial_messages = Some(messages);
        self.resume_session_id = Some(id.to_string());
        self.resumed_session = Some(session);
        self.session_manager = Some(manager);
        Ok(self)
    }

    pub async fn resume_session_from_node(
        mut self,
        session_id: impl Into<String>,
        from_node: crate::graph::NodeId,
    ) -> crate::Result<Self> {
        let id = crate::session::SessionId::from(session_id.into());
        let manager = self.session_manager.take().unwrap_or_default();
        let session = manager.get(&id).await?;
        let replay = session.replay_input(Some(from_node));

        self.initial_messages = Some(replay.messages);
        self.resume_session_id = Some(id.to_string());
        self.resumed_session = Some(session);
        self.session_manager = Some(manager);
        Ok(self)
    }

    /// Sets initial messages for the conversation.
    pub fn messages(mut self, messages: Vec<crate::types::Message>) -> Self {
        self.initial_messages = Some(messages);
        self
    }

    // =========================================================================
    // MCP (Model Context Protocol)
    // =========================================================================

    /// Adds an MCP server configuration.
    pub fn mcp_server(
        mut self,
        name: impl Into<String>,
        config: crate::mcp::McpServerConfig,
    ) -> Self {
        self.mcp_configs.insert(name.into(), config);
        self
    }

    /// Adds an MCP server using stdio transport.
    pub fn mcp_stdio(
        mut self,
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
    ) -> Self {
        self.mcp_configs.insert(
            name.into(),
            crate::mcp::McpServerConfig::Stdio {
                command: command.into(),
                args,
                env: std::collections::HashMap::new(),
                cwd: None,
            },
        );
        self
    }

    /// Sets an owned MCP manager.
    pub fn mcp_manager(mut self, manager: crate::mcp::McpManager) -> Self {
        self.mcp_manager = Some(std::sync::Arc::new(manager));
        self
    }

    /// Sets a shared MCP manager (for multi-agent scenarios).
    pub fn shared_mcp_manager(mut self, manager: std::sync::Arc<crate::mcp::McpManager>) -> Self {
        self.mcp_manager = Some(manager);
        self
    }

    /// Registers an MCP toolset configuration for deferred loading.
    pub fn mcp_toolset(mut self, toolset: crate::mcp::McpToolset) -> Self {
        self.mcp_toolset_registry
            .get_or_insert_with(crate::mcp::McpToolsetRegistry::new)
            .register(toolset);
        self
    }

    // =========================================================================
    // Tool Search
    // =========================================================================

    /// Enables tool search with default configuration.
    pub fn tool_search(mut self) -> Self {
        self.tool_search_config = Some(crate::tools::ToolSearchConfig::default());
        self
    }

    /// Sets the tool search configuration.
    pub fn tool_search_config(mut self, config: crate::tools::ToolSearchConfig) -> Self {
        self.tool_search_config = Some(config);
        self
    }

    /// Sets the tool search threshold as a fraction of context window (0.0 - 1.0).
    pub fn tool_search_threshold(mut self, threshold: f64) -> Self {
        let config = self
            .tool_search_config
            .get_or_insert_with(crate::tools::ToolSearchConfig::default);
        config.threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Sets the search mode for tool search.
    pub fn tool_search_mode(mut self, mode: crate::tools::SearchMode) -> Self {
        let config = self
            .tool_search_config
            .get_or_insert_with(crate::tools::ToolSearchConfig::default);
        config.search_mode = mode;
        self
    }

    /// Sets tools that should always be loaded immediately (never deferred).
    pub fn always_load_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let config = self
            .tool_search_config
            .get_or_insert_with(crate::tools::ToolSearchConfig::default);
        config.always_load = tools.into_iter().map(Into::into).collect();
        self
    }

    /// Sets a shared tool search manager.
    pub fn shared_tool_search_manager(
        mut self,
        manager: std::sync::Arc<crate::tools::ToolSearchManager>,
    ) -> Self {
        self.tool_search_manager = Some(manager);
        self
    }

    // =========================================================================
    // Skills
    // =========================================================================

    /// Sets a complete skill registry.
    pub fn skill_registry(mut self, registry: IndexRegistry<SkillIndex>) -> Self {
        self.skill_registry = Some(registry);
        self
    }

    /// Registers a single skill index.
    pub fn skill(mut self, skill: SkillIndex) -> Self {
        self.skill_registry
            .get_or_insert_with(IndexRegistry::new)
            .register(skill);
        self
    }

    /// Adds a rule index for rule discovery.
    pub fn rule_index(mut self, index: RuleIndex) -> Self {
        self.rule_indices.push(index);
        self
    }

    /// Adds memory content (CLAUDE.md style).
    pub fn memory_content(mut self, content: impl Into<String>) -> Self {
        self.memory_provider
            .get_or_insert_with(LeveledMemoryProvider::new)
            .add_content(content);
        self
    }

    /// Adds local memory content (CLAUDE.local.md style).
    pub fn local_memory_content(mut self, content: impl Into<String>) -> Self {
        self.memory_provider
            .get_or_insert_with(LeveledMemoryProvider::new)
            .add_local_content(content);
        self
    }

    // =========================================================================
    // Subagents
    // =========================================================================

    /// Sets a complete subagent registry.
    pub fn subagent_registry(mut self, registry: IndexRegistry<SubagentIndex>) -> Self {
        self.subagent_registry = Some(registry);
        self
    }

    /// Registers a single subagent.
    pub fn subagent(mut self, subagent: SubagentIndex) -> Self {
        self.subagent_registry
            .get_or_insert_with(|| {
                let mut registry = IndexRegistry::new();
                registry.register_all(builtin_subagents());
                registry
            })
            .register(subagent);
        self
    }

    // =========================================================================
    // Plugins
    // =========================================================================

    /// Adds a plugin directory to discover and load plugins from.
    ///
    /// Each plugin requires a `.claude-plugin/plugin.json` manifest.
    /// If `dir` is a plugin root, it is loaded directly.
    /// Otherwise, child directories with manifests are discovered automatically.
    #[cfg(feature = "plugins")]
    pub fn plugin_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.plugin_dirs.push(dir.into());
        self
    }

    /// Adds multiple plugin directories.
    #[cfg(feature = "plugins")]
    pub fn plugin_dirs(mut self, dirs: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        self.plugin_dirs.extend(dirs.into_iter().map(Into::into));
        self
    }

    // =========================================================================
    // Hooks
    // =========================================================================

    /// Registers an event hook.
    pub fn hook<H: Hook + 'static>(mut self, hook: H) -> Self {
        self.hooks.register(hook);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::DEFAULT_MAX_TOKENS;
    use crate::session::{SessionConfig, SessionManager, SessionMessage};
    use crate::types::ContentBlock;

    #[test]
    fn test_tool_access() {
        assert!(ToolAccess::all().is_allowed("Read"));
        assert!(!ToolAccess::none().is_allowed("Read"));
        assert!(ToolAccess::only(["Read", "Write"]).is_allowed("Read"));
        assert!(!ToolAccess::only(["Read", "Write"]).is_allowed("Bash"));
        assert!(!ToolAccess::except(["Bash"]).is_allowed("Bash"));
        assert!(ToolAccess::except(["Bash"]).is_allowed("Read"));
    }

    #[test]
    fn test_max_tokens_default() {
        let builder = AgentBuilder::new();
        assert_eq!(builder.config.model.max_tokens, DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn test_max_tokens_custom() {
        let builder = AgentBuilder::new().max_tokens(16384);
        assert_eq!(builder.config.model.max_tokens, 16384);
    }

    #[tokio::test]
    async fn test_resume_and_fork_from_node() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        let session_id = session.id;

        manager
            .add_message(
                &session_id,
                SessionMessage::user(vec![ContentBlock::text("one")]),
            )
            .await
            .unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::assistant(vec![ContentBlock::text("two")]),
            )
            .await
            .unwrap();

        let loaded = manager.get(&session_id).await.unwrap();
        let from_node = loaded
            .graph
            .branch_head(loaded.graph.primary_branch)
            .unwrap();

        let resumed = AgentBuilder::new()
            .session_manager(manager.clone())
            .resume_session_from_node(session_id.to_string(), from_node)
            .await
            .unwrap();
        let forked = AgentBuilder::new()
            .session_manager(manager)
            .fork_session_from_node(session_id.to_string(), from_node)
            .await
            .unwrap();

        assert!(
            resumed
                .initial_messages
                .as_ref()
                .is_some_and(|messages| messages.len() == 1)
        );
        assert!(
            forked
                .initial_messages
                .as_ref()
                .is_some_and(|messages| messages.len() == 1)
        );
    }
}
