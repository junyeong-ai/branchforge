//! Agent configuration types.
//!
//! Domain-separated configuration for clarity and maintainability.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use rust_decimal::Decimal;

use crate::authorization::ToolPolicy;
use crate::client::messages::DEFAULT_MAX_TOKENS;
use crate::output_style::OutputStyle;
use crate::tools::ToolSurface;

/// Model-related configuration.
#[derive(Debug, Clone)]
pub struct AgentModelConfig {
    /// Primary model for main operations
    pub primary: String,
    /// Smaller model for quick operations
    pub small: String,
    /// Maximum tokens per response
    pub max_tokens: u32,
    /// Enable extended context window (1M for supported models)
    pub extended_context: bool,
}

impl Default for AgentModelConfig {
    fn default() -> Self {
        Self {
            primary: crate::client::DEFAULT_MODEL.to_string(),
            small: crate::client::DEFAULT_SMALL_MODEL.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            extended_context: false,
        }
    }
}

impl AgentModelConfig {
    pub fn new(primary: impl Into<String>) -> Self {
        Self {
            primary: primary.into(),
            ..Default::default()
        }
    }

    pub fn small(mut self, small: impl Into<String>) -> Self {
        self.small = small.into();
        self
    }

    pub fn max_tokens(mut self, tokens: u32) -> Self {
        self.max_tokens = tokens;
        self
    }

    pub fn extended_context(mut self, enabled: bool) -> Self {
        self.extended_context = enabled;
        self
    }
}

/// Execution behavior configuration.
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Maximum agentic loop iterations
    pub max_iterations: usize,
    /// Overall execution timeout
    pub timeout: Option<Duration>,
    /// Timeout between streaming chunks (detects stalled connections)
    pub chunk_timeout: Duration,
    /// Enable automatic context compaction
    pub auto_compact: bool,
    /// Context usage threshold for compaction (0.0-1.0)
    pub compact_threshold: f32,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            max_iterations: 100,
            timeout: Some(Duration::from_secs(300)),
            chunk_timeout: Duration::from_secs(60),
            auto_compact: true,
            compact_threshold: crate::session::compact::DEFAULT_COMPACT_THRESHOLD,
        }
    }
}

impl ExecutionConfig {
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn without_timeout(mut self) -> Self {
        self.timeout = None;
        self
    }

    pub fn chunk_timeout(mut self, timeout: Duration) -> Self {
        self.chunk_timeout = timeout;
        self
    }

    pub fn auto_compact(mut self, enabled: bool) -> Self {
        self.auto_compact = enabled;
        self
    }

    pub fn compact_threshold(mut self, threshold: f32) -> Self {
        self.compact_threshold = threshold.clamp(0.0, 1.0);
        self
    }
}

/// Security and permission configuration.
#[derive(Debug, Clone, Default)]
pub struct SecurityConfig {
    /// Tool permission policy
    pub authorization_policy: ToolPolicy,
    /// Tool access control
    pub tool_surface: ToolSurface,
    /// Environment variables for tool execution
    pub env: HashMap<String, String>,
}

impl SecurityConfig {
    pub fn permissive() -> Self {
        Self {
            authorization_policy: ToolPolicy::permissive(),
            tool_surface: ToolSurface::All,
            ..Default::default()
        }
    }

    pub fn read_only() -> Self {
        Self {
            authorization_policy: ToolPolicy::builder()
                .allow("Read")
                .allow("Glob")
                .allow("Grep")
                .allow("WebSearch")
                .allow("WebFetch")
                .build(),
            tool_surface: ToolSurface::only(["Read", "Glob", "Grep", "WebSearch", "WebFetch"]),
            ..Default::default()
        }
    }

    pub fn authorization_policy(mut self, policy: ToolPolicy) -> Self {
        self.authorization_policy = policy;
        self
    }

    pub fn tool_surface(mut self, access: ToolSurface) -> Self {
        self.tool_surface = access;
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn envs(
        mut self,
        vars: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        for (k, v) in vars {
            self.env.insert(k.into(), v.into());
        }
        self
    }
}

/// Budget and cost control configuration.
#[derive(Debug, Clone)]
pub struct BudgetConfig {
    /// Maximum cost in USD
    pub max_cost_usd: Option<Decimal>,
    /// Model to fall back to when budget exceeded
    pub fallback_model: Option<String>,
    /// Budget usage percentage (0-100) at which to emit a warning alert
    pub alert_threshold_pct: u32,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_cost_usd: None,
            fallback_model: None,
            alert_threshold_pct: 80,
        }
    }
}

impl BudgetConfig {
    pub fn unlimited() -> Self {
        Self::default()
    }

    pub fn max_cost(mut self, usd: Decimal) -> Self {
        self.max_cost_usd = Some(usd);
        self
    }

    pub fn fallback(mut self, model: impl Into<String>) -> Self {
        self.fallback_model = Some(model.into());
        self
    }

    pub fn alert_threshold(mut self, pct: u32) -> Self {
        self.alert_threshold_pct = pct.min(100);
        self
    }
}

/// Identity and ownership configuration.
#[derive(Debug, Clone, Default)]
pub struct IdentityConfig {
    /// Tenant identifier for multi-tenant scoping.
    pub tenant_id: Option<String>,
    /// Principal identifier for the actor who owns the session.
    pub principal_id: Option<String>,
}

impl IdentityConfig {
    pub fn tenant(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    pub fn principal(mut self, principal_id: impl Into<String>) -> Self {
        self.principal_id = Some(principal_id.into());
        self
    }
}

/// Prompt and output configuration.
#[derive(Debug, Clone, Default)]
pub struct PromptConfig {
    /// Custom system prompt
    pub system_prompt: Option<String>,
    /// How to apply system prompt
    pub system_prompt_mode: SystemPromptMode,
    /// Output style customization
    pub output_style: Option<OutputStyle>,
    /// Structured output schema
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SystemPromptMode {
    /// Replace default system prompt
    #[default]
    Replace,
    /// Append to default system prompt
    Append,
}

impl PromptConfig {
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn append_mode(mut self) -> Self {
        self.system_prompt_mode = SystemPromptMode::Append;
        self
    }

    pub fn output_style(mut self, style: OutputStyle) -> Self {
        self.output_style = Some(style);
        self
    }

    pub fn output_schema(mut self, schema: serde_json::Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn structured_output<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.output_schema = serde_json::to_value(schema).ok();
        self
    }
}

/// Cache strategy determining which content types to cache.
///
/// Anthropic best practices recommend caching static content (system prompts,
/// tools) with longer TTLs and dynamic content (messages) with shorter TTLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheStrategy {
    /// No caching - all content sent without cache_control
    Disabled,
    /// Cache static context only (system prompt, CLAUDE.md, rules, skills)
    StaticOnly,
    /// Cache tool metadata segment only
    ToolsOnly,
    /// Cache static context and tool metadata segments
    StaticAndTools,
    /// Cache conversation history only
    ConversationOnly,
    /// Cache static context and conversation history
    StaticAndConversation,
    /// Cache tool metadata and conversation history
    ToolsAndConversation,
    /// Cache static context, tool metadata, and conversation history
    #[default]
    Full,
}

impl CacheStrategy {
    /// Returns true if static-context caching is enabled.
    pub fn cache_static(&self) -> bool {
        matches!(
            self,
            Self::StaticOnly | Self::StaticAndTools | Self::StaticAndConversation | Self::Full
        )
    }

    /// Returns true if tool-metadata caching is enabled.
    pub fn cache_tools(&self) -> bool {
        matches!(
            self,
            Self::ToolsOnly | Self::StaticAndTools | Self::ToolsAndConversation | Self::Full
        )
    }

    /// Returns true if conversation-history caching is enabled.
    pub fn cache_conversation(&self) -> bool {
        matches!(
            self,
            Self::ConversationOnly
                | Self::StaticAndConversation
                | Self::ToolsAndConversation
                | Self::Full
        )
    }

    /// Returns true if any caching is enabled
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

/// Cache configuration for prompt caching.
///
/// Implements Anthropic's prompt caching best practices:
/// - Static content (system prompt, CLAUDE.md) uses longer TTL (1 hour default)
/// - Dynamic content (messages) uses shorter TTL (5 minutes default)
/// - Long TTL content must come before short TTL content in requests
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Cache strategy determining which content types to cache
    pub strategy: CacheStrategy,
    /// TTL for static content (system prompt, tools, CLAUDE.md)
    pub static_ttl: crate::types::CacheTtl,
    /// TTL for message content (last user turn)
    pub message_ttl: crate::types::CacheTtl,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            strategy: CacheStrategy::Full,
            static_ttl: crate::types::CacheTtl::OneHour,
            message_ttl: crate::types::CacheTtl::FiveMinutes,
        }
    }
}

impl CacheConfig {
    /// Create a disabled cache configuration
    pub fn disabled() -> Self {
        Self {
            strategy: CacheStrategy::Disabled,
            ..Default::default()
        }
    }

    /// Create a static-context-only cache configuration.
    pub fn static_only() -> Self {
        Self {
            strategy: CacheStrategy::StaticOnly,
            ..Default::default()
        }
    }

    /// Create a tool-metadata-only cache configuration.
    pub fn tools_only() -> Self {
        Self {
            strategy: CacheStrategy::ToolsOnly,
            ..Default::default()
        }
    }

    /// Create a static-context-plus-tools cache configuration.
    pub fn static_and_tools() -> Self {
        Self {
            strategy: CacheStrategy::StaticAndTools,
            ..Default::default()
        }
    }

    /// Create a conversation-only cache configuration.
    pub fn conversation_only() -> Self {
        Self {
            strategy: CacheStrategy::ConversationOnly,
            ..Default::default()
        }
    }

    /// Create a static-context-plus-conversation cache configuration.
    pub fn static_and_conversation() -> Self {
        Self {
            strategy: CacheStrategy::StaticAndConversation,
            ..Default::default()
        }
    }

    /// Create a tools-plus-conversation cache configuration.
    pub fn tools_and_conversation() -> Self {
        Self {
            strategy: CacheStrategy::ToolsAndConversation,
            ..Default::default()
        }
    }

    /// Set the cache strategy
    pub fn strategy(mut self, strategy: CacheStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Set the TTL for static content
    pub fn static_ttl(mut self, ttl: crate::types::CacheTtl) -> Self {
        self.static_ttl = ttl;
        self
    }

    /// Set the TTL for message content
    pub fn message_ttl(mut self, ttl: crate::types::CacheTtl) -> Self {
        self.message_ttl = ttl;
        self
    }

    /// Get conversation TTL if conversation caching is enabled, None otherwise.
    ///
    /// This is a convenience method to avoid duplicating the cache_conversation() check
    /// at every call site.
    pub fn conversation_ttl_option(&self) -> Option<crate::types::CacheTtl> {
        if self.strategy.cache_conversation() {
            Some(self.message_ttl)
        } else {
            None
        }
    }
}

/// Server-side tools configuration.
///
/// Anthropic's built-in server-side tools (Brave Search, web fetch).
/// These are automatically enabled when "WebSearch" or "WebFetch" are in ToolSurface.
#[derive(Debug, Clone, Default)]
pub struct ServerToolsConfig {
    pub web_search: Option<crate::types::WebSearchTool>,
    pub web_fetch: Option<crate::types::WebFetchTool>,
}

impl ServerToolsConfig {
    pub fn all() -> Self {
        Self {
            web_search: Some(crate::types::WebSearchTool::default()),
            web_fetch: Some(crate::types::WebFetchTool::default()),
        }
    }

    pub fn web_search(mut self, config: crate::types::WebSearchTool) -> Self {
        self.web_search = Some(config);
        self
    }

    pub fn web_fetch(mut self, config: crate::types::WebFetchTool) -> Self {
        self.web_fetch = Some(config);
        self
    }
}

/// Complete agent configuration combining all domain configs.
#[derive(Debug, Clone, Default)]
pub struct AgentConfig {
    pub model: AgentModelConfig,
    pub execution: ExecutionConfig,
    pub security: SecurityConfig,
    pub identity: IdentityConfig,
    pub budget: BudgetConfig,
    pub prompt: PromptConfig,
    pub cache: CacheConfig,
    pub working_dir: Option<PathBuf>,
    pub server_tools: ServerToolsConfig,
    pub coding_mode: bool,
}

impl AgentConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn model(mut self, config: AgentModelConfig) -> Self {
        self.model = config;
        self
    }

    pub fn execution(mut self, config: ExecutionConfig) -> Self {
        self.execution = config;
        self
    }

    pub fn security(mut self, config: SecurityConfig) -> Self {
        self.security = config;
        self
    }

    pub fn identity(mut self, config: IdentityConfig) -> Self {
        self.identity = config;
        self
    }

    pub fn budget(mut self, config: BudgetConfig) -> Self {
        self.budget = config;
        self
    }

    pub fn prompt(mut self, config: PromptConfig) -> Self {
        self.prompt = config;
        self
    }

    pub fn cache(mut self, config: CacheConfig) -> Self {
        self.cache = config;
        self
    }

    pub fn working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    pub fn server_tools(mut self, config: ServerToolsConfig) -> Self {
        self.server_tools = config;
        self
    }

    pub fn coding_mode(mut self, enabled: bool) -> Self {
        self.coding_mode = enabled;
        self
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_model_config() {
        let config = AgentModelConfig::new("claude-opus-4-6")
            .small("claude-haiku")
            .max_tokens(4096);

        assert_eq!(config.primary, "claude-opus-4-6");
        assert_eq!(config.small, "claude-haiku");
        assert_eq!(config.max_tokens, 4096);
    }

    #[test]
    fn test_execution_config() {
        let config = ExecutionConfig::default()
            .max_iterations(50)
            .timeout(Duration::from_secs(600))
            .auto_compact(false);

        assert_eq!(config.max_iterations, 50);
        assert_eq!(config.timeout, Some(Duration::from_secs(600)));
        assert!(!config.auto_compact);
    }

    #[test]
    fn test_security_config() {
        let config = SecurityConfig::permissive().env("API_KEY", "secret");

        assert_eq!(config.env.get("API_KEY"), Some(&"secret".to_string()));
    }

    #[test]
    fn test_budget_config() {
        let config = BudgetConfig::unlimited()
            .max_cost(dec!(10))
            .fallback("claude-haiku");

        assert_eq!(config.max_cost_usd, Some(dec!(10)));
        assert_eq!(config.fallback_model, Some("claude-haiku".to_string()));
    }

    #[test]
    fn test_identity_config() {
        let config = IdentityConfig::default()
            .tenant("org-123")
            .principal("user-456");

        assert_eq!(config.tenant_id, Some("org-123".to_string()));
        assert_eq!(config.principal_id, Some("user-456".to_string()));
    }

    #[test]
    fn test_agent_config() {
        let config = AgentConfig::new()
            .model(AgentModelConfig::new("claude-opus-4-6"))
            .budget(BudgetConfig::unlimited().max_cost(dec!(5)))
            .working_dir("/project");

        assert_eq!(config.model.primary, "claude-opus-4-6");
        assert_eq!(config.budget.max_cost_usd, Some(dec!(5)));
        assert_eq!(config.working_dir, Some(PathBuf::from("/project")));
    }

    #[test]
    fn test_cache_strategy_default_is_full() {
        let config = CacheConfig::default();
        assert_eq!(config.strategy, CacheStrategy::Full);
        assert_eq!(config.static_ttl, crate::types::CacheTtl::OneHour);
        assert_eq!(config.message_ttl, crate::types::CacheTtl::FiveMinutes);
    }

    #[test]
    fn test_cache_strategy_disabled() {
        let config = CacheConfig::disabled();
        assert_eq!(config.strategy, CacheStrategy::Disabled);
        assert!(!config.strategy.is_enabled());
        assert!(!config.strategy.cache_static());
        assert!(!config.strategy.cache_tools());
        assert!(!config.strategy.cache_conversation());
    }

    #[test]
    fn test_cache_strategy_static_only() {
        let config = CacheConfig::static_only();
        assert_eq!(config.strategy, CacheStrategy::StaticOnly);
        assert!(config.strategy.is_enabled());
        assert!(config.strategy.cache_static());
        assert!(!config.strategy.cache_tools());
        assert!(!config.strategy.cache_conversation());
    }

    #[test]
    fn test_cache_strategy_conversation_only() {
        let config = CacheConfig::conversation_only();
        assert_eq!(config.strategy, CacheStrategy::ConversationOnly);
        assert!(config.strategy.is_enabled());
        assert!(!config.strategy.cache_static());
        assert!(!config.strategy.cache_tools());
        assert!(config.strategy.cache_conversation());
    }

    #[test]
    fn test_cache_strategy_static_and_tools() {
        let config = CacheConfig::static_and_tools();
        assert!(config.strategy.cache_static());
        assert!(config.strategy.cache_tools());
        assert!(!config.strategy.cache_conversation());
    }

    #[test]
    fn test_cache_strategy_static_and_conversation() {
        let config = CacheConfig::static_and_conversation();
        assert!(config.strategy.cache_static());
        assert!(!config.strategy.cache_tools());
        assert!(config.strategy.cache_conversation());
    }

    #[test]
    fn test_cache_strategy_tools_and_conversation() {
        let config = CacheConfig::tools_and_conversation();
        assert!(!config.strategy.cache_static());
        assert!(config.strategy.cache_tools());
        assert!(config.strategy.cache_conversation());
    }

    #[test]
    fn test_cache_strategy_full() {
        let config = CacheConfig::default();
        assert!(config.strategy.is_enabled());
        assert!(config.strategy.cache_static());
        assert!(config.strategy.cache_tools());
        assert!(config.strategy.cache_conversation());
    }

    #[test]
    fn test_cache_config_with_ttl() {
        let config = CacheConfig::default()
            .static_ttl(crate::types::CacheTtl::FiveMinutes)
            .message_ttl(crate::types::CacheTtl::OneHour);

        assert_eq!(config.static_ttl, crate::types::CacheTtl::FiveMinutes);
        assert_eq!(config.message_ttl, crate::types::CacheTtl::OneHour);
    }
}
