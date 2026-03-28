use secrecy::SecretString;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    ApiKey,
    OAuth,
    Cloud,
}

#[derive(Debug, Clone)]
pub struct CredentialRecord {
    pub kind: CredentialKind,
    pub value: SecretString,
    pub source: String,
}

impl CredentialRecord {
    pub fn new(kind: CredentialKind, value: SecretString, source: impl Into<String>) -> Self {
        Self {
            kind,
            value,
            source: source.into(),
        }
    }
}
