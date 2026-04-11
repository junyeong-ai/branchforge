//! Integration regression tests proving BranchForge works as a
//! **general-purpose Agent SDK** — not just a coding-agent runtime.
//!
//! These tests run with `--no-default-features` to lock in the
//! pure-Layer-1 surface, then again with `--features local-fs` to
//! cover the local-machine knowledge-agent topology. They use
//! [`branchforge::client::mock::MockLlmCall`] so they require zero
//! network and zero credentials.
//!
//! The goal is to catch regressions where a future change leaks a
//! `coding-tools` (or `local-fs`) dependency into the pure core
//! Agent surface, or where a Layer 1 user can no longer construct
//! and run an agent without filesystem features.

use branchforge::client::mock::MockLlmCall;
use branchforge::ir::{
    ContentPart, FinishReason, ModelResponse, ModelStreamChunk, Role, ToolOrigin, Usage,
};
use std::sync::Arc;

/// Construct a synthetic ModelResponse that requests one tool call,
/// then a follow-up text response after the tool returns. Used to
/// drive the agent loop through a single iteration.
fn tool_call_response(tool_name: &str, args: serde_json::Value) -> ModelResponse {
    ModelResponse {
        id: "msg_tool_call".into(),
        model: "test-model".into(),
        content: vec![ContentPart::ToolCall {
            id: "call_1".into(),
            name: tool_name.into(),
            arguments: args,
            origin: ToolOrigin::Local,
        }],
        finish_reason: FinishReason::ToolCalls,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
        continuation: None,
        warnings: Vec::new(),
        raw: None,
    }
}

fn final_text_response(text: &str) -> ModelResponse {
    ModelResponse {
        id: "msg_final".into(),
        model: "test-model".into(),
        content: vec![ContentPart::Text { text: text.into() }],
        finish_reason: FinishReason::Stop,
        usage: Usage {
            input_tokens: 12,
            output_tokens: 8,
            ..Default::default()
        },
        continuation: None,
        warnings: Vec::new(),
        raw: None,
    }
}

/// MockLlmCall is callable end-to-end through the LlmCall trait
/// without any feature flag — proves that test infrastructure
/// itself is Layer 1.
#[tokio::test]
async fn mock_llm_call_implements_llmcall_at_layer_1() {
    use branchforge::client::LlmCall;
    use branchforge::ir::{Message, ModelRequest};

    let mock = MockLlmCall::new()
        .then_response(tool_call_response(
            "DemoTool",
            serde_json::json!({"q": "x"}),
        ))
        .then_response(final_text_response("done"));

    // Round-trip a unary call.
    let req = ModelRequest::new("test-model", vec![Message::user("hi")]);
    let r1 = mock.send(&req).await.unwrap();
    assert!(matches!(r1.finish_reason, FinishReason::ToolCalls));
    assert_eq!(r1.usage.input_tokens, 10);

    let r2 = mock.send(&req).await.unwrap();
    assert!(matches!(r2.finish_reason, FinishReason::Stop));
    assert_eq!(r2.text(), "done");

    assert_eq!(mock.call_count(), 2);
    assert_eq!(mock.remaining(), 0);
}

/// Streaming round-trip via MockLlmCall — exercises the
/// `ChunkStream` path used by the streaming agent loop, including
/// the cancel-token plumbing introduced in NEW-1.
#[tokio::test]
async fn mock_llm_call_streams_chunks_with_cancel_token_at_layer_1() {
    use branchforge::client::LlmCall;
    use branchforge::ir::{Message, ModelRequest};
    use futures::StreamExt;
    use tokio_util::sync::CancellationToken;

    let chunks = vec![
        Ok(ModelStreamChunk::MessageStart {
            id: "msg_1".into(),
            model: "test-model".into(),
            role: Role::Assistant,
        }),
        Ok(ModelStreamChunk::TextDelta {
            index: 0,
            text: "hello ".into(),
        }),
        Ok(ModelStreamChunk::TextDelta {
            index: 0,
            text: "world".into(),
        }),
        Ok(ModelStreamChunk::Finish {
            reason: FinishReason::Stop,
            usage: Usage::default(),
        }),
    ];
    let mock = MockLlmCall::new().then_stream(chunks);

    let req = ModelRequest::new("test-model", vec![Message::user("hi")]);
    let token = CancellationToken::new();
    let mut stream = mock.send_stream(&req, token).await.unwrap();
    let mut accumulated = String::new();
    while let Some(chunk) = stream.next().await {
        if let Ok(ModelStreamChunk::TextDelta { text, .. }) = chunk {
            accumulated.push_str(&text);
        }
    }
    assert_eq!(accumulated, "hello world");
}

/// Decorator stack composes at Layer 1: wrapping a MockLlmCall in
/// `RetryingClient` must succeed without any feature flag and
/// preserve the underlying call semantics. This catches regressions
/// where retry/circuit-breaker plumbing accidentally pulls in a
/// feature-gated dependency.
#[tokio::test]
async fn retrying_client_decorates_mock_at_layer_1() {
    use branchforge::client::{LlmCall, RetryPolicy, RetryingClient};
    use branchforge::ir::{Message, ModelRequest};

    let mock = Arc::new(MockLlmCall::new().then_response(final_text_response("ok")));
    let retrying = RetryingClient::new(mock, RetryPolicy::default());

    let req = ModelRequest::new("test-model", vec![Message::user("hi")]);
    let resp = retrying.send(&req).await.unwrap();
    assert_eq!(resp.text(), "ok");
}

/// Construct a `BudgetTracker`, a `BudgetContext` with no tenant,
/// and verify the preflight + record path runs end-to-end at Layer 1.
/// Catches regressions where budget code accidentally requires
/// `coding-tools` (e.g. by importing `tokio::process`).
#[test]
fn budget_tracker_round_trip_at_layer_1() {
    use branchforge::budget::{BudgetExceedPolicy, BudgetTracker, estimate_request_tokens};
    use branchforge::ir::{Message, ModelRequest, Usage};
    use rust_decimal_macros::dec;

    let tracker = BudgetTracker::new(dec!(5.0)).on_exceed(BudgetExceedPolicy::Stop);

    let req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("hi")]);
    let estimate = estimate_request_tokens(&req);
    assert!(estimate.input > 0);
    let cost = tracker.estimate_cost(&req.model, estimate);
    assert!(cost > rust_decimal::Decimal::ZERO);

    let usage = Usage {
        input_tokens: 1000,
        output_tokens: 500,
        ..Default::default()
    };
    let recorded = tracker.record("claude-sonnet-4-5", &usage).unwrap();
    assert!(recorded > rust_decimal::Decimal::ZERO);
    assert!(tracker.used_cost_usd() >= recorded);
}

/// IR + structured output validator are Layer 1: a strict JSON
/// schema spec must validate a matching JSON body without pulling
/// any feature flag.
#[test]
fn structured_output_validator_runs_at_layer_1() {
    use branchforge::client::schema::validate_structured_output;
    use branchforge::ir::JsonSchemaSpec;
    use serde_json::json;

    let spec = JsonSchemaSpec {
        schema: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "count": {"type": "integer"}
            },
            "required": ["name", "count"]
        }),
        name: None,
        description: None,
        strict: true,
    };
    validate_structured_output(r#"{"name":"x","count":3}"#, &spec).unwrap();
    assert!(validate_structured_output(r#"{"name":"x"}"#, &spec).is_err());
}

/// Recovery recipes (#23 A1) are pure-core: a registry with the
/// builtin recipes must be constructible and queryable at Layer 1.
#[test]
fn recovery_recipes_run_at_layer_1() {
    use branchforge::FailureCategory;
    use branchforge::agent::recovery_recipes::*;

    let registry = RecipeRegistry::new().with_boxed_recipes(builtin_general_recipes());
    assert!(registry.len() >= 4);

    let action = registry.decide(&RecoveryDecisionInput {
        category: FailureCategory::RateLimit,
        attempt: 0,
    });
    assert!(matches!(action, RecoveryAction::RetryAfter { .. }));
}

/// Subagent FSM (#27 A5) lives at Layer 1.
#[test]
fn subagent_fsm_runs_at_layer_1() {
    use branchforge::agent::SubagentState;

    let s = SubagentState::default();
    let s = s.transition_to(SubagentState::Awaiting).unwrap();
    let s = s.transition_to(SubagentState::Ready).unwrap();
    let s = s.transition_to(SubagentState::Running).unwrap();
    let s = s.transition_to(SubagentState::Finished).unwrap();
    assert!(s.is_terminal());
}

/// Permission rule DSL (#22 S6) is Layer 1.
#[test]
fn permission_rule_dsl_runs_at_layer_1() {
    use branchforge::authorization::{
        PermissionRuleSyntax, RuleDecisionKeyword, SubjectPattern, parse_permission_rule,
    };

    let parsed: PermissionRuleSyntax = parse_permission_rule("deny Bash(rm:*)").unwrap();
    assert_eq!(parsed.decision, RuleDecisionKeyword::Deny);
    assert_eq!(parsed.tool, "Bash");
    assert_eq!(
        parsed.subject,
        Some(SubjectPattern::PrefixWild("rm".into()))
    );
}
