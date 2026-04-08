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
//! use branchforge::{Agent, Auth, ToolSurface};
//!
//! # async fn example() -> branchforge::Result<()> {
//! let agent = Agent::builder()
//!     .auth(Auth::from_env()).await?
//!     .model("claude-sonnet-4-5")
//!     .tools(ToolSurface::core())
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

use crate::agent::{CloudProvider, ModelConfig, ProviderConfig};
use crate::auth::{Credential, OAuthConfig};
use crate::authorization::{ExecutionMode, ToolPolicy, ToolRule};
use crate::budget::TenantBudgetManager;
use crate::client::FallbackConfig;
use crate::common::IndexRegistry;
use crate::context::{LeveledMemoryProvider, RuleIndex};
use crate::hooks::{Hook, HookManager};
use crate::output_style::OutputStyle;
use crate::skills::SkillIndex;
use crate::subagents::{SubagentIndex, builtin_subagents};
use crate::tools::{Tool, ToolSurface};

use crate::agent::config::{AgentConfig, CacheConfig, SystemPromptMode};

/// Default number of messages to preserve during context compaction.
pub const DEFAULT_COMPACT_KEEP_MESSAGES: usize = 4;

/// Fluent builder for constructing [`crate::Agent`] instances with custom configuration.
///
/// Use [`crate::Agent::builder()`] to create a new builder instance.
#[derive(Default)]
/// Builder for [`Agent`] construction.
///
/// Fields are organised by domain so the builder API surface stays
/// navigable. The grouping is documentary — Rust struct fields are flat
/// — but every builder method (`with_*`, `enable_*`, …) below sets a
/// field belonging to one of these groups. Reading the groups in order
/// roughly mirrors the order in which `build()` consumes them.
///
/// Groups: agent core configuration, auth & provider selection, resource
/// catalogues, hooks & policies, MCP configuration, tool search, session
/// & orchestration, resource-level loading flags, cloud-provider hints,
/// plugins, and the pre-built provider client.
pub struct AgentBuilder {
    // ── Agent core configuration ─────────────────────────────────────
    pub(super) config: AgentConfig,

    // ── Auth & provider selection ────────────────────────────────────
    pub(super) credential: Option<Credential>,
    pub(super) auth_type: Option<crate::auth::Auth>,
    pub(super) oauth_config: Option<OAuthConfig>,
    pub(super) cloud_provider: Option<CloudProvider>,
    pub(super) model_config: Option<ModelConfig>,
    pub(super) provider_config: Option<ProviderConfig>,
    pub(super) fallback_config: Option<FallbackConfig>,
    /// Pre-built [`ProviderClient`] from the codec/transport stack.
    /// When set, the agent runtime dispatches LLM calls through this
    /// directly instead of resolving a preset from environment variables.
    pub(super) provider_client: Option<crate::client::provider_client::ProviderClient>,

    // ── Resource catalogues (skills, subagents, rules, memory) ───────
    pub(super) skill_registry: Option<IndexRegistry<SkillIndex>>,
    pub(super) subagent_registry: Option<IndexRegistry<SubagentIndex>>,
    pub(super) rule_indices: Vec<RuleIndex>,
    pub(super) memory_provider: Option<LeveledMemoryProvider>,
    pub(super) output_style_name: Option<String>,

    // ── Hooks, policies, and execution control ───────────────────────
    pub(super) hooks: HookManager,
    pub(super) execution_mode: ExecutionMode,
    pub(super) custom_tools: Vec<Arc<dyn Tool>>,
    pub(super) sandbox_settings: Option<crate::config::SandboxConfig>,
    pub(super) authorization_policy_explicit: bool,
    pub(super) tenant_budget_manager: Option<TenantBudgetManager>,

    // ── MCP configuration ────────────────────────────────────────────
    pub(super) mcp_configs: std::collections::HashMap<String, crate::mcp::McpServerConfig>,
    pub(super) mcp_manager: Option<std::sync::Arc<crate::mcp::McpManager>>,
    pub(super) mcp_toolset_registry: Option<crate::mcp::McpToolsetRegistry>,

    // ── Tool search ──────────────────────────────────────────────────
    pub(super) tool_search_config: Option<crate::tools::ToolSearchConfig>,
    pub(super) tool_search_manager: Option<std::sync::Arc<crate::tools::ToolSearchManager>>,

    // ── Session & orchestration ──────────────────────────────────────
    pub(super) session_manager: Option<crate::session::SessionManager>,
    pub(super) context_scope: Option<crate::context_scope::SharedContextScope>,
    pub(super) compaction_chain: Option<std::sync::Arc<crate::session::compact::CompactionChain>>,
    pub(super) coordination: Option<std::sync::Arc<dyn crate::orchestration::Coordination>>,
    pub(super) recovery_strategy:
        Option<std::sync::Arc<dyn crate::session::compact::recovery::RecoveryStrategy>>,
    pub(super) initial_messages: Option<Vec<crate::ir::Message>>,
    pub(super) resume_session_id: Option<String>,
    pub(super) resumed_session: Option<crate::session::Session>,

    // ── Resource-level loading flags ─────────────────────────────────
    // Order of precedence inside `build()`:
    //   Enterprise → User → Project → Local (later overrides earlier).
    pub(super) load_enterprise: bool,
    pub(super) load_user: bool,
    pub(super) load_project: bool,
    pub(super) load_local: bool,

    // ── Cloud provider hints (feature-gated) ─────────────────────────
    #[cfg(feature = "aws")]
    pub(super) aws_region: Option<String>,
    #[cfg(feature = "gcp")]
    pub(super) gcp_project: Option<String>,
    #[cfg(feature = "gcp")]
    pub(super) gcp_region: Option<String>,
    #[cfg(feature = "azure")]
    pub(super) azure_resource: Option<String>,

    // ── Plugins (feature-gated) ──────────────────────────────────────
    #[cfg(feature = "plugins")]
    pub(super) plugin_dirs: Vec<PathBuf>,
}

impl AgentBuilder {
    pub(super) fn tool_policy_is_custom(policy: &ToolPolicy) -> bool {
        !policy.rules.is_empty() || !policy.tool_limits.is_empty()
    }

    fn parse_session_id(
        value: impl AsRef<str>,
        operation: &str,
    ) -> crate::Result<crate::session::SessionId> {
        let value = value.as_ref();
        crate::session::SessionId::parse(value).ok_or_else(|| {
            crate::Error::Session(crate::session::SessionError::InvalidId {
                value: format!("{operation}: {value}"),
            })
        })
    }

    /// Creates a new builder with default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a pre-built [`crate::ProviderClient`] from the new
    /// codec/transport stack. When set, the agent dispatches all LLM calls
    /// through it instead of the old `ProviderAdapter`. This is the
    /// recommended way to use the new multi-provider stack.
    ///
    /// ```rust,no_run
    /// # async fn example() -> branchforge::Result<()> {
    /// let pc = branchforge::Preset::VertexGemini.build_from_env().await?;
    /// let agent = branchforge::Agent::builder()
    ///     .provider_client(pc)
    ///     .model("gemini-2.5-flash")
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn provider_client(mut self, pc: crate::client::provider_client::ProviderClient) -> Self {
        self.provider_client = Some(pc);
        self
    }

    // =========================================================================
    // Configuration
    // =========================================================================

    /// Sets the complete agent configuration, replacing all defaults.
    pub fn agent_config(mut self, config: AgentConfig) -> Self {
        self.authorization_policy_explicit =
            Self::tool_policy_is_custom(&config.security.authorization_policy);
        self.config = config;
        self
    }

    /// Disables automatic resource loading for delegated runtimes.
    pub fn skip_resource_loading(mut self) -> Self {
        self.load_enterprise = false;
        self.load_user = false;
        self.load_project = false;
        self.load_local = false;
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

    /// `true` if `auth` should map to the direct Anthropic Messages API
    /// (i.e. not a cloud-provider variant). Used by [`Self::auth`] to
    /// decide whether to wire a `ProviderClient` from the resolved
    /// credential rather than fall back to the env-var preset path.
    fn auth_targets_direct_anthropic(auth: &crate::auth::Auth) -> bool {
        #[allow(unreachable_patterns)]
        match auth {
            crate::auth::Auth::ApiKey(_)
            | crate::auth::Auth::FromEnv
            | crate::auth::Auth::OAuth { .. }
            | crate::auth::Auth::Resolved(_) => true,
            #[cfg(feature = "cli-auth")]
            crate::auth::Auth::ClaudeCli => true,
            // Cloud-provider variants are routed via their own preset
            // paths (Bedrock SigV4, Vertex ADC, Foundry Entra) and never
            // resolve to a credential here.
            _ => false,
        }
    }

    /// Build a `ProviderClient` for the direct Anthropic Messages API
    /// from a resolved credential. The transport is wired to the optional
    /// refresh provider so 401s after token expiry can recover without
    /// rebuilding the agent.
    ///
    /// When the credential is an `OAuth` variant (Claude Code CLI), the
    /// transport additionally injects the OAuth-specific headers
    /// (`user-agent`, `x-app`, `anthropic-dangerous-direct-browser-access`,
    /// `anthropic-beta: oauth-2025-04-20,claude-code-20250219`) and the
    /// `?beta=true` URL parameter that the Anthropic API requires to
    /// accept Bearer tokens. Without this, the API rejects OAuth requests
    /// with `"OAuth authentication is currently not supported."`.
    fn build_anthropic_direct_client(
        credential: &Credential,
        refresh_provider: Option<Arc<dyn crate::auth::CredentialProvider>>,
    ) -> crate::Result<crate::client::provider_client::ProviderClient> {
        use crate::auth::{CLAUDE_CODE_BETA, OAuthConfig};
        use crate::client::codec::AnthropicMessagesCodec;
        use crate::client::provider_client::ProviderClient;
        use crate::client::transport::{DirectAuth, DirectTransport};

        let is_oauth = matches!(credential, Credential::OAuth(_));
        let direct_auth = match credential {
            // Anthropic Direct accepts API keys via the `x-api-key` header.
            Credential::ApiKey(secret) => DirectAuth::XApiKey(secret.clone()),
            // OAuth tokens (Claude CLI) ride on `Authorization: Bearer ...`.
            Credential::OAuth(oauth) => DirectAuth::Bearer(oauth.access_token.clone()),
        };

        let base = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".into());
        let mut transport =
            DirectTransport::new(base, direct_auth).with_allowed_codecs(&["anthropic-messages"]);

        if is_oauth {
            // The Anthropic API only accepts Bearer tokens when these
            // headers + URL flag are present together. The `BetaFeature::OAuth`
            // header value is the same `oauth-2025-04-20` constant used by
            // `OAuthConfig::build_beta_header`.
            let cfg = OAuthConfig::default();
            let oauth_beta = crate::agent::BetaFeature::OAuth.header_value();
            let beta_header = format!("{},{}", oauth_beta, CLAUDE_CODE_BETA);
            let mut extra_headers: std::collections::HashMap<String, String> =
                cfg.extra_headers.clone();
            extra_headers.insert("user-agent".to_string(), cfg.user_agent.clone());
            extra_headers.insert("x-app".to_string(), cfg.app_identifier.clone());
            extra_headers.insert("anthropic-beta".to_string(), beta_header);
            transport = transport
                .with_extra_headers(extra_headers)
                .with_extra_url_params(cfg.url_params.clone());
        }

        if let Some(provider) = refresh_provider {
            transport = transport.with_credential_provider(provider);
        }

        let codec =
            Arc::new(AnthropicMessagesCodec::new()) as Arc<dyn crate::client::codec::ModelCodec>;
        let transport = Arc::new(transport) as Arc<dyn crate::client::transport::ModelTransport>;
        ProviderClient::new(codec, transport)
    }

    /// Configures authentication for the API.
    ///
    /// # Supported Methods
    /// - `Auth::from_env()` - Uses `ANTHROPIC_API_KEY` environment variable
    /// - `Auth::api_key("sk-...")` - Explicit API key
    /// - `Auth::claude_cli()` - Uses Claude CLI OAuth (requires `cli-auth` feature)
    /// - `Auth::bedrock("region")` - AWS Bedrock (requires `aws` feature)
    /// - `Auth::vertex("project", "region")` - GCP Vertex AI (requires `gcp` feature)
    ///
    /// # Example
    /// ```rust,no_run
    /// # use branchforge::{Agent, Auth};
    /// # async fn example() -> branchforge::Result<()> {
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
            #[cfg(feature = "openai")]
            crate::auth::Auth::OpenAi { .. } => {
                self.cloud_provider = Some(CloudProvider::OpenAi);
                self.model_config = Some(ModelConfig::openai());
                self = self.apply_provider_models();
            }
            #[cfg(feature = "gemini")]
            crate::auth::Auth::Gemini { .. } => {
                self.cloud_provider = Some(CloudProvider::Gemini);
                self.model_config = Some(ModelConfig::gemini());
                self = self.apply_provider_models();
            }
            _ => {}
        }

        // `resolve_with_provider` returns both the resolved credential and
        // the refresh-capable provider (if any). For OAuth-style auth
        // (Claude CLI), the provider is what wires `DirectTransport::refresh`
        // so 401s recover from token expiry without rebuilding the client.
        let (credential, refresh_provider) = auth.resolve_with_provider().await?;
        if !credential.is_placeholder() {
            self.credential = Some(credential.clone());
        }

        // If the user picked a direct-API auth that resolved to a real
        // credential AND didn't already supply an explicit `provider_client`,
        // build the matching ProviderClient now so `build()` doesn't fall
        // back to the env-var preset path (which would ignore the resolved
        // credential and fail with "ANTHROPIC_API_KEY not set"). Cloud
        // providers (Bedrock/Vertex/Foundry) keep their existing path —
        // they don't resolve through this credential, only the cloud
        // provider routing handled above.
        if self.provider_client.is_none()
            && !credential.is_placeholder()
            && Self::auth_targets_direct_anthropic(&auth)
        {
            self.cloud_provider = Some(CloudProvider::Anthropic);
            self.provider_client = Some(Self::build_anthropic_direct_client(
                &credential,
                refresh_provider,
            )?);
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
    /// Requires both auth support and ToolSurface allowing "WebSearch" or "WebFetch".
    pub fn supports_server_tools(&self) -> bool {
        let auth_supports = self
            .auth_type
            .as_ref()
            .map(|a| a.supports_server_tools())
            .unwrap_or(true);

        let access_allows = self.config.security.tool_surface.is_allowed("WebSearch")
            || self.config.security.tool_surface.is_allowed("WebFetch");

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

    #[cfg(any(
        feature = "aws",
        feature = "gcp",
        feature = "azure",
        feature = "openai",
        feature = "gemini"
    ))]
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
    /// - `ToolSurface::core()` - Enable the minimal core tool surface
    /// - `ToolSurface::all()` - Enable all built-in and workflow tools
    /// - `ToolSurface::none()` - Disable all tools
    /// - `ToolSurface::only(["Read", "Write"])` - Enable specific tools
    /// - `ToolSurface::except(["Bash"])` - Enable all except specific tools
    pub fn tools(mut self, access: ToolSurface) -> Self {
        self.config.security.tool_surface = access;
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
    // Authorization
    // =========================================================================

    /// Sets the complete tool policy.
    pub fn authorization_policy(mut self, policy: ToolPolicy) -> Self {
        self.authorization_policy_explicit = true;
        self.config.security.authorization_policy = policy;
        self
    }

    /// Sets the execution mode.
    pub fn execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }

    /// Adds a rule to allow a tool or pattern (e.g., `"Read"` or `"Bash(git:*)"`)
    pub fn allow_tool(mut self, pattern: impl Into<String>) -> Self {
        self.authorization_policy_explicit = true;
        self.config
            .security
            .authorization_policy
            .rules
            .push(ToolRule::allow_pattern(pattern));
        self
    }

    /// Adds a rule to deny a tool or pattern (e.g., `"Write"` or `"Bash(rm:*)"`)
    pub fn deny_tool(mut self, pattern: impl Into<String>) -> Self {
        self.authorization_policy_explicit = true;
        self.config
            .security
            .authorization_policy
            .rules
            .push(ToolRule::deny_pattern(pattern));
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
            .get_or_insert_with(crate::config::SandboxConfig::default)
            .network
            .allowed_domains
            .insert(domain.into());
        self
    }

    /// Adds a domain to the network blocklist.
    pub fn deny_domain(mut self, domain: impl Into<String>) -> Self {
        self.sandbox_settings
            .get_or_insert_with(crate::config::SandboxConfig::default)
            .network
            .blocked_domains
            .insert(domain.into());
        self
    }

    /// Enables or disables sandbox isolation.
    pub fn sandbox_enabled(mut self, enabled: bool) -> Self {
        self.sandbox_settings
            .get_or_insert_with(crate::config::SandboxConfig::default)
            .enabled = enabled;
        self
    }

    /// Excludes a command from sandbox restrictions.
    pub fn exclude_command(mut self, command: impl Into<String>) -> Self {
        self.sandbox_settings
            .get_or_insert_with(crate::config::SandboxConfig::default)
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

    /// Sets a custom session manager for live session and delegated task persistence.
    pub fn session_manager(mut self, manager: crate::session::SessionManager) -> Self {
        self.session_manager = Some(manager);
        self
    }

    /// Forks an existing session, creating a new branch.
    pub async fn fork_session(mut self, session_id: impl Into<String>) -> crate::Result<Self> {
        let manager = self.session_manager.take().unwrap_or_default();
        let session_id_str: String = session_id.into();
        let original_id = Self::parse_session_id(&session_id_str, "fork_session")?;
        let forked = manager.fork(&original_id).await?;

        self.initial_messages = Some(forked.to_api_messages());
        self.resume_session_id = Some(forked.id.to_string());
        self.resumed_session = Some(forked);
        self.session_manager = Some(manager);
        Ok(self)
    }

    pub async fn fork_session_from_node(
        mut self,
        session_id: impl Into<String>,
        from_node: crate::graph::NodeId,
    ) -> crate::Result<Self> {
        let manager = self.session_manager.take().unwrap_or_default();
        let session_id = session_id.into();
        let original_id = Self::parse_session_id(&session_id, "fork_session_from_node")?;
        let forked = manager.fork_from_node(&original_id, from_node).await?;

        self.initial_messages = Some(forked.to_api_messages());
        self.resume_session_id = Some(forked.id.to_string());
        self.resumed_session = Some(forked);
        self.session_manager = Some(manager);
        Ok(self)
    }

    /// Resumes an existing session by ID.
    pub async fn resume_session(mut self, session_id: impl Into<String>) -> crate::Result<Self> {
        let session_id_str: String = session_id.into();
        let id = Self::parse_session_id(&session_id_str, "resume_session")?;
        let manager = self.session_manager.take().unwrap_or_default();
        let session = manager.get(&id).await?;

        let messages: Vec<crate::ir::Message> = session
            .current_branch_messages()
            .into_iter()
            .map(|m| crate::ir::Message {
                role: m.role,
                content: m.content,
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
        let session_id = session_id.into();
        let id = Self::parse_session_id(&session_id, "resume_session_from_node")?;
        let manager = self.session_manager.take().unwrap_or_default();
        let session = manager.get(&id).await?;
        let replay = session.replay_input(Some(from_node))?;

        self.initial_messages = Some(replay.messages);
        self.resume_session_id = Some(id.to_string());
        self.resumed_session = Some(session);
        self.session_manager = Some(manager);
        Ok(self)
    }

    /// Sets initial messages for the conversation.
    pub fn messages(mut self, messages: Vec<crate::ir::Message>) -> Self {
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
    // Context Scope
    // =========================================================================

    /// Attaches a [`ContextScope`](crate::ContextScope) that wraps every tool
    /// execution future with per-request context.
    ///
    /// This is useful for propagating task-locals (e.g., workspace IDs for
    /// database RLS), tracing spans, or other request-scoped state into
    /// parallel tool invocations.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use branchforge::Agent;
    /// # async fn example() -> branchforge::Result<()> {
    /// // let scope: Arc<dyn branchforge::ContextScope> = Arc::new(my_scope);
    /// // let agent = Agent::builder().context_scope(scope).build().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn context_scope(mut self, scope: crate::context_scope::SharedContextScope) -> Self {
        self.context_scope = Some(scope);
        self
    }

    // =========================================================================
    // Compaction
    // =========================================================================

    /// Set a custom compaction chain.
    pub fn compaction_chain(mut self, chain: crate::session::compact::CompactionChain) -> Self {
        self.compaction_chain = Some(std::sync::Arc::new(chain));
        self
    }

    /// Configure a default advanced compaction chain (MicroCompaction → FullCompaction).
    pub fn advanced_compaction(self) -> Self {
        let chain = crate::session::compact::CompactionChain::builder()
            .strategy(crate::session::compact::MicroCompaction::default())
            .strategy(crate::session::compact::FullCompaction::default())
            .build();
        self.compaction_chain(chain)
    }

    // =========================================================================
    // Coordination
    // =========================================================================

    /// Set a multi-agent coordination mode.
    pub fn coordination(
        mut self,
        coord: impl crate::orchestration::Coordination + 'static,
    ) -> Self {
        self.coordination = Some(std::sync::Arc::new(coord));
        self
    }

    // =========================================================================
    // Recovery
    // =========================================================================

    /// Sets a custom context recovery strategy for handling context overflow errors.
    pub fn recovery_strategy(
        mut self,
        strategy: impl crate::session::compact::recovery::RecoveryStrategy + 'static,
    ) -> Self {
        self.recovery_strategy = Some(std::sync::Arc::new(strategy));
        self
    }

    /// Enables the default context recovery strategy ([`ContextRecovery`](crate::session::ContextRecovery)).
    pub fn default_recovery(self) -> Self {
        self.recovery_strategy(crate::session::compact::recovery::ContextRecovery::default())
    }

    // =========================================================================
    // Hooks
    // =========================================================================

    /// Registers an event hook.
    pub fn hook<H: Hook + 'static>(mut self, hook: H) -> Self {
        self.hooks.register(hook);
        self
    }

    /// Replaces the complete hook manager.
    pub fn hooks_manager(mut self, hooks: HookManager) -> Self {
        self.hooks = hooks;
        self
    }

    /// Replaces sandbox settings directly.
    pub fn sandbox_settings(mut self, settings: crate::config::SandboxConfig) -> Self {
        self.sandbox_settings = Some(settings);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::DEFAULT_MAX_TOKENS;
    use crate::ir::ContentPart;
    use crate::session::{SessionConfig, SessionManager, SessionMessage};

    #[test]
    fn test_tool_surface() {
        assert!(ToolSurface::all().is_allowed("Read"));
        assert!(!ToolSurface::none().is_allowed("Read"));
        assert!(ToolSurface::only(["Read", "Write"]).is_allowed("Read"));
        assert!(!ToolSurface::only(["Read", "Write"]).is_allowed("Bash"));
        assert!(!ToolSurface::except(["Bash"]).is_allowed("Bash"));
        assert!(ToolSurface::except(["Bash"]).is_allowed("Read"));
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
                SessionMessage::user(vec![ContentPart::text("one")]),
            )
            .await
            .unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::assistant(vec![ContentPart::text("two")]),
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
        let resumed_id = forked.resumed_session.as_ref().unwrap().id.to_string();
        assert_eq!(
            forked.resume_session_id.as_deref(),
            Some(resumed_id.as_str())
        );
    }

    #[tokio::test]
    async fn test_resume_session_rejects_invalid_uuid() {
        let result = AgentBuilder::new()
            .session_manager(SessionManager::in_memory())
            .resume_session("not-a-uuid")
            .await;

        assert!(result.is_err());
        let error = result.err().unwrap();
        assert!(error.to_string().contains("Invalid session ID"));
    }

    // =========================================================================
    // R10 fix regression guards — Auth wiring + OAuth header injection
    // =========================================================================

    /// R10-fix-1 — `Auth::ApiKey` resolves to a real credential and the
    /// builder constructs an explicit `ProviderClient` instead of falling
    /// through to `preset.build_from_env()`. Pre-fix: build_llm() ignored
    /// `self.credential` and tried to load `ANTHROPIC_API_KEY` from env.
    #[tokio::test]
    async fn test_auth_api_key_wires_provider_client() {
        let builder = AgentBuilder::new()
            .auth(crate::auth::Auth::api_key("sk-test-static-key"))
            .await
            .expect("api_key auth resolves");
        assert!(
            builder.provider_client.is_some(),
            "auth(ApiKey) must wire a ProviderClient so build_llm doesn't \
             fall back to env-var preset loading"
        );
        let pc = builder.provider_client.as_ref().unwrap();
        assert_eq!(pc.codec_id(), "anthropic-messages");
    }

    /// R10-fix-2 — `Auth::OAuth` (and `Auth::ClaudeCli` under cli-auth)
    /// resolves a Bearer credential and the resulting `DirectTransport`
    /// must include the Claude Code OAuth headers + `?beta=true` URL
    /// flag. Without these, the Anthropic API rejects with "OAuth
    /// authentication is currently not supported".
    #[tokio::test]
    async fn test_auth_oauth_wires_oauth_headers() {
        let builder = AgentBuilder::new()
            .auth(crate::auth::Auth::oauth("oat_test_token"))
            .await
            .expect("oauth auth resolves");
        assert!(builder.provider_client.is_some());

        let pc = builder.provider_client.as_ref().unwrap();
        // Resolve the endpoint via the codec shape so we observe the
        // headers + URL the transport will actually emit on every call.
        use crate::client::codec::{InvocationMode, ModelCodec};
        let codec = crate::client::codec::AnthropicMessagesCodec::new();
        let endpoint = pc
            .transport()
            .resolve_endpoint(
                codec.endpoint_shape(),
                "claude-haiku-4-5",
                InvocationMode::Unary,
            )
            .await
            .expect("resolve_endpoint succeeds");

        // The `?beta=true` query flag is REQUIRED for OAuth acceptance.
        assert!(
            endpoint.url.contains("beta=true"),
            "expected ?beta=true in OAuth URL: {}",
            endpoint.url
        );

        // Required OAuth headers from `OAuthConfig::default()`.
        let header_names: Vec<&str> = endpoint.headers.iter().map(|(k, _)| k.as_str()).collect();
        for required in [
            "user-agent",
            "x-app",
            "anthropic-dangerous-direct-browser-access",
            "anthropic-beta",
        ] {
            assert!(
                header_names.contains(&required),
                "expected header `{required}` in OAuth endpoint, got: {header_names:?}"
            );
        }

        // The anthropic-beta header must contain BOTH the OAuth marker
        // (`oauth-2025-04-20`) and the Claude Code marker
        // (`claude-code-20250219`).
        let beta_header = endpoint
            .headers
            .iter()
            .find(|(k, _)| k == "anthropic-beta")
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert!(
            beta_header.contains("oauth-2025-04-20"),
            "expected oauth-2025-04-20 in anthropic-beta: {beta_header}"
        );
        assert!(
            beta_header.contains("claude-code-20250219"),
            "expected claude-code-20250219 in anthropic-beta: {beta_header}"
        );
    }

    /// R10-fix-1 — `Auth::ApiKey` (no OAuth) must NOT inject the OAuth
    /// headers nor `?beta=true`. The bug we'd avoid: a static API key
    /// going out with `claude-cli/2.0.76` user-agent, which is wrong.
    #[tokio::test]
    async fn test_auth_api_key_does_not_inject_oauth_headers() {
        let builder = AgentBuilder::new()
            .auth(crate::auth::Auth::api_key("sk-test-key"))
            .await
            .unwrap();
        let pc = builder.provider_client.as_ref().unwrap();
        use crate::client::codec::{InvocationMode, ModelCodec};
        let codec = crate::client::codec::AnthropicMessagesCodec::new();
        let endpoint = pc
            .transport()
            .resolve_endpoint(
                codec.endpoint_shape(),
                "claude-haiku-4-5",
                InvocationMode::Unary,
            )
            .await
            .unwrap();
        assert!(
            !endpoint.url.contains("beta=true"),
            "ApiKey path must not append ?beta=true: {}",
            endpoint.url
        );
        let header_names: Vec<&str> = endpoint.headers.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            !header_names.contains(&"x-app"),
            "ApiKey path must not inject x-app header (Claude Code only)"
        );
    }

    #[test]
    fn auth_targets_direct_anthropic_covers_all_direct_variants() {
        // ApiKey, FromEnv, OAuth, Resolved → all direct.
        assert!(AgentBuilder::auth_targets_direct_anthropic(
            &crate::auth::Auth::api_key("k")
        ));
        assert!(AgentBuilder::auth_targets_direct_anthropic(
            &crate::auth::Auth::from_env()
        ));
        assert!(AgentBuilder::auth_targets_direct_anthropic(
            &crate::auth::Auth::oauth("t")
        ));
        // Resolved with an api-key credential.
        assert!(AgentBuilder::auth_targets_direct_anthropic(
            &crate::auth::Auth::Resolved(crate::auth::Credential::api_key("k"))
        ));

        // Cloud-provider variants must NOT be routed here.
        #[cfg(feature = "aws")]
        {
            let bedrock = crate::auth::Auth::Bedrock {
                region: "us-east-1".into(),
            };
            assert!(!AgentBuilder::auth_targets_direct_anthropic(&bedrock));
        }
    }
}
