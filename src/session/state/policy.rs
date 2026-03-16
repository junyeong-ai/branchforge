//! Session-level authorization configuration for storage.
//!
//! These types are simplified serializable versions for session persistence.
//! For runtime tool policy checking with rules, see `crate::authorization::ToolPolicy`.
//! For runtime execution mode, see `crate::authorization::ExecutionMode`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Serializable execution mode for session persistence.
///
/// Maps to [`crate::authorization::ExecutionMode`] at runtime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionExecutionMode {
    /// Tools execute automatically when policy allows.
    #[default]
    Auto,
    /// Exploration only — read/navigation tools only.
    Plan,
    /// All tools require user review.
    Supervised,
}

/// Session-level authorization configuration.
///
/// This is a simplified, serializable version for session storage.
/// For runtime tool policy checking with rule patterns, use `crate::authorization::ToolPolicy`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAuthorization {
    pub mode: SessionExecutionMode,
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
