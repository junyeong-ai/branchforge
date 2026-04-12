//! Unified human-in-the-loop (HITL) channel for the agent runtime.
//!
//! # Motivation
//!
//! Prior to Phase D Workstream C-1, the agent runtime spoke to human
//! operators through a single special-purpose mpsc channel
//! (`ApprovalSender`) that only carried tool-approval requests. Two
//! upcoming features — `AskUserQuestion` (a structured HITL tool) and
//! MCP `elicitation` (server-initiated user prompts) — each wanted
//! their own channel, which would have produced three parallel
//! delivery mechanisms for the same underlying concept ("the agent
//! needs a human to answer something right now"). That is exactly
//! the "no dual systems" anti-pattern called out in
//! `.claude/rules/naming.md`.
//!
//! # Design
//!
//! [`HumanInteractionHandler`] is a single `async_trait` with one
//! default-stub method per kind of interaction. A host application
//! (CLI REPL, API server, test harness) implements the trait once
//! and plugs the handler into the agent via
//! `AgentBuilder::human_handler`. Default method impls return
//! [`HumanInteractionError::NotSupported`] so a host that only cares
//! about one kind of interaction writes one method and gets
//! fail-closed behaviour on the rest — no boilerplate.
//!
//! Adding a new interaction kind is additive: add a trait method
//! with a `NotSupported` default. Existing handlers continue to
//! compile, and the runtime's hot path degrades to
//! "not-supported → fail closed" without code changes elsewhere.
//!
//! # Fail-closed contract
//!
//! Every method returns [`HumanInteractionResult`]. A runtime that
//! receives `NotSupported` **must** translate the error to a denied
//! decision at the call site (e.g. `ToolApprovalResponse::Deny`).
//! This is the "unknown capability is denied" invariant that keeps
//! pure-core builds (no handler wired) safe to run against
//! supervised-mode agents.

#![allow(missing_docs)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Request for a tool-approval decision. Issued by the agent when
/// a tool is about to execute under `ExecutionMode::Supervised`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolApprovalRequest {
    pub tool_name: String,
    pub tool_call_id: String,
    pub tool_input: serde_json::Value,
    /// Human-readable reason the agent is asking for approval.
    pub reason: String,
}

/// Response to a [`ToolApprovalRequest`].
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum ToolApprovalResponse {
    Approve,
    Deny { reason: String },
}

/// A single structured question inside a [`QuestionRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    /// The question text presented to the user.
    pub text: String,
    /// Selectable options. Empty means "free-form answer" (host may
    /// reject that if it does not support free-form input).
    pub options: Vec<String>,
    /// `true` when the user can pick multiple options at once.
    #[serde(default)]
    pub multi_select: bool,
    /// Optional preview string rendered alongside the question
    /// (e.g. a diff, a URL preview, a short summary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Batch of structured questions asked by the `AskUserQuestion` tool
/// (Phase D Workstream C-2). The host renders all questions in a
/// single interaction and returns the user's selections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionRequest {
    pub questions: Vec<Question>,
}

/// Response to a [`QuestionRequest`]. `selections[i]` contains the
/// indices the user chose for `questions[i]`. An empty inner vec
/// means the user skipped that question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionResponse {
    pub selections: Vec<Vec<usize>>,
}

/// MCP `elicitation/create` request translated into the unified
/// HITL channel. Used by the MCP client when a remote server asks
/// the host to prompt the user (Phase D Workstream C-3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElicitationRequest {
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

/// Response to an [`ElicitationRequest`]. `value` must validate
/// against the request's `schema` when one was supplied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElicitationResponse {
    pub value: serde_json::Value,
}

/// Errors returned by [`HumanInteractionHandler`] methods. Every
/// variant represents a failure to obtain a human decision — the
/// caller must translate them into a fail-closed deny/abort action.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum HumanInteractionError {
    /// The handler does not implement this interaction kind. This
    /// is what the default trait method impls return, and the
    /// fail-closed contract requires the runtime to translate it
    /// into a denied decision.
    #[error("human interaction not supported by handler: {0}")]
    NotSupported(&'static str),
    /// The handler returned an error (wrapped as a string so the
    /// trait object stays dyn-compatible).
    #[error("human interaction handler error: {0}")]
    Handler(String),
    /// The host took too long to respond. The runtime typically
    /// wraps the handler call in a `tokio::time::timeout` and
    /// converts elapsed timeouts to this variant.
    #[error("human interaction timed out")]
    Timeout,
}

/// Result type returned by every [`HumanInteractionHandler`] method.
pub type HumanInteractionResult<T> = std::result::Result<T, HumanInteractionError>;

/// [`crate::common::Extensions`] entry carrying a shared
/// [`HumanInteractionHandler`] into the [`crate::tools::ExecutionContext`].
///
/// Tools that need human interaction (e.g. the built-in
/// `AskUserQuestion` tool) read the handler via
/// `ctx.extensions().get::<HumanInteractionExtension>()` and invoke
/// its methods. The extension wraps an `Arc<dyn HumanInteractionHandler>`
/// so cloning it across async boundaries is cheap.
#[derive(Clone)]
pub struct HumanInteractionExtension(pub std::sync::Arc<dyn HumanInteractionHandler>);

impl std::fmt::Debug for HumanInteractionExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("HumanInteractionExtension")
            .field(&self.0.name())
            .finish()
    }
}

impl HumanInteractionExtension {
    pub fn new(handler: std::sync::Arc<dyn HumanInteractionHandler>) -> Self {
        Self(handler)
    }

    pub fn handler(&self) -> &std::sync::Arc<dyn HumanInteractionHandler> {
        &self.0
    }
}

/// Unified handler for every kind of human-in-the-loop interaction
/// the agent runtime can initiate: tool-approval decisions,
/// structured questions (AskUserQuestion), and MCP elicitation.
///
/// Host applications implement this trait once. Default method
/// impls return [`HumanInteractionError::NotSupported`] so a host
/// that only supports approval (for example) can implement
/// [`Self::approve_tool`] and leave the rest untouched; the runtime
/// degrades gracefully to fail-closed for the unsupported kinds.
#[async_trait]
pub trait HumanInteractionHandler: Send + Sync + std::fmt::Debug {
    /// Stable name for logs and span attributes.
    fn name(&self) -> &str {
        "human_interaction_handler"
    }

    /// Approve or deny a tool invocation. Called when the agent is
    /// in `ExecutionMode::Supervised` or `SupervisedFor`.
    async fn approve_tool(
        &self,
        request: ToolApprovalRequest,
    ) -> HumanInteractionResult<ToolApprovalResponse> {
        let _ = request;
        Err(HumanInteractionError::NotSupported("approve_tool"))
    }

    /// Ask the user a batch of structured questions. Called by the
    /// `AskUserQuestion` tool (Phase D Workstream C-2).
    async fn ask_question(
        &self,
        request: QuestionRequest,
    ) -> HumanInteractionResult<QuestionResponse> {
        let _ = request;
        Err(HumanInteractionError::NotSupported("ask_question"))
    }

    /// Respond to an MCP server's `elicitation/create` request.
    /// Called by the MCP client (Phase D Workstream C-3).
    async fn elicit(
        &self,
        request: ElicitationRequest,
    ) -> HumanInteractionResult<ElicitationResponse> {
        let _ = request;
        Err(HumanInteractionError::NotSupported("elicit"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Debug)]
    struct NoopHandler;

    #[async_trait]
    impl HumanInteractionHandler for NoopHandler {}

    #[tokio::test]
    async fn default_methods_return_not_supported() {
        let handler: Arc<dyn HumanInteractionHandler> = Arc::new(NoopHandler);

        let err = handler
            .approve_tool(ToolApprovalRequest {
                tool_name: "Bash".into(),
                tool_call_id: "tc_1".into(),
                tool_input: serde_json::json!({}),
                reason: "test".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            HumanInteractionError::NotSupported("approve_tool")
        ));

        let err = handler
            .ask_question(QuestionRequest { questions: vec![] })
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            HumanInteractionError::NotSupported("ask_question")
        ));

        let err = handler
            .elicit(ElicitationRequest {
                prompt: "pick one".into(),
                schema: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, HumanInteractionError::NotSupported("elicit")));
    }

    #[derive(Debug)]
    struct AutoApprover;

    #[async_trait]
    impl HumanInteractionHandler for AutoApprover {
        fn name(&self) -> &str {
            "auto_approver"
        }
        async fn approve_tool(
            &self,
            _req: ToolApprovalRequest,
        ) -> HumanInteractionResult<ToolApprovalResponse> {
            Ok(ToolApprovalResponse::Approve)
        }
    }

    #[tokio::test]
    async fn implementers_can_override_one_method() {
        let handler: Arc<dyn HumanInteractionHandler> = Arc::new(AutoApprover);
        assert_eq!(handler.name(), "auto_approver");

        let resp = handler
            .approve_tool(ToolApprovalRequest {
                tool_name: "Bash".into(),
                tool_call_id: "tc_1".into(),
                tool_input: serde_json::json!({}),
                reason: "test".into(),
            })
            .await
            .unwrap();
        assert!(matches!(resp, ToolApprovalResponse::Approve));

        // Other methods still fail closed.
        assert!(
            handler
                .ask_question(QuestionRequest { questions: vec![] })
                .await
                .is_err()
        );
    }
}
