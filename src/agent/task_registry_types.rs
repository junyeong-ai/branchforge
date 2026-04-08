//! Data types and the in-memory runtime entry for [`TaskRegistry`].
//!
//! These are the small, mostly-data structs that the registry stores
//! and that downstream code (subagents, persistence backends, public
//! API consumers) needs to reference. Splitting them out keeps
//! `task_registry.rs` focused on the registry's behaviour.

use std::sync::Arc;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::ir::{ContentPart, FinishReason};
use crate::session::{SessionState, ThinkingMetadata, ToolResultMeta};

use super::AgentResult;

/// Pending state transition awaiting reconciliation against persisted
/// session state. Created when a task finishes (success/failure/cancel)
/// but the registry has not yet committed the terminal state to the
/// underlying [`crate::session::Persistence`].
///
/// `Box<AgentResult>` is used because `AgentResult` is large and the
/// surrounding `TaskRuntime` would otherwise inflate every entry in the
/// registry's HashMap.
#[derive(Clone)]
pub(super) enum PendingTaskTransition {
    Completed(Box<AgentResult>),
    Failed(String),
    Cancelled,
}

impl PendingTaskTransition {
    pub(super) fn intent_state(&self) -> SessionState {
        match self {
            Self::Completed(_) => SessionState::Completing,
            Self::Failed(_) => SessionState::Failing,
            Self::Cancelled => SessionState::Cancelling,
        }
    }

    pub(super) fn terminal_state(&self) -> SessionState {
        match self {
            Self::Completed(_) => SessionState::Completed,
            Self::Failed(_) => SessionState::Failed,
            Self::Cancelled => SessionState::Cancelled,
        }
    }
}

/// One row in the registry's in-memory runtime map. Tracks the spawned
/// tokio task, its cancel channel, any pending state transition, and
/// whether the entry occupies a background-execution slot.
pub(super) struct TaskRuntime {
    pub(super) handle: Option<JoinHandle<()>>,
    pub(super) cancel_tx: Option<oneshot::Sender<()>>,
    pub(super) pending_transition: Option<PendingTaskTransition>,
    pub(super) background_slot: bool,
}

/// Metadata captured for the assistant message produced by a task. This
/// is what consumers see when they query the registry for a finished
/// task's result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAssistantMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_results: Option<Vec<ToolResultMeta>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingMetadata>,
}

/// Aggregate execution statistics for a finished task: token usage,
/// timing, iteration count, and cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExecutionSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<FinishReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iterations: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::ir::Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_calls: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compactions: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<Decimal>,
}

/// Snapshot of a task's terminal state: status, content, structured
/// output, metadata, execution summary, and any error message. This is
/// the public-facing payload returned by [`TaskRegistry::result`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResultSnapshot {
    pub status: SessionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentPart>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_metadata: Option<TaskAssistantMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<TaskExecutionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// Forward declaration so the doc-comment on TaskResultSnapshot can refer
// to the registry's `result` method without a circular import.
#[allow(dead_code)]
pub(super) struct _TaskRegistryDocMarker(Arc<()>);
