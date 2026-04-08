//! Agent-domain types.
//!
//! Types that belong to the agent runtime contract — request metadata,
//! token budget defaults, and other knobs that aren't part of the
//! provider-neutral IR.

use serde::{Deserialize, Serialize};

/// Default maximum output tokens for a model request when no explicit
/// budget is set on the agent config.
pub const DEFAULT_MAX_TOKENS: u32 = 8192;

/// Minimum extended-thinking budget when reasoning is enabled.
///
/// Vendors that expose a thinking-budget parameter (Anthropic, OpenAI
/// Responses, Gemini Pro) all clamp values below ~1k tokens, so the SDK
/// rounds up to this floor on enable.
pub const MIN_THINKING_BUDGET: u32 = 1024;

/// Per-request metadata for SDK-level audit, attribution, and routing.
///
/// `user_id` is forwarded to the provider for abuse-tracking. `tenant_id`
/// and `session_id` are kept SDK-internal — they appear in logs and
/// observability spans but never on the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestMetadata {
    /// External user identifier — sent to the provider for abuse tracking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Tenant identifier — kept SDK-internal, never on the wire.
    #[serde(skip)]
    pub tenant_id: Option<String>,
    /// Session identifier — kept SDK-internal, never on the wire.
    #[serde(skip)]
    pub session_id: Option<String>,
}

impl RequestMetadata {
    /// Build request metadata from identity fields.
    ///
    /// Returns `None` if no `principal_id` is supplied — without an
    /// external identifier there is nothing to attach to the request.
    pub fn from_identity(
        tenant_id: Option<&str>,
        principal_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Option<Self> {
        let principal_id = principal_id?;
        Some(Self {
            user_id: Some(principal_id.to_string()),
            tenant_id: tenant_id.map(String::from),
            session_id: session_id.map(String::from),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_identity_requires_principal() {
        assert!(RequestMetadata::from_identity(Some("t"), None, None).is_none());
    }

    #[test]
    fn from_identity_populates_all_fields() {
        let m = RequestMetadata::from_identity(Some("t"), Some("u"), Some("s")).unwrap();
        assert_eq!(m.user_id.as_deref(), Some("u"));
        assert_eq!(m.tenant_id.as_deref(), Some("t"));
        assert_eq!(m.session_id.as_deref(), Some("s"));
    }

    #[test]
    fn tenant_and_session_not_serialized() {
        let m = RequestMetadata::from_identity(Some("t"), Some("u"), Some("s")).unwrap();
        let json = serde_json::to_value(&m).unwrap();
        assert!(json.get("user_id").is_some());
        assert!(json.get("tenant_id").is_none());
        assert!(json.get("session_id").is_none());
    }
}
