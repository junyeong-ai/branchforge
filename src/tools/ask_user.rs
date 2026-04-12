//! `AskUserQuestion` — structured human-in-the-loop clarification tool.
//!
//! # Why it exists
//!
//! LLM agents operating in non-trivial domains (customer support,
//! survey analysis, data exploration, configuration wizards) often
//! need to ask the user for a structured clarification — "which of
//! these three filter modes do you want?", "which of these files
//! should I edit?", "approve this list of changes?". Prior to
//! Phase D, the only escape hatch was free-form text in the
//! assistant response, which the host had to parse out of prose.
//!
//! `AskUserQuestion` turns that escape hatch into a first-class
//! SDK primitive: the model emits a typed list of questions with
//! options, the runtime routes it through the unified
//! [`crate::authorization::HumanInteractionHandler`] channel (the
//! same channel that carries tool-approval decisions and MCP
//! elicitation in Phase D C-1/C-3), and the user's selections come
//! back to the model as a deterministic JSON payload.
//!
//! # General-purpose design
//!
//! This tool is Layer 1 (no filesystem, no shell, no coding-agent
//! assumptions). It is registered by default in the `Core` tool
//! surface so every agent — research, support, coding, analyst —
//! picks it up automatically.
//!
//! Hosts that do not wire a [`crate::authorization::HumanInteractionHandler`]
//! receive a fail-closed `ToolOutput::error` pointing at
//! `AgentBuilder::human_handler`. The tool is thus safe to expose
//! in unattended / batch deployments — it simply fails loudly
//! instead of hanging.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::SchemaTool;
use super::context::ExecutionContext;
use crate::authorization::{
    HumanInteractionError, HumanInteractionExtension, Question, QuestionRequest,
};
use crate::types::ToolResult;

/// Model-facing schema for one question inside an
/// [`AskUserQuestionInput`]. Mirrors
/// [`crate::authorization::Question`] but carries a
/// `JsonSchema` derivation so the tool schema is auto-generated.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskUserQuestionItem {
    /// The question text shown to the user.
    pub text: String,
    /// Selectable options. Empty means free-form answer; the host
    /// may reject that if it does not support free-form input, in
    /// which case the tool surfaces the error as a `ToolResult::error`.
    #[serde(default)]
    pub options: Vec<String>,
    /// `true` if the user may select multiple options at once.
    #[serde(default)]
    pub multi_select: bool,
    /// Optional preview text (e.g. a diff, a URL preview, a short
    /// summary of what the options mean).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Batch of structured questions issued by the model.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AskUserQuestionInput {
    /// One or more questions to ask the user. The host renders them
    /// together as a single interaction and returns all answers at
    /// once — this minimises round-trips.
    pub questions: Vec<AskUserQuestionItem>,
}

/// Built-in [`crate::tools::Tool`] that routes structured questions
/// through the unified HITL channel.
#[derive(Debug, Default, Clone, Copy)]
pub struct AskUserQuestionTool;

#[async_trait]
impl SchemaTool for AskUserQuestionTool {
    type Input = AskUserQuestionInput;

    const NAME: &'static str = "AskUserQuestion";
    const SEARCH_HINT: Option<&'static str> =
        Some("ask the user a structured multi-choice question");
    const DESCRIPTION: &'static str = r#"Ask the user one or more structured multi-choice questions and receive their selections.

Use this tool when you need the user to disambiguate between choices before proceeding: "which of these three options do you want?", "which files should I edit?", "approve this plan?". Do NOT use this tool for free-form conversation — continue the assistant response with plain text for that.

Each question in the `questions` array has:
- `text`: the prompt shown to the user
- `options`: the selectable choices (an empty array means free-form answer, which the host may reject)
- `multi_select`: true when more than one option may be picked
- `preview` (optional): a short preview / diff / URL / summary rendered alongside the question

The tool returns a JSON object with a `selections` array of arrays: `selections[i]` is the list of option indices (0-based) the user picked for `questions[i]`. An empty inner array means the user skipped or declined that question.

The host application is responsible for rendering the questions and collecting the user's response. When no host is wired (unattended / batch mode), the tool returns an error immediately rather than hanging."#;

    async fn handle(&self, input: Self::Input, context: &ExecutionContext) -> ToolResult {
        // 1. Fail closed when no human handler is wired.
        let Some(ext) = context.extensions().get::<HumanInteractionExtension>() else {
            return ToolResult::error(
                "AskUserQuestion requires a HumanInteractionHandler. Wire one via \
                 AgentBuilder::human_handler(..) — the tool is safe to expose in \
                 unattended deployments and will return this error instead of hanging.",
            );
        };

        // 2. Reject empty batches early. An LLM that calls the tool
        //    with no questions is almost certainly confused; returning
        //    an error lets the recovery loop clarify instead of
        //    forwarding a degenerate request to the host.
        if input.questions.is_empty() {
            return ToolResult::error(
                "AskUserQuestion called with an empty questions array. Provide at least one question.",
            );
        }

        // 3. Translate the tool-facing schema into the neutral
        //    authorization-layer [`Question`] type. Keeps the tool's
        //    wire schema decoupled from the HITL channel's internal
        //    representation — we can evolve one without breaking
        //    the other.
        let request = QuestionRequest {
            questions: input
                .questions
                .into_iter()
                .map(|q| Question {
                    text: q.text,
                    options: q.options,
                    multi_select: q.multi_select,
                    preview: q.preview,
                })
                .collect(),
        };

        // 4. Route through the unified handler and translate errors
        //    to `ToolResult::error` so the model sees the failure as
        //    a normal tool-result (not an exception).
        match ext.handler().ask_question(request).await {
            Ok(response) => match serde_json::to_string(&response) {
                Ok(json) => ToolResult::success(json),
                Err(e) => ToolResult::error(format!(
                    "AskUserQuestion failed to serialise the handler response: {e}"
                )),
            },
            Err(HumanInteractionError::NotSupported(_)) => ToolResult::error(
                "The configured HumanInteractionHandler does not implement ask_question. \
                 Override the `ask_question` method on your HumanInteractionHandler impl.",
            ),
            Err(HumanInteractionError::Timeout) => {
                ToolResult::error("AskUserQuestion timed out waiting for the user")
            }
            Err(HumanInteractionError::Handler(msg)) => {
                ToolResult::error(format!("HumanInteractionHandler returned an error: {msg}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::{HumanInteractionHandler, HumanInteractionResult, QuestionResponse};
    use crate::tools::Tool;
    use std::sync::Arc;

    #[derive(Debug)]
    struct StubHandler {
        response: QuestionResponse,
    }

    #[async_trait]
    impl HumanInteractionHandler for StubHandler {
        async fn ask_question(
            &self,
            _req: QuestionRequest,
        ) -> HumanInteractionResult<QuestionResponse> {
            Ok(self.response.clone())
        }
    }

    fn ctx_with_handler(handler: Arc<dyn HumanInteractionHandler>) -> ExecutionContext {
        let mut ctx = ExecutionContext::empty();
        ctx.extensions_mut()
            .insert(HumanInteractionExtension::new(handler));
        ctx
    }

    #[tokio::test]
    async fn ask_user_question_routes_through_unified_handler() {
        let handler: Arc<dyn HumanInteractionHandler> = Arc::new(StubHandler {
            response: QuestionResponse {
                selections: vec![vec![0, 2]],
            },
        });
        let tool = AskUserQuestionTool;
        let ctx = ctx_with_handler(handler);

        let input = serde_json::json!({
            "questions": [
                {
                    "text": "Pick fruits",
                    "options": ["apple", "banana", "cherry"],
                    "multi_select": true,
                }
            ]
        });

        let result = tool.execute(input, &ctx).await;
        assert!(!result.is_error());
        let text = result.text();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["selections"], serde_json::json!([[0, 2]]));
    }

    #[tokio::test]
    async fn ask_user_question_fails_closed_without_handler() {
        let tool = AskUserQuestionTool;
        let ctx = ExecutionContext::empty();
        let input = serde_json::json!({
            "questions": [{"text": "Pick one", "options": ["a"]}]
        });
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_error());
        assert!(result.error_message().contains("HumanInteractionHandler"));
    }

    #[tokio::test]
    async fn ask_user_question_rejects_empty_questions_list() {
        #[derive(Debug)]
        struct PanicHandler;
        #[async_trait]
        impl HumanInteractionHandler for PanicHandler {
            async fn ask_question(
                &self,
                _req: QuestionRequest,
            ) -> HumanInteractionResult<QuestionResponse> {
                panic!("should never be called");
            }
        }
        let tool = AskUserQuestionTool;
        let ctx = ctx_with_handler(Arc::new(PanicHandler));
        let input = serde_json::json!({ "questions": [] });
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_error());
        assert!(result.error_message().contains("empty questions array"));
    }

    #[tokio::test]
    async fn ask_user_question_propagates_not_supported_as_tool_error() {
        #[derive(Debug)]
        struct ApprovalOnly;
        #[async_trait]
        impl HumanInteractionHandler for ApprovalOnly {
            // Uses default `ask_question` which returns NotSupported.
        }
        let tool = AskUserQuestionTool;
        let ctx = ctx_with_handler(Arc::new(ApprovalOnly));
        let input = serde_json::json!({
            "questions": [{"text": "x", "options": ["a"]}]
        });
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_error());
        assert!(result.error_message().contains("ask_question"));
    }
}
