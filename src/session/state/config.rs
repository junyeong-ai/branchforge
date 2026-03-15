//! Session configuration.

use serde::{Deserialize, Serialize};

use super::policy::SessionAuthorization;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub model: String,
    pub max_tokens: u32,
    #[serde(default)]
    pub authorization: SessionAuthorization,
    pub ttl_secs: Option<u64>,
    pub system_prompt: Option<String>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 16384,
            authorization: SessionAuthorization::default(),
            ttl_secs: None,
            system_prompt: None,
        }
    }
}
