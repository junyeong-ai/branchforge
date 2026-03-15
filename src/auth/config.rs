//! OAuth configuration and request building for Claude Code CLI authentication.

use std::collections::HashMap;

use crate::client::BetaConfig;

pub const DEFAULT_USER_AGENT: &str = "claude-cli/2.0.76 (external, cli)";
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
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(ua) = std::env::var("BRANCHFORGE_USER_AGENT") {
            config.user_agent = ua;
        }
        if let Ok(app) = std::env::var("BRANCHFORGE_APP_IDENTIFIER") {
            config.app_identifier = app;
        }

        config
    }

    pub fn builder() -> OAuthConfigBuilder {
        OAuthConfigBuilder::default()
    }

    pub fn build_beta_header(&self, base: &BetaConfig) -> String {
        let mut beta = base.clone();
        beta.add(crate::client::BetaFeature::OAuth);
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
}
