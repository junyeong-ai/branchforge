//! Human-in-the-loop approval for tool execution.
//!
//! When a tool requires human review (e.g., in [`ExecutionMode::Supervised`]
//! or [`ExecutionMode::SupervisedFor`]), the agent sends an [`ApprovalRequest`]
//! through a bounded channel and waits for an [`ApprovalResponse`].
//!
//! # Example
//!
//! ```rust,no_run
//! use branchforge::authorization::approval::{approval_channel, ApprovalResponse};
//!
//! # #[tokio::main] async fn main() {
//! let (sender, mut receiver) = approval_channel(16);
//!
//! // In a separate task, handle approval requests:
//! tokio::spawn(async move {
//!     while let Some((request, responder)) = receiver.recv().await {
//!         println!("Tool '{}' wants to run. Approve? ", request.tool_name);
//!         let _ = responder.send(ApprovalResponse::Approve);
//!     }
//! });
//! # }
//! ```

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

/// Request sent to the approval channel when a tool needs human review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// Name of the tool requesting approval.
    pub tool_name: String,
    /// Provider-assigned tool call identifier.
    pub tool_call_id: String,
    /// The input arguments the tool will receive.
    pub tool_input: serde_json::Value,
    /// Human-readable reason why approval is required.
    pub reason: String,
}

/// Human response to an [`ApprovalRequest`].
#[derive(Debug, Clone)]
pub enum ApprovalResponse {
    /// Allow the tool to execute.
    Approve,
    /// Deny execution with an explanation.
    Deny { reason: String },
}

/// Sender half of the approval channel.
///
/// Pass this to [`AgentBuilder::approval_channel`] so the agent runtime
/// can send approval requests during tool execution.
pub type ApprovalSender = mpsc::Sender<(ApprovalRequest, oneshot::Sender<ApprovalResponse>)>;

/// Receiver half of the approval channel.
///
/// The host application reads from this to present approval prompts to
/// the human operator.
pub type ApprovalReceiver = mpsc::Receiver<(ApprovalRequest, oneshot::Sender<ApprovalResponse>)>;

/// Create a bounded approval channel.
///
/// `buffer` controls how many pending approval requests can be queued
/// before the agent blocks. A value of 8-16 is typical for interactive
/// use; larger values are useful for batch pipelines where a single
/// reviewer processes approvals asynchronously.
pub fn approval_channel(buffer: usize) -> (ApprovalSender, ApprovalReceiver) {
    mpsc::channel(buffer)
}

/// Default timeout in seconds for waiting on human approval.
///
/// If no response arrives within this window the tool execution is
/// treated as denied with a timeout reason.
pub const DEFAULT_APPROVAL_TIMEOUT_SECS: u64 = 30;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approval_channel_approve_roundtrip() {
        let (tx, mut rx) = approval_channel(1);

        let request = ApprovalRequest {
            tool_name: "Bash".into(),
            tool_call_id: "tc_1".into(),
            tool_input: serde_json::json!({"command": "ls"}),
            reason: "Supervised mode".into(),
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send((request.clone(), resp_tx)).await.unwrap();

        let (received, responder) = rx.recv().await.unwrap();
        assert_eq!(received.tool_name, "Bash");
        responder.send(ApprovalResponse::Approve).unwrap();

        let response = resp_rx.await.unwrap();
        assert!(matches!(response, ApprovalResponse::Approve));
    }

    #[tokio::test]
    async fn approval_channel_deny_roundtrip() {
        let (tx, mut rx) = approval_channel(1);

        let request = ApprovalRequest {
            tool_name: "Write".into(),
            tool_call_id: "tc_2".into(),
            tool_input: serde_json::json!({"file_path": "/etc/passwd"}),
            reason: "Supervised mode".into(),
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        tx.send((request, resp_tx)).await.unwrap();

        let (_, responder) = rx.recv().await.unwrap();
        responder
            .send(ApprovalResponse::Deny {
                reason: "Not allowed".into(),
            })
            .unwrap();

        let response = resp_rx.await.unwrap();
        assert!(matches!(response, ApprovalResponse::Deny { .. }));
    }

    #[test]
    fn approval_request_serialization() {
        let request = ApprovalRequest {
            tool_name: "Bash".into(),
            tool_call_id: "tc_1".into(),
            tool_input: serde_json::json!({"command": "git status"}),
            reason: "Supervised mode".into(),
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["tool_name"], "Bash");
        assert_eq!(json["tool_call_id"], "tc_1");
        assert_eq!(json["reason"], "Supervised mode");

        let roundtrip: ApprovalRequest = serde_json::from_value(json).unwrap();
        assert_eq!(roundtrip.tool_name, request.tool_name);
    }
}
