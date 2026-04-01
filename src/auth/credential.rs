//! Credential types.

use std::fmt;

use chrono::{DateTime, Duration, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

mod secret_serde {
    use secrecy::{ExposeSecret, SecretString};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        secret: &SecretString,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(secret.expose_secret())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SecretString, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(SecretString::from(s))
    }
}

mod option_secret_serde {
    use secrecy::{ExposeSecret, SecretString};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        secret: &Option<SecretString>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match secret {
            Some(s) => serializer.serialize_some(s.expose_secret()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SecretString>, D::Error> {
        let opt = Option::<String>::deserialize(deserializer)?;
        Ok(opt.map(SecretString::from))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthCredential {
    #[serde(with = "secret_serde")]
    pub access_token: SecretString,
    #[serde(
        with = "option_secret_serde",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub refresh_token: Option<SecretString>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<String>,
}

impl fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthCredential")
            .field("access_token", &"[redacted]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .field("subscription_type", &self.subscription_type)
            .finish()
    }
}

impl OAuthCredential {
    pub fn expires_at_datetime(&self) -> Option<DateTime<Utc>> {
        self.expires_at.and_then(|ts| {
            // Claude Code CLI stores expires_at in milliseconds,
            // while standard OAuth2 uses seconds. Detect and normalize:
            // timestamps > year 2100 in seconds (~4102444800) are likely milliseconds.
            let ts_secs = if ts > 4_102_444_800 { ts / 1000 } else { ts };
            DateTime::from_timestamp(ts_secs, 0).or_else(|| {
                tracing::warn!(
                    timestamp = ts,
                    "Invalid expires_at timestamp, treating as expired"
                );
                None
            })
        })
    }

    pub fn is_expired(&self) -> bool {
        match (self.expires_at, self.expires_at_datetime()) {
            (Some(_), None) => true,
            (_, Some(exp)) => Utc::now() >= exp,
            (None, None) => false,
        }
    }

    /// Returns true within 5 minutes of expiry.
    pub fn needs_refresh(&self) -> bool {
        match (self.expires_at, self.expires_at_datetime()) {
            (Some(_), None) => true,
            (_, Some(exp)) => Utc::now() >= exp - Duration::minutes(5),
            (None, None) => false,
        }
    }
}

#[derive(Clone)]
pub enum Credential {
    ApiKey(SecretString),
    OAuth(OAuthCredential),
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey(_) => f.debug_tuple("ApiKey").field(&"[redacted]").finish(),
            Self::OAuth(oauth) => f.debug_tuple("OAuth").field(oauth).finish(),
        }
    }
}

impl Credential {
    /// Create a placeholder credential for cloud providers that handle
    /// authentication through their own token mechanisms (Bedrock, Vertex, Foundry).
    pub fn placeholder() -> Self {
        Self::ApiKey(SecretString::from(""))
    }

    pub fn api_key(key: impl Into<String>) -> Self {
        Self::ApiKey(SecretString::from(key.into()))
    }

    pub fn oauth(token: impl Into<String>) -> Self {
        Self::OAuth(OAuthCredential {
            access_token: SecretString::from(token.into()),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
            subscription_type: None,
        })
    }

    pub fn is_placeholder(&self) -> bool {
        match self {
            Self::ApiKey(key) => key.expose_secret().is_empty(),
            Self::OAuth(oauth) => oauth.access_token.expose_secret().is_empty(),
        }
    }

    pub fn is_expired(&self) -> bool {
        match self {
            Credential::ApiKey(_) => false,
            Credential::OAuth(oauth) => oauth.is_expired(),
        }
    }

    pub fn needs_refresh(&self) -> bool {
        match self {
            Credential::ApiKey(_) => false,
            Credential::OAuth(oauth) => oauth.needs_refresh(),
        }
    }

    pub fn credential_type(&self) -> &'static str {
        match self {
            Credential::ApiKey(_) => "api_key",
            Credential::OAuth(_) => "oauth",
        }
    }

    pub fn is_oauth(&self) -> bool {
        matches!(self, Credential::OAuth(_))
    }

    pub fn is_api_key(&self) -> bool {
        matches!(self, Credential::ApiKey(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_key_credential() {
        let cred = Credential::api_key("sk-ant-api-test");
        assert!(!cred.is_expired());
        assert!(!cred.needs_refresh());
        assert_eq!(cred.credential_type(), "api_key");
    }

    #[test]
    fn test_oauth_credential() {
        let cred = Credential::oauth("sk-ant-oat01-test");
        assert_eq!(cred.credential_type(), "oauth");
    }

    #[test]
    fn test_oauth_expiry() {
        let expired = OAuthCredential {
            access_token: SecretString::from("test"),
            refresh_token: None,
            expires_at: Some(0),
            scopes: vec![],
            subscription_type: None,
        };
        assert!(expired.is_expired());

        let future = OAuthCredential {
            access_token: SecretString::from("test"),
            refresh_token: None,
            expires_at: Some(Utc::now().timestamp() + 3600),
            scopes: vec![],
            subscription_type: None,
        };
        assert!(!future.is_expired());

        // Claude Code CLI stores expires_at in milliseconds
        let expired_ms = OAuthCredential {
            access_token: SecretString::from("test"),
            refresh_token: None,
            expires_at: Some(1_000), // 1 second in ms, epoch = expired
            scopes: vec![],
            subscription_type: None,
        };
        assert!(expired_ms.is_expired());

        let future_ms = OAuthCredential {
            access_token: SecretString::from("test"),
            refresh_token: None,
            expires_at: Some((Utc::now().timestamp() + 3600) * 1000), // 1 hour from now in ms
            scopes: vec![],
            subscription_type: None,
        };
        assert!(!future_ms.is_expired());
    }

    #[test]
    fn test_oauth_needs_refresh_milliseconds() {
        // Token expiring in 2 minutes (inside 5-min window) — in milliseconds
        let expiring_soon_ms = OAuthCredential {
            access_token: SecretString::from("test"),
            refresh_token: None,
            expires_at: Some((Utc::now().timestamp() + 120) * 1000),
            scopes: vec![],
            subscription_type: None,
        };
        assert!(expiring_soon_ms.needs_refresh());

        // Token expiring in 10 minutes (outside 5-min window) — in milliseconds
        let not_yet_ms = OAuthCredential {
            access_token: SecretString::from("test"),
            refresh_token: None,
            expires_at: Some((Utc::now().timestamp() + 600) * 1000),
            scopes: vec![],
            subscription_type: None,
        };
        assert!(!not_yet_ms.needs_refresh());
    }

    #[test]
    fn test_credential_debug_redacts_secrets() {
        let cred = Credential::api_key("super-secret-key");
        let debug = format!("{:?}", cred);
        assert!(!debug.contains("super-secret-key"));
        assert!(debug.contains("[redacted]"));
    }

    #[test]
    fn test_oauth_debug_redacts_tokens() {
        let oauth = OAuthCredential {
            access_token: SecretString::from("secret-token"),
            refresh_token: Some(SecretString::from("secret-refresh")),
            expires_at: None,
            scopes: vec![],
            subscription_type: None,
        };
        let debug = format!("{:?}", oauth);
        assert!(!debug.contains("secret-token"));
        assert!(!debug.contains("secret-refresh"));
        assert!(debug.contains("[redacted]"));
    }
}
