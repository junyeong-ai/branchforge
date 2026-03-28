//! Live model verification for RunConfig runtime overrides.
//!
//! Verifies that RunConfig properly overrides model, max_iterations,
//! system_prompt, and timeout at runtime using real Claude model calls
//! via CLI OAuth credentials.
//!
//! Run: cargo test --test live_run_config_tests --all-features -- --ignored --nocapture

#![cfg(feature = "cli-auth")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use branchforge::events::{EventBus, EventKind};
use branchforge::tools::{ExecutionContext, Tool};
use branchforge::types::ToolResult;
use branchforge::{Agent, AgentEvent, Auth, RunConfig, ToolSurface};
use futures::StreamExt;
use serde_json::{Value, json};
use std::any::Any;
use std::pin::pin;
use tempfile::tempdir;

async fn build_agent(dir: &std::path::Path) -> Agent {
    Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::none())
        .working_dir(dir)
        .max_iterations(1)
        .build()
        .await
        .expect("Agent build failed")
}

// =============================================================================
// Test 1: RunConfig model override
// =============================================================================

#[tokio::test]
#[ignore]
async fn live_run_config_model_override() {
    let dir = tempdir().unwrap();
    let agent = build_agent(dir.path()).await;

    // Default model
    let default_result = agent
        .execute("Reply with exactly: HELLO")
        .await
        .expect("Default execute failed");
    println!("[DEFAULT] text={}", default_result.text().trim());

    // Override to haiku
    let config = RunConfig::new().model("claude-haiku-4-5-20251001");
    let override_result = agent
        .execute_with("Reply with exactly: WORLD", config)
        .await
        .expect("Override execute failed");
    println!("[OVERRIDE] text={}", override_result.text().trim());

    assert!(!default_result.text().is_empty());
    assert!(!override_result.text().is_empty());
}

// =============================================================================
// Test 2: RunConfig max_iterations override
// =============================================================================

#[tokio::test]
#[ignore]
async fn live_run_config_max_iterations() {
    let dir = tempdir().unwrap();
    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::none())
        .working_dir(dir.path())
        .max_iterations(10)
        .build()
        .await
        .expect("Agent build failed");

    let config = RunConfig::new().max_iterations(1);
    let result = agent
        .execute_with("Say hello", config)
        .await
        .expect("Execute failed");

    println!(
        "[MAX_ITER=1] iterations={}, text={}",
        result.metrics.iterations,
        result.text().trim()
    );
    assert_eq!(result.metrics.iterations, 1);
}

// =============================================================================
// Test 3: RunConfig system_prompt override
// =============================================================================

#[tokio::test]
#[ignore]
async fn live_run_config_system_prompt_override() {
    let dir = tempdir().unwrap();
    let agent = build_agent(dir.path()).await;

    let config = RunConfig::new().system_prompt(
        "You are a pirate. You MUST start every response with 'Arrr!' no matter what.",
    );
    let result = agent
        .execute_with("What is 2+2?", config)
        .await
        .expect("Execute failed");

    let text = result.text();
    println!("[SYSTEM_PROMPT] text={}", text.trim());
    assert!(
        text.contains("Arrr") || text.contains("arrr") || text.contains("pirate"),
        "System prompt override not applied. Got: {}",
        text.trim()
    );
}

// =============================================================================
// Test 4: RunConfig with streaming path
// =============================================================================

#[tokio::test]
#[ignore]
async fn live_run_config_streaming() {
    let dir = tempdir().unwrap();
    let agent = build_agent(dir.path()).await;

    let config = RunConfig::new().model("claude-haiku-4-5-20251001");
    let stream = agent
        .execute_stream_with("Reply with one word: PING", config)
        .await
        .expect("Stream start failed");

    let mut stream = pin!(stream);
    let mut text = String::new();
    let mut completed = false;

    while let Some(event) = stream.next().await {
        match event.expect("Stream error") {
            AgentEvent::Text { delta } => text.push_str(&delta),
            AgentEvent::Complete(_) => {
                completed = true;
            }
            _ => {}
        }
    }

    println!("[STREAM] text={}", text.trim());
    assert!(completed, "Stream should complete");
    assert!(!text.is_empty(), "Should produce text");
}

// =============================================================================
// Test 5: Runtime tool registration + RunConfig combined
// =============================================================================

#[derive(Debug)]
struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> &str {
        "Echo"
    }
    fn description(&self) -> &str {
        "Echoes input back. Use this tool to echo text."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Text to echo" }
            },
            "required": ["text"]
        })
    }
    async fn execute(&self, input: Value, _ctx: &ExecutionContext) -> ToolResult {
        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("(empty)");
        ToolResult::success(format!("ECHO: {}", text))
    }
}

#[tokio::test]
#[ignore]
async fn live_runtime_tool_plus_run_config() {
    let dir = tempdir().unwrap();
    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::only(["Echo".to_string()]))
        .tool(EchoTool)
        .working_dir(dir.path())
        .max_iterations(3)
        .build()
        .await
        .expect("Agent build failed");

    let config = RunConfig::new()
        .model("claude-haiku-4-5-20251001")
        .max_iterations(3);

    let result = agent
        .execute_with(
            "Use the Echo tool to echo 'runtime works'. Report the result.",
            config,
        )
        .await
        .expect("Execute failed");

    println!(
        "[COMBINED] tools={}, text={}",
        result.metrics.tool_calls,
        result.text().trim()
    );
    assert!(
        result.metrics.tool_calls >= 1,
        "Should have called Echo tool"
    );
}

// =============================================================================
// Test 6: EventBus SubscriptionHandle auto-unsubscribe
// =============================================================================

#[tokio::test]
#[ignore]
async fn live_eventbus_subscription_handle() {
    let dir = tempdir().unwrap();
    let event_bus = Arc::new(EventBus::default());
    let token_count = Arc::new(AtomicUsize::new(0));

    let tc = token_count.clone();
    let handle = event_bus.subscribe_with_handle(
        EventKind::TokensConsumed,
        Arc::new(move |event| {
            if let Some(total) = event.data.get("total_tokens").and_then(|v| v.as_u64()) {
                tc.fetch_add(total as usize, Ordering::SeqCst);
            }
        }),
    );

    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::none())
        .working_dir(dir.path())
        .max_iterations(1)
        .build()
        .await
        .expect("Agent build failed")
        .with_event_bus(event_bus.clone());

    agent.execute("Say one word").await.expect("Execute failed");

    tokio::time::sleep(Duration::from_millis(100)).await;
    println!(
        "[EVENTBUS] tokens_tracked={}",
        token_count.load(Ordering::SeqCst)
    );

    drop(handle);
    assert_eq!(
        event_bus.subscriber_count(EventKind::TokensConsumed),
        0,
        "SubscriptionHandle must auto-unsubscribe on drop"
    );
}

// =============================================================================
// Test 7: Graceful shutdown
// =============================================================================

#[tokio::test]
#[ignore]
async fn live_graceful_shutdown() {
    let dir = tempdir().unwrap();
    let agent = build_agent(dir.path()).await;

    // Cancel before execute
    agent.shutdown_token().cancel();

    let result = agent.execute("Write a long essay").await;
    println!("[SHUTDOWN] completed={}", result.is_ok());
    // Key: test does not hang
}
