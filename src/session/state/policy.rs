//! Session-level authorization configuration for storage.
//!
//! These types are simplified serializable versions for session persistence.
//! For runtime authorization checking with rules, see `crate::authorization::AuthorizationPolicy`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAuthorizationMode {
    #[default]
    Rules,
    AutoApproveFiles,
    AllowAll,
    ReadOnly,
}

/// Session-level authorization configuration.
///
/// This is a simplified, serializable version for session storage.
/// For runtime authorization checking with rule patterns, use `crate::authorization::AuthorizationPolicy`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAuthorization {
    pub mode: SessionAuthorizationMode,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub tool_limits: HashMap<String, SessionToolLimits>,
}

/// Session-level tool limits for storage.
///
/// For detailed runtime limits with path-based rules, see `crate::authorization::ToolLimits`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToolLimits {
    pub timeout_ms: Option<u64>,
    pub max_output_size: Option<usize>,
}
