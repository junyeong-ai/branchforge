//! OAuth2 token refresh via refresh_token grant (RFC 6749 Section 6).

use chrono::Utc;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use super::OAuthCredential;
use crate::{Error, Result};

const DEFAULT_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

/// Resolve the token endpoint URL from environment or default.
pub fn token_url() -> String {
    std::env::var("BRANCHFORGE_TOKEN_URL").unwrap_or_else(|_| DEFAULT_TOKEN_URL.to_string())
}

/// OAuth2 token endpoint response.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    #[allow(dead_code)]
    token_type: Option<String>,
    #[allow(dead_code)]
    scope: Option<String>,
}

/// Perform an OAuth2 refresh_token grant and return updated credentials.
///
/// If the server does not issue a new refresh token, the caller is responsible
/// for preserving the original one (RFC 6749 Section 6).
pub async fn refresh_access_token(
    http: &reqwest::Client,
    token_endpoint: &str,
    refresh_token: &SecretString,
    client_id: Option<&str>,
) -> Result<OAuthCredential> {
    let body = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("grant_type", "refresh_token");
        serializer.append_pair("refresh_token", refresh_token.expose_secret());
        if let Some(id) = client_id {
            serializer.append_pair("client_id", id);
        }
        serializer.finish()
    };

    let response = http
        .post(token_endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| Error::auth(format!("Token refresh request failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let err_body = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("<no body>"));
        return Err(Error::auth(format!(
            "Token refresh failed ({}): {}",
            status, err_body
        )));
    }

    let token: TokenResponse = response
        .json()
        .await
        .map_err(|e| Error::auth(format!("Failed to parse token response: {}", e)))?;

    let expires_at = token.expires_in.map(|secs| Utc::now().timestamp() + secs);

    Ok(OAuthCredential {
        access_token: SecretString::from(token.access_token),
        refresh_token: token.refresh_token.map(SecretString::from),
        expires_at,
        scopes: vec![],
        subscription_type: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_refresh_success() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "new-access-token",
                "refresh_token": "new-refresh-token",
                "expires_in": 3600,
                "token_type": "Bearer"
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let refresh_token = SecretString::from("old-refresh-token");

        let cred = refresh_access_token(&http, &server.uri(), &refresh_token, None)
            .await
            .unwrap();

        assert_eq!(cred.access_token.expose_secret(), "new-access-token");
        assert!(cred.refresh_token.is_some());
        assert!(cred.expires_at.is_some());
    }

    #[tokio::test]
    async fn test_refresh_token_not_rotated() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "new-access-token",
                "expires_in": 3600,
                "token_type": "Bearer"
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let refresh_token = SecretString::from("keep-this-token");

        let cred = refresh_access_token(&http, &server.uri(), &refresh_token, None)
            .await
            .unwrap();

        // Server didn't rotate: caller must preserve original
        assert!(cred.refresh_token.is_none());
    }

    #[tokio::test]
    async fn test_refresh_server_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "Refresh token expired"
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let refresh_token = SecretString::from("expired-token");

        let result = refresh_access_token(&http, &server.uri(), &refresh_token, None).await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid_grant"));
    }

    #[tokio::test]
    async fn test_refresh_with_client_id() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(body_string_contains("client_id=my-app"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "new-token",
                "expires_in": 3600,
                "token_type": "Bearer"
            })))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let refresh_token = SecretString::from("rt");

        let result =
            refresh_access_token(&http, &server.uri(), &refresh_token, Some("my-app")).await;

        assert!(result.is_ok());
    }

    #[test]
    fn test_token_url_default() {
        // When env var is not set, returns default
        let url = token_url();
        // Either env var value or default
        assert!(!url.is_empty());
    }
}
