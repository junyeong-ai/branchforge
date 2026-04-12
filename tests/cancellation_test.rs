//! Integration test for cancellation propagation.
//!
//! Verifies that `runtime.shutdown.child_token()` (wired into
//! `ToolRegistry::execute_with_cancel` and the streaming spawn path)
//! actually aborts in-flight tools when shutdown is signalled.
//!
//! These are end-to-end tests at the ToolRegistry layer because
//! constructing a full Agent in a test is heavyweight; the layer below
//! the agent is where the cancellation race actually lives, and it's
//! the same code path execution.rs and streaming.rs invoke.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use branchforge::tools::{ExecutionContext, Tool, ToolRegistry};
use branchforge::{ToolOutput, ToolResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// A tool that sleeps for `duration_ms` ms and only then returns success.
/// Honours `context.cancel_token()` cooperatively via `tokio::select!`.
struct SlowTool {
    duration_ms: u64,
}

#[async_trait]
impl Tool for SlowTool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn name(&self) -> &str {
        "slow"
    }
    fn description(&self) -> &str {
        "Sleeps for a configured number of ms"
    }
    fn input_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn execute(&self, _input: Value, context: &ExecutionContext) -> ToolResult {
        let ms = self.duration_ms;
        if let Some(token) = context.cancel_token() {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(ms)) => {
                    ToolResult { output: ToolOutput::Success(format!("slept {ms}ms")), inner_usage: None, inner_model: None, overflow: None }
                }
                _ = token.cancelled() => {
                    ToolResult { output: ToolOutput::Success("cancelled".to_string()), inner_usage: None, inner_model: None, overflow: None }
                }
            }
        } else {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            ToolResult {
                output: ToolOutput::Success(format!("slept {ms}ms")),
                inner_usage: None,
                inner_model: None,
                overflow: None,
            }
        }
    }
}

fn registry_with_slow_tool(duration_ms: u64) -> ToolRegistry {
    // Build a minimal registry. The Layer 1 empty() constructor gives us
    // everything we need — this test only cares about cancellation
    // propagation, not about filesystem security or tool policy.
    let ctx = ExecutionContext::empty();
    let registry = ToolRegistry::from_context(ctx);
    registry.register(Arc::new(SlowTool { duration_ms }));
    registry
}

#[tokio::test]
async fn cooperative_cancellation_aborts_long_running_tool() {
    // Set up a registry with a 5s sleeping tool. Issue cancel after 50ms
    // and assert the call returns within ~200ms.
    let registry = registry_with_slow_tool(5000);
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    let handle = tokio::spawn(async move {
        registry
            .execute_with_cancel("slow", serde_json::json!({}), cancel_clone)
            .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();

    let result = tokio::time::timeout(Duration::from_millis(500), handle).await;
    let result = result.expect("call returned within 500ms");
    let tool_result = result.expect("task did not panic");
    // The registry's tokio::select! race returns ToolResult::error
    // ("Tool execution cancelled") when the token fires before the tool
    // completes. The fact that we got back within 500ms (not 5s) is
    // proof enough that cancellation propagated.
    let text = match tool_result.output {
        ToolOutput::Error(e) => e.to_string(),
        ToolOutput::Success(s) => s,
        other => panic!("unexpected output: {other:?}"),
    };
    assert!(
        text.to_lowercase().contains("cancel"),
        "expected cancellation marker, got: {text}"
    );
}

#[tokio::test]
async fn child_token_cancels_when_parent_runtime_shuts_down() {
    // Mirrors the agent runtime pattern: a parent token is created, then
    // each tool gets a child via `parent.child_token()`. Cancelling the
    // parent must cancel the child.
    let parent = CancellationToken::new();
    let registry = registry_with_slow_tool(5000);
    let child = parent.child_token();

    let handle = tokio::spawn(async move {
        registry
            .execute_with_cancel("slow", serde_json::json!({}), child)
            .await
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    parent.cancel(); // simulate runtime.shutdown()

    let result = tokio::time::timeout(Duration::from_millis(500), handle).await;
    assert!(
        result.is_ok(),
        "child token did not propagate parent cancel"
    );
}

#[tokio::test]
async fn no_cancel_token_lets_tool_run_to_completion_within_timeout() {
    // Sanity check: when cancellation is not used, the tool runs normally
    // (subject to the registry's own per-tool timeout).
    let registry = registry_with_slow_tool(50);

    let result = registry.execute("slow", serde_json::json!({})).await;
    let text = match result.output {
        ToolOutput::Success(s) => s,
        _ => panic!("expected success"),
    };
    assert!(text.contains("slept 50ms"));
}
