//! Common utilities for cloud adapters.

use crate::{Error, Result};

pub struct RequestService;

impl RequestService {
    #[cfg(any(feature = "gcp", feature = "azure"))]
    pub async fn post(
        http: &reqwest::Client,
        url: &str,
        body: &serde_json::Value,
        headers: Vec<(String, String)>,
    ) -> Result<reqwest::Response> {
        let mut req = http
            .post(url)
            .header("Content-Type", "application/json")
            .json(body);

        for (name, value) in headers {
            req = req.header(&name, &value);
        }

        let response = req.send().await?;
        Self::check_response(response).await
    }

    #[cfg(feature = "aws")]
    pub async fn post_bytes(
        http: &reqwest::Client,
        url: &str,
        body: Vec<u8>,
        headers: Vec<(String, String)>,
    ) -> Result<reqwest::Response> {
        let mut req = http
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body);

        for (name, value) in headers {
            req = req.header(&name, &value);
        }

        let response = req.send().await?;
        Self::check_response(response).await
    }

    async fn check_response(response: reqwest::Response) -> Result<reqwest::Response> {
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Api {
                message: text,
                status: Some(status),
                error_type: None,
            });
        }
        Ok(response)
    }
}
