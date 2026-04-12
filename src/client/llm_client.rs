//! Standalone LLM client builder — construct an `Arc<dyn LlmCall>` without
//! a full Agent.
//!
//! The [`LlmClient`] builder handles provider detection, credential resolution,
//! OAuth header injection, and retry wrapping through the same code path as
//! [`crate::agent::AgentBuilder::auth`], but without requiring tools, session
//! management, or any other agent infrastructure.
//!
//! # Examples
//!
//! ```rust,no_run
//! use branchforge::{Auth, LlmClient};
//!
//! # async fn example() -> branchforge::Result<()> {
//! // One-liner: resolve auth, build client with default retry
//! let client = LlmClient::from_auth(Auth::from_env()).await?;
//!
//! // Builder: customise retry policy and HTTP client
//! let client = LlmClient::builder()
//!     .auth(Auth::api_key("sk-..."))
//!     .await?
//!     .retry(branchforge::RetryPolicy::default())
//!     .build()?;
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use crate::auth::{Auth, CLAUDE_CODE_BETA, Credential, CredentialProvider, OAuthConfig};
use crate::client::codec::{AnthropicMessagesCodec, ModelCodec};
use crate::client::llm_call::LlmCall;
use crate::client::preset::ProfileRegistry;
use crate::client::provider_client::ProviderClient;
use crate::client::transport::{DirectAuth, DirectTransport, ModelTransport};
use crate::client::{RetryPolicy, RetryingClient};
use crate::{Error, Result};

// ---------------------------------------------------------------------------
// LlmClient — public entry point
// ---------------------------------------------------------------------------

/// Standalone LLM client construction without agent overhead.
///
/// Two construction paths:
/// - [`LlmClient::from_auth`] — one-liner with default retry.
/// - [`LlmClient::builder`] — customisable retry, circuit breaker, HTTP client.
pub struct LlmClient;

impl LlmClient {
    /// Build an LLM client from an auth configuration with default retry.
    ///
    /// This is the recommended entry point for most use cases. Handles all
    /// provider types (API key, OAuth/ClaudeCli, Bedrock SigV4, etc.)
    /// through a single code path.
    pub async fn from_auth(auth: impl Into<Auth>) -> Result<Arc<dyn LlmCall>> {
        LlmClientBuilder::new()
            .auth(auth)
            .await?
            .retry(RetryPolicy::default())
            .build()
    }

    /// Start a builder for fine-grained control over retry, circuit breaker,
    /// and HTTP client configuration.
    pub fn builder() -> LlmClientBuilder {
        LlmClientBuilder::new()
    }
}

// ---------------------------------------------------------------------------
// LlmClientBuilder
// ---------------------------------------------------------------------------

/// Builder for standalone LLM clients.
///
/// Fluent API: `.auth()` → optional `.retry()` / `.http_client()` → `.build()`.
pub struct LlmClientBuilder {
    provider_client: Option<ProviderClient>,
    credential: Option<Credential>,
    cloud_profile: Option<&'static str>,
    retry: Option<RetryPolicy>,
    http: Option<reqwest::Client>,
}

impl LlmClientBuilder {
    fn new() -> Self {
        Self {
            provider_client: None,
            credential: None,
            cloud_profile: None,
            retry: None,
            http: None,
        }
    }

    /// Configure authentication. Resolves credentials and builds the
    /// appropriate transport for the provider.
    ///
    /// Supports all auth types: API key, OAuth (ClaudeCli), Bedrock,
    /// Vertex, Foundry, OpenAI, Gemini.
    pub async fn auth(mut self, auth: impl Into<Auth>) -> Result<Self> {
        let auth = auth.into();

        // Cloud providers that use ProfileRegistry (not direct Anthropic)
        match &auth {
            #[cfg(feature = "aws")]
            Auth::Bedrock { .. } => {
                self.cloud_profile = Some("bedrock");
            }
            #[cfg(feature = "gcp")]
            Auth::Vertex { .. } => {
                self.cloud_profile = Some("vertex-anthropic");
            }
            #[cfg(feature = "azure")]
            Auth::Foundry { .. } => {
                self.cloud_profile = Some("foundry-anthropic");
            }
            #[cfg(feature = "openai")]
            Auth::OpenAi { .. } => {
                self.cloud_profile = Some("openai");
            }
            #[cfg(feature = "gemini")]
            Auth::Gemini { .. } => {
                self.cloud_profile = Some("gemini");
            }
            _ => {}
        }

        // Resolve credential + optional refresh provider
        let (credential, refresh_provider) = auth.resolve_with_provider().await?;

        // Direct Anthropic auth (API key, OAuth, ClaudeCli, FromEnv, Resolved)
        if self.cloud_profile.is_none() && !credential.is_placeholder() {
            self.provider_client = Some(build_direct_anthropic(
                &credential,
                refresh_provider,
                self.http.as_ref(),
            )?);
        }

        self.credential = Some(credential);
        Ok(self)
    }

    /// Use a pre-built ProviderClient directly (advanced).
    pub fn provider_client(mut self, pc: ProviderClient) -> Self {
        self.provider_client = Some(pc);
        self
    }

    /// Set retry policy. Default: no retry. Use `RetryPolicy::default()`
    /// for 2 retries with exponential backoff.
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }

    /// Share a custom `reqwest::Client` (connection pool, proxies, timeouts).
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    /// Build the final `Arc<dyn LlmCall>` client.
    pub fn build(self) -> Result<Arc<dyn LlmCall>> {
        let base: Arc<dyn LlmCall> = if let Some(pc) = self.provider_client {
            Arc::new(pc)
        } else if let Some(profile_id) = self.cloud_profile {
            let registry = ProfileRegistry::with_builtins();
            Arc::new(registry.build(profile_id)?)
        } else {
            // Fallback: try Anthropic profile from env
            let registry = ProfileRegistry::with_builtins();
            Arc::new(registry.build("anthropic")?)
        };

        // Apply retry wrapper if requested
        let client: Arc<dyn LlmCall> = match self.retry {
            Some(policy) => Arc::new(RetryingClient::new(base, policy)),
            None => base,
        };

        Ok(client)
    }
}

// ---------------------------------------------------------------------------
// Direct Anthropic client builder (shared with AgentBuilder)
// ---------------------------------------------------------------------------

/// Build a `ProviderClient` for direct Anthropic API access.
///
/// Handles both API key (`x-api-key` header) and OAuth Bearer token
/// authentication. For OAuth credentials (Claude CLI), injects the
/// required beta headers and URL parameters.
///
/// This is the canonical implementation used by both [`LlmClientBuilder`]
/// and [`crate::agent::AgentBuilder`] — no duplication.
pub(crate) fn build_direct_anthropic(
    credential: &Credential,
    refresh_provider: Option<Arc<dyn CredentialProvider>>,
    http: Option<&reqwest::Client>,
) -> Result<ProviderClient> {
    let is_oauth = credential.is_oauth();
    let direct_auth = match credential {
        Credential::ApiKey(secret) => DirectAuth::XApiKey(secret.clone()),
        Credential::OAuth(oauth) => DirectAuth::Bearer(oauth.access_token.clone()),
        #[allow(unreachable_patterns)]
        _ => {
            return Err(Error::Config(
                "Unsupported credential type for direct Anthropic access".into(),
            ));
        }
    };

    let base =
        std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".into());
    let mut transport =
        DirectTransport::new(base, direct_auth).with_allowed_codecs(&["anthropic-messages"]);

    if is_oauth {
        let cfg = OAuthConfig::default();
        let beta_header = format!(
            "{},{}",
            crate::agent::BetaFeature::OAuth.header_value(),
            CLAUDE_CODE_BETA,
        );
        let mut extra_headers: HashMap<String, String> = cfg.extra_headers.clone();
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

    let auth_preamble = credential.auth_preamble();
    let codec = Arc::new(AnthropicMessagesCodec::new()) as Arc<dyn ModelCodec>;
    let transport = Arc::new(transport) as Arc<dyn ModelTransport>;

    if let Some(http) = http {
        ProviderClient::with_http(codec, transport, http.clone(), auth_preamble)
    } else {
        ProviderClient::new(codec, transport, auth_preamble)
    }
}
