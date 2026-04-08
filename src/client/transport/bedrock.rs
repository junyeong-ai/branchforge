//! `BedrockTransport` — AWS Bedrock with SigV4 / bearer-token auth.
//!
//! Pinned to [`BedrockConverseCodec`](crate::client::codec::BedrockConverseCodec)
//! because Bedrock Converse only ever travels over
//! `bedrock-runtime.{region}.amazonaws.com` with AWS SigV4 (or the new
//! `AWS_BEARER_TOKEN_BEDROCK` token).
//!
//! Feature-gated behind `aws` because of the AWS SigV4 + credentials
//! crates.

#![cfg(feature = "aws")]

use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_credential_types::provider::ProvideCredentials;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningSettings, sign};
use aws_sigv4::sign::v4::SigningParams;
use aws_smithy_runtime_api::client::identity::Identity;
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::RwLock;

use super::{Endpoint, ModelTransport};
use crate::client::codec::{EndpointShape, InvocationMode};
use crate::{Error, Result};

const SERVICE: &str = "bedrock";

/// Authentication scheme for Bedrock.
#[derive(Clone)]
enum BedrockAuth {
    /// AWS SigV4 over a `ProvideCredentials` chain (env, profile, IMDS, …).
    SigV4(Arc<dyn ProvideCredentials>),
    /// `Authorization: Bearer <token>` using `AWS_BEARER_TOKEN_BEDROCK`.
    Bearer(SecretString),
}

impl std::fmt::Debug for BedrockAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SigV4(_) => f.debug_tuple("SigV4").field(&"<provider>").finish(),
            Self::Bearer(_) => f.debug_tuple("Bearer").field(&"[redacted]").finish(),
        }
    }
}

/// Bedrock transport.
pub struct BedrockTransport {
    region: String,
    use_global_endpoint: bool,
    auth: RwLock<BedrockAuth>,
}

impl std::fmt::Debug for BedrockTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BedrockTransport")
            .field("region", &self.region)
            .field("use_global_endpoint", &self.use_global_endpoint)
            .finish_non_exhaustive()
    }
}

impl BedrockTransport {
    /// Build from environment: `AWS_REGION`, optional `AWS_BEARER_TOKEN_BEDROCK`,
    /// otherwise falls back to the default AWS credential provider chain.
    pub async fn from_env() -> Result<Self> {
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".into());

        let auth = if let Ok(tok) = std::env::var("AWS_BEARER_TOKEN_BEDROCK") {
            BedrockAuth::Bearer(SecretString::from(tok))
        } else {
            let aws_config = aws_config::load_defaults(BehaviorVersion::latest()).await;
            let provider = aws_config
                .credentials_provider()
                .ok_or_else(|| Error::auth("bedrock: no AWS credentials found"))?;
            BedrockAuth::SigV4(Arc::from(provider))
        };

        Ok(Self {
            region,
            use_global_endpoint: false,
            auth: RwLock::new(auth),
        })
    }

    /// Override the region.
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    /// Use the global endpoint (`bedrock-runtime.amazonaws.com`) instead
    /// of the regional one.
    pub fn with_global_endpoint(mut self) -> Self {
        self.use_global_endpoint = true;
        self
    }

    fn host(&self) -> String {
        if self.use_global_endpoint {
            "bedrock-runtime.amazonaws.com".to_string()
        } else {
            format!("bedrock-runtime.{}.amazonaws.com", self.region)
        }
    }

    async fn sigv4_headers(&self, url: &str, body: &[u8]) -> Result<Vec<(String, String)>> {
        let provider = {
            let auth = self.auth.read().await;
            match &*auth {
                BedrockAuth::SigV4(p) => Arc::clone(p),
                BedrockAuth::Bearer(_) => {
                    return Err(Error::auth("bedrock: bearer token mode skips sigv4"));
                }
            }
        };
        let creds = provider
            .provide_credentials()
            .await
            .map_err(|e| Error::auth(format!("bedrock credentials: {e}")))?;

        let aws_creds = Credentials::new(
            creds.access_key_id(),
            creds.secret_access_key(),
            creds.session_token().map(str::to_string),
            creds.expiry(),
            "branchforge-bedrock",
        );
        let identity = Identity::new(aws_creds, creds.expiry());
        let signing_params = SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name(SERVICE)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .map_err(|e| Error::auth(format!("bedrock signing params: {e}")))?;

        let signable = SignableRequest::new(
            "POST",
            url,
            std::iter::empty::<(&str, &str)>(),
            SignableBody::Bytes(body),
        )
        .map_err(|e| Error::auth(format!("bedrock signable request: {e}")))?;

        let (instructions, _) = sign(signable, &signing_params.into())
            .map_err(|e| Error::auth(format!("bedrock sign: {e}")))?
            .into_parts();

        Ok(instructions
            .headers()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect())
    }
}

#[async_trait]
impl ModelTransport for BedrockTransport {
    fn id(&self) -> &'static str {
        "bedrock"
    }

    fn supports_codec(&self, codec_id: &str) -> bool {
        codec_id == "bedrock-converse"
    }

    async fn resolve_endpoint(
        &self,
        shape: &EndpointShape,
        model: &str,
        mode: InvocationMode,
    ) -> Result<Endpoint> {
        let verb = match mode {
            InvocationMode::Stream => shape.verb_stream,
            _ => shape.verb_unary,
        };
        let encoded_model = urlencoding::encode(model);
        let path = format!("model/{encoded_model}/{verb}");
        let url = format!("https://{}/{}", self.host(), path);

        let mut headers = Vec::new();
        // Streaming requires the Accept header for AWS EventStream framing.
        if matches!(mode, InvocationMode::Stream) {
            headers.push((
                "accept".to_string(),
                "application/vnd.amazon.eventstream".to_string(),
            ));
        }
        headers.push(("content-type".to_string(), "application/json".to_string()));
        Ok(Endpoint { url, headers })
    }

    fn classify_error(
        &self,
        status: u16,
        body: &str,
    ) -> (crate::error::ProviderErrorKind, Option<&'static str>) {
        use crate::error::ProviderErrorKind;

        // Bedrock returns ThrottlingException in the JSON body for rate
        // limiting, even on 4xx status codes.  ServiceUnavailableException
        // is retryable server overload.
        if body.contains("ThrottlingException") || body.contains("TooManyRequestsException") {
            return (
                ProviderErrorKind::RateLimit,
                Some("Bedrock throttled the request — back off and retry"),
            );
        }
        if body.contains("ServiceUnavailableException")
            || body.contains("ModelStreamErrorException")
        {
            return (
                ProviderErrorKind::Server,
                Some("Bedrock service temporarily unavailable — retry with backoff"),
            );
        }
        if body.contains("AccessDeniedException") {
            return (
                ProviderErrorKind::Auth,
                Some("Bedrock access denied — check IAM policy for bedrock:InvokeModel*"),
            );
        }
        if body.contains("ModelNotReadyException") {
            return (
                ProviderErrorKind::Server,
                Some("Bedrock model not ready — the model may be warming up, retry shortly"),
            );
        }
        if body.contains("ValidationException") {
            return (ProviderErrorKind::BadRequest, None);
        }
        super::default_classify_status(status)
    }

    async fn authorize(
        &self,
        req: reqwest::RequestBuilder,
        body_bytes: &[u8],
    ) -> Result<reqwest::RequestBuilder> {
        let auth = self.auth.read().await;
        match &*auth {
            BedrockAuth::Bearer(token) => {
                Ok(req.header("authorization", format!("Bearer {}", token.expose_secret())))
            }
            BedrockAuth::SigV4(_) => {
                drop(auth);
                // Reconstruct the URL from the request to feed into the
                // SigV4 signer. We use a clone trick: build the request
                // once to read its URL, then re-attach all signed headers
                // to a fresh builder that the caller will further extend
                // with body bytes.
                let built = req
                    .try_clone()
                    .ok_or_else(|| Error::auth("bedrock: request not cloneable for sigv4"))?
                    .build()
                    .map_err(|e| Error::auth(format!("bedrock build for sigv4: {e}")))?;
                let url = built.url().to_string();
                let signed = self.sigv4_headers(&url, body_bytes).await?;
                let mut req = req;
                for (name, value) in signed {
                    req = req.header(&name, &value);
                }
                Ok(req)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::codec::{BedrockConverseCodec, ModelCodec};

    fn fake_transport(region: &str) -> BedrockTransport {
        BedrockTransport {
            region: region.to_string(),
            use_global_endpoint: false,
            auth: RwLock::new(BedrockAuth::Bearer(SecretString::from("test-token"))),
        }
    }

    #[tokio::test]
    async fn unary_url_uses_converse() {
        let t = fake_transport("us-west-2");
        let codec = BedrockConverseCodec::new();
        let ep = t
            .resolve_endpoint(
                codec.endpoint_shape(),
                "anthropic.claude-sonnet-4-5-20250929-v1:0",
                InvocationMode::Unary,
            )
            .await
            .unwrap();
        assert_eq!(
            ep.url,
            "https://bedrock-runtime.us-west-2.amazonaws.com/model/anthropic.claude-sonnet-4-5-20250929-v1%3A0/converse"
        );
        assert!(ep.headers.iter().any(|(k, _)| k == "content-type"));
        // Unary should not include the eventstream Accept header.
        assert!(
            !ep.headers
                .iter()
                .any(|(_, v)| v == "application/vnd.amazon.eventstream")
        );
    }

    #[tokio::test]
    async fn stream_url_uses_converse_stream_with_eventstream_accept() {
        let t = fake_transport("us-west-2");
        let codec = BedrockConverseCodec::new();
        let ep = t
            .resolve_endpoint(
                codec.endpoint_shape(),
                "anthropic.claude-sonnet-4-5-20250929-v1:0",
                InvocationMode::Stream,
            )
            .await
            .unwrap();
        assert!(ep.url.ends_with("/converse-stream"));
        assert!(
            ep.headers
                .iter()
                .any(|(k, v)| k == "accept" && v == "application/vnd.amazon.eventstream")
        );
    }

    #[tokio::test]
    async fn global_endpoint_host() {
        let mut t = fake_transport("us-east-1");
        t = t.with_global_endpoint();
        let codec = BedrockConverseCodec::new();
        let ep = t
            .resolve_endpoint(codec.endpoint_shape(), "x", InvocationMode::Unary)
            .await
            .unwrap();
        assert!(ep.url.starts_with("https://bedrock-runtime.amazonaws.com/"));
    }

    #[tokio::test]
    async fn supports_codec_filter() {
        let t = fake_transport("us-east-1");
        assert!(t.supports_codec("bedrock-converse"));
        assert!(!t.supports_codec("anthropic-messages"));
        assert!(!t.supports_codec("openai-chat"));
        assert!(!t.supports_codec("gemini-generate"));
    }

    #[tokio::test]
    async fn bearer_authorize_sets_authorization_header() {
        let t = fake_transport("us-east-1");
        let req = reqwest::Client::new().post("https://x");
        let built = t.authorize(req, b"{}").await.unwrap().build().unwrap();
        assert_eq!(
            built.headers().get("authorization").unwrap(),
            "Bearer test-token"
        );
    }

    #[test]
    fn classify_error_throttling() {
        let t = fake_transport("us-east-1");
        let (kind, hint) = t.classify_error(
            429,
            r#"{"__type":"ThrottlingException","message":"Rate exceeded"}"#,
        );
        assert!(matches!(kind, crate::error::ProviderErrorKind::RateLimit));
        assert!(hint.is_some());
    }

    #[test]
    fn classify_error_access_denied() {
        let t = fake_transport("us-east-1");
        let (kind, hint) = t.classify_error(
            403,
            r#"{"__type":"AccessDeniedException","message":"not authorized"}"#,
        );
        assert!(matches!(kind, crate::error::ProviderErrorKind::Auth));
        assert!(hint.unwrap().contains("IAM"));
    }

    #[test]
    fn classify_error_generic_500_fallback() {
        let t = fake_transport("us-east-1");
        let (kind, hint) = t.classify_error(500, "Internal Server Error");
        assert!(matches!(kind, crate::error::ProviderErrorKind::Server));
        assert!(hint.is_none());
    }

    #[test]
    fn classify_error_service_unavailable() {
        let t = fake_transport("us-east-1");
        let (kind, hint) = t.classify_error(
            503,
            r#"{"__type":"ServiceUnavailableException","message":"service down"}"#,
        );
        assert!(matches!(kind, crate::error::ProviderErrorKind::Server));
        assert!(hint.unwrap().contains("retry"));
    }

    #[test]
    fn classify_error_model_not_ready() {
        let t = fake_transport("us-east-1");
        let (kind, hint) = t.classify_error(
            503,
            r#"{"__type":"ModelNotReadyException","message":"model warming up"}"#,
        );
        assert!(matches!(kind, crate::error::ProviderErrorKind::Server));
        assert!(hint.unwrap().contains("warming up"));
    }

    #[test]
    fn classify_error_validation() {
        let t = fake_transport("us-east-1");
        let (kind, hint) = t.classify_error(
            400,
            r#"{"__type":"ValidationException","message":"invalid input"}"#,
        );
        assert!(matches!(kind, crate::error::ProviderErrorKind::BadRequest));
        assert!(hint.is_none());
    }
}
