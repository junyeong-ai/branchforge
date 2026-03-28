//! Google Vertex AI adapter with ADC authentication.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gcp_auth::TokenProvider;

use super::base::RequestService;
use super::config::ProviderConfig;
use super::request::build_cloud_request_body;
use super::token_cache::{CachedToken, TokenCache, new_token_cache};
use super::traits::ProviderAdapter;
use crate::client::messages::CreateMessageRequest;
use crate::config::VertexConfig;
use crate::types::ApiResponse;
use crate::{Error, Result};

const ANTHROPIC_VERSION: &str = "vertex-2023-10-16";

pub struct VertexAdapter {
    config: ProviderConfig,
    project_id: String,
    default_region: String,
    model_regions: HashMap<String, String>,
    enable_1m_context: bool,
    token_provider: Arc<dyn TokenProvider>,
    token_cache: TokenCache,
}

impl std::fmt::Debug for VertexAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexAdapter")
            .field("config", &self.config)
            .field("project_id", &self.project_id)
            .field("default_region", &self.default_region)
            .field("model_regions", &self.model_regions)
            .field("enable_1m_context", &self.enable_1m_context)
            .finish_non_exhaustive()
    }
}

impl VertexAdapter {
    pub async fn from_env(config: ProviderConfig) -> Result<Self> {
        let vertex_config = VertexConfig::from_env();
        Self::from_config(config, vertex_config).await
    }

    pub async fn from_config(config: ProviderConfig, vertex: VertexConfig) -> Result<Self> {
        let token_provider = gcp_auth::provider()
            .await
            .map_err(|e| Error::auth(e.to_string()))?;

        let project_id = vertex
            .project_id
            .ok_or_else(|| Error::auth("No GCP project ID found"))?;

        let default_region = vertex.region.unwrap_or_else(|| "us-central1".into());

        Ok(Self {
            config,
            project_id,
            default_region,
            model_regions: vertex.model_regions,
            enable_1m_context: vertex.enable_1m_context,
            token_provider,
            token_cache: new_token_cache(),
        })
    }

    pub fn project(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = project_id.into();
        self
    }

    pub fn region(mut self, region: impl Into<String>) -> Self {
        self.default_region = region.into();
        self
    }

    pub fn model_region(mut self, model_key: impl Into<String>, region: impl Into<String>) -> Self {
        self.model_regions.insert(model_key.into(), region.into());
        self
    }

    pub fn use_1m_context(mut self, enable: bool) -> Self {
        self.enable_1m_context = enable;
        self
    }

    fn region_for_model(&self, model: &str) -> &str {
        for (key, region) in &self.model_regions {
            if model.contains(key) {
                return region;
            }
        }
        &self.default_region
    }

    fn is_global(&self) -> bool {
        self.default_region == "global"
    }

    fn build_url_for_model(&self, model: &str, stream: bool) -> String {
        let region = self.region_for_model(model);
        let endpoint = if stream {
            "streamRawPredict"
        } else {
            "rawPredict"
        };

        if self.is_global() && region == "global" {
            format!(
                "https://aiplatform.googleapis.com/v1/projects/{}/locations/global/publishers/anthropic/models/{}:{}",
                self.project_id, model, endpoint
            )
        } else {
            format!(
                "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/anthropic/models/{}:{}",
                region, self.project_id, region, model, endpoint
            )
        }
    }

    fn build_request_body(&self, request: &CreateMessageRequest) -> serde_json::Value {
        build_cloud_request_body(
            request,
            ANTHROPIC_VERSION,
            self.config.thinking_budget,
            self.enable_1m_context,
        )
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

        let scopes = &["https://www.googleapis.com/auth/cloud-platform"];
        let token = self
            .token_provider
            .token(scopes)
            .await
            .map_err(|e| Error::auth(e.to_string()))?;

        let token_str = token.as_str().to_string();
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
        let token = self.get_token().await?;
        let headers = vec![("Authorization".into(), format!("Bearer {}", token))];
        RequestService::post(http, url, body, headers).await
    }
}

#[async_trait]
impl ProviderAdapter for VertexAdapter {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    fn name(&self) -> &'static str {
        "vertex"
    }

    async fn build_url(&self, model: &str, stream: bool) -> String {
        self.build_url_for_model(model, stream)
    }

    async fn transform_request(&self, request: CreateMessageRequest) -> Result<serde_json::Value> {
        Ok(self.build_request_body(&request))
    }

    async fn send(
        &self,
        http: &reqwest::Client,
        request: CreateMessageRequest,
    ) -> Result<ApiResponse> {
        let model = request.model.clone();
        let url = self.build_url_for_model(&model, false);
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
        let model = request.model.clone();
        let url = self.build_url_for_model(&model, true);
        let body = self.build_request_body(&request);

        self.execute_request(http, &url, &body).await
    }

    async fn refresh_credentials(&self) -> Result<()> {
        *self.token_cache.write().await = None;
        self.get_token().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use crate::client::adapter::ModelConfig;

    #[tokio::test]
    async fn test_build_url() {
        let url = format!(
            "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/anthropic/models/{}:rawPredict",
            "us-central1", "my-project", "us-central1", "claude-sonnet-4-5@20250929"
        );
        assert!(url.contains("aiplatform.googleapis.com"));
        assert!(url.contains("rawPredict"));
    }

    #[test]
    fn test_model_config() {
        let config = ModelConfig::vertex();
        assert!(config.primary.contains("@"));
    }
}
