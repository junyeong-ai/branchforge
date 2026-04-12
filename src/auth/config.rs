//! OAuth configuration and request building for Claude Code CLI authentication.

#![allow(missing_docs)]

use std::collections::HashMap;

use crate::agent::BetaConfig;

/// User-agent sent on Claude Code OAuth requests.
///
/// The Anthropic OAuth-enabled endpoint validates the `claude-cli/<version>`
/// prefix as part of the OAuth-app allowlist, so we keep the original CLI
/// identifier first. The `branchforge/<version>` suffix follows standard
/// HTTP user-agent chaining (RFC 9110 §10.1.5: `User-Agent = product
/// *( RWS ( product / comment ) )`) so observers — proxies, tracing
/// dashboards, abuse-detection — can still identify the actual SDK
/// making the call. The branchforge version is read from
/// `CARGO_PKG_VERSION` at compile time so the suffix stays in sync with
/// Cargo.toml automatically.
pub const DEFAULT_USER_AGENT: &str = concat!(
    "claude-cli/2.0.76 (external, cli) branchforge/",
    env!("CARGO_PKG_VERSION"),
);
pub const DEFAULT_APP_IDENTIFIER: &str = "cli";
pub const CLAUDE_CODE_BETA: &str = "claude-code-20250219";

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub user_agent: String,
    pub app_identifier: String,
    pub url_params: HashMap<String, String>,
    pub extra_headers: HashMap<String, String>,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.to_string(),
            app_identifier: DEFAULT_APP_IDENTIFIER.to_string(),
            url_params: [("beta".to_string(), "true".to_string())]
                .into_iter()
                .collect(),
            extra_headers: [(
                "anthropic-dangerous-direct-browser-access".to_string(),
                "true".to_string(),
            )]
            .into_iter()
            .collect(),
        }
    }
}

impl OAuthConfig {
    /// Build a config from the process environment. Convenience
    /// wrapper around [`Self::from_env_with`] that passes
    /// [`crate::common::env::SystemEnv`]. Production callers use
    /// this; tests use `from_env_with` with an injected fake.
    pub fn from_env() -> Self {
        Self::from_env_with(&crate::common::env::SystemEnv)
    }

    /// Phase I-1: build a config against an injected
    /// [`crate::common::env::EnvLookup`]. Reads
    /// `BRANCHFORGE_USER_AGENT` and `BRANCHFORGE_APP_IDENTIFIER`
    /// through the seam so tests stay hermetic — no process env
    /// mutation, no parallel-test races.
    pub fn from_env_with(env: &dyn crate::common::env::EnvLookup) -> Self {
        let mut config = Self::default();

        if let Some(ua) = env.get("BRANCHFORGE_USER_AGENT") {
            config.user_agent = ua;
        }
        if let Some(app) = env.get("BRANCHFORGE_APP_IDENTIFIER") {
            config.app_identifier = app;
        }

        config
    }

    pub fn builder() -> OAuthConfigBuilder {
        OAuthConfigBuilder::default()
    }

    pub fn build_beta_header(&self, base: &BetaConfig) -> String {
        let mut beta = base.clone();
        beta.add(crate::agent::BetaFeature::OAuth);
        beta.add_custom(CLAUDE_CODE_BETA);
        beta.header_value().unwrap_or_default()
    }

    pub fn build_url(&self, base_url: &str, endpoint: &str) -> String {
        let url = format!("{}{}", base_url, endpoint);
        if self.url_params.is_empty() {
            url
        } else {
            let mut serializer = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in &self.url_params {
                serializer.append_pair(k, v);
            }
            format!("{}?{}", url, serializer.finish())
        }
    }

    pub fn apply_headers(
        &self,
        req: reqwest::RequestBuilder,
        token: &str,
        api_version: &str,
        beta: &BetaConfig,
    ) -> reqwest::RequestBuilder {
        let mut r = req
            .header("Authorization", format!("Bearer {}", token))
            .header("anthropic-version", api_version)
            .header("content-type", "application/json")
            .header("user-agent", &self.user_agent)
            .header("x-app", &self.app_identifier);

        for (k, v) in &self.extra_headers {
            r = r.header(k.as_str(), v.as_str());
        }

        let beta_header = self.build_beta_header(beta);
        if !beta_header.is_empty() {
            r = r.header("anthropic-beta", beta_header);
        }

        r
    }
}

pub struct OAuthConfigBuilder {
    config: OAuthConfig,
}

impl Default for OAuthConfigBuilder {
    fn default() -> Self {
        Self {
            config: OAuthConfig::from_env(),
        }
    }
}

impl OAuthConfigBuilder {
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.config.user_agent = ua.into();
        self
    }

    pub fn app_identifier(mut self, app: impl Into<String>) -> Self {
        self.config.app_identifier = app.into();
        self
    }

    pub fn url_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.url_params.insert(key.into(), value.into());
        self
    }

    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.config.extra_headers.insert(key.into(), value.into());
        self
    }

    pub fn build(self) -> OAuthConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = OAuthConfig::default();
        assert_eq!(config.user_agent, DEFAULT_USER_AGENT);
        assert_eq!(config.app_identifier, DEFAULT_APP_IDENTIFIER);
    }

    #[test]
    fn test_builder() {
        let config = OAuthConfig::builder().user_agent("my-app/1.0").build();

        assert_eq!(config.user_agent, "my-app/1.0");
    }

    #[test]
    fn test_url_params() {
        let config = OAuthConfig::default();
        assert_eq!(config.url_params.get("beta"), Some(&"true".to_string()));
    }

    /// The `claude-cli/<version>` prefix is required by the Anthropic
    /// OAuth-enabled endpoint (it gates OAuth acceptance on the CLI
    /// allowlist). Removing or reordering it would break the live OAuth
    /// flow — pin the contract here so a future refactor can't silently
    /// drop the prefix.
    #[test]
    fn user_agent_starts_with_claude_cli_prefix() {
        assert!(
            DEFAULT_USER_AGENT.starts_with("claude-cli/"),
            "Anthropic OAuth requires the claude-cli/ prefix; got: {DEFAULT_USER_AGENT}"
        );
    }

    /// The chained `branchforge/<version>` suffix lets observers identify
    /// the actual SDK behind the OAuth call. The version must come from
    /// `CARGO_PKG_VERSION` so it stays in sync with Cargo.toml — hard-
    /// coded version strings drift the moment we bump the crate.
    #[test]
    fn user_agent_includes_branchforge_version_from_cargo() {
        let expected_suffix = format!("branchforge/{}", env!("CARGO_PKG_VERSION"));
        assert!(
            DEFAULT_USER_AGENT.contains(&expected_suffix),
            "expected `{expected_suffix}` in DEFAULT_USER_AGENT, got: {DEFAULT_USER_AGENT}"
        );
    }
}
