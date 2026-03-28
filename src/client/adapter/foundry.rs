//! Azure AI Foundry adapter with API key and Entra ID authentication.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use azure_identity::DeveloperToolsCredential;

use super::base::RequestService;
use super::config::ProviderConfig;
use super::request::build_messages_body;
use super::token_cache::{CachedToken, TokenCache, new_token_cache};
use super::traits::ProviderAdapter;
use crate::client::messages::CreateMessageRequest;
use crate::config::FoundryConfig;
use crate::types::ApiResponse;
use crate::{Error, Result};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const COGNITIVE_SERVICES_SCOPE: &str = "https://cognitiveservices.azure.com/.default";

#[derive(Debug)]
pub struct FoundryAdapter {
    config: ProviderConfig,
    resource_name: Option<String>,
    base_url: Option<String>,
    credential: Arc<DeveloperToolsCredential>,
    api_key: Option<String>,
    token_cache: TokenCache,
}

impl FoundryAdapter {
    pub async fn from_env(config: ProviderConfig) -> Result<Self> {
        let foundry_config = FoundryConfig::from_env();
        Self::from_config(config, foundry_config).await
    }

    pub async fn from_config(config: ProviderConfig, foundry: FoundryConfig) -> Result<Self> {
        if foundry.resource.is_none() && foundry.base_url.is_none() {
            return Err(Error::auth(
                "Either ANTHROPIC_FOUNDRY_RESOURCE or ANTHROPIC_FOUNDRY_BASE_URL must be set",
            ));
        }

        let credential = DeveloperToolsCredential::new(None)
            .map_err(|e| Error::auth(format!("Failed to create Azure credential: {}", e)))?;

        Ok(Self {
            config,
            resource_name: foundry.resource,
            base_url: foundry.base_url,
            credential,
            api_key: foundry.api_key,
            token_cache: new_token_cache(),
        })
    }

    pub fn resource(mut self, resource: impl Into<String>) -> Self {
        self.resource_name = Some(resource.into());
        self
    }

    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    fn build_messages_url(&self) -> String {
        if let Some(ref base_url) = self.base_url {
            let base = base_url.trim_end_matches('/');
            format!("{}/v1/messages", base)
        } else if let Some(ref resource) = self.resource_name {
            format!(
                "https://{}.services.ai.azure.com/anthropic/v1/messages",
                resource
            )
        } else {
            unreachable!(
                "FoundryAdapter requires base_url or resource_name, enforced by from_config"
            )
        }
    }

    fn build_request_body(&self, request: &CreateMessageRequest) -> serde_json::Value {
        build_messages_body(request, None, self.config.thinking_budget)
    }

    async fn get_auth_header(&self) -> Result<(String, String)> {
        if let Some(ref api_key) = self.api_key {
            return Ok(("api-key".into(), api_key.clone()));
        }
        let token = self.get_token().await?;
        Ok(("Authorization".into(), format!("Bearer {}", token)))
    }

    async fn get_token(&self) -> Result<String> {
        {
            let cache = self.token_cache.read().await;
            if let Some(ref cached) = *cache
                && !cached.is_expired()
            {
                return Ok(cached.token().to_string());
            }
        }

        let token_response: azure_core::credentials::AccessToken = self
            .credential
            .get_token(&[COGNITIVE_SERVICES_SCOPE], None)
            .await
            .map_err(|e| Error::auth(format!("Failed to get Azure token: {}", e)))?;

        let token_str = token_response.token.secret().to_string();
        let cached = CachedToken::new(token_str.clone(), Duration::from_secs(3600));
        *self.token_cache.write().await = Some(cached);

        Ok(token_str)
    }

    async fn execute_request(
        &self,
        http: &reqwest::Client,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response> {
        let (header_name, header_value) = self.get_auth_header().await?;
        let headers = vec![
            (header_name, header_value),
            ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
        ];
        RequestService::post(http, url, body, headers).await
    }
}

#[async_trait]
impl ProviderAdapter for FoundryAdapter {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn name(&self) -> &'static str {
        "foundry"
    }

    async fn build_url(&self, _model: &str, _stream: bool) -> String {
        self.build_messages_url()
    }

    async fn transform_request(&self, request: CreateMessageRequest) -> Result<serde_json::Value> {
        Ok(self.build_request_body(&request))
    }

    async fn send(
        &self,
        http: &reqwest::Client,
        request: CreateMessageRequest,
    ) -> Result<ApiResponse> {
        let url = self.build_messages_url();
        let body = self.build_request_body(&request);
        let response = self.execute_request(http, &url, &body).await?;
        let json: serde_json::Value = response.json().await?;
        self.transform_response(json)
    }

    async fn send_stream(
        &self,
        http: &reqwest::Client,
        mut request: CreateMessageRequest,
    ) -> Result<reqwest::Response> {
        request.stream = Some(true);
        let url = self.build_messages_url();
        let body = self.build_request_body(&request);
        self.execute_request(http, &url, &body).await
    }

    async fn refresh_credentials(&self) -> Result<()> {
        *self.token_cache.write().await = None;
        if self.api_key.is_none() {
            self.get_token().await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::client::adapter::ModelConfig;

    #[test]
    fn test_build_url_with_resource() {
        let url = format!(
            "https://{}.services.ai.azure.com/anthropic/v1/messages",
            "my-resource"
        );
        assert!(url.contains("services.ai.azure.com"));
        assert!(url.contains("/anthropic/v1/messages"));
    }

    #[test]
    fn test_build_url_with_base_url() {
        let base_url = "https://custom-endpoint.azure.com/anthropic";
        let url = format!("{}/v1/messages", base_url.trim_end_matches('/'));
        assert!(url.contains("custom-endpoint.azure.com"));
        assert!(url.contains("/v1/messages"));
    }

    #[test]
    fn test_model_config() {
        let config = ModelConfig::foundry();
        assert!(config.primary.contains("sonnet"));
    }

    #[test]
    fn test_anthropic_version() {
        assert_eq!(super::ANTHROPIC_VERSION, "2023-06-01");
    }
}
