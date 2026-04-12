//! Live verification of D1-D9 design review fixes.
//!
//! Each test verifies a specific fix against a real Claude model via
//! CLI OAuth. Uses Haiku for speed and cost efficiency.
//!
//! Run: cargo test --test live_design_review_fixes --features "cli-auth,coding-tools" -- --ignored --nocapture

#![cfg(feature = "cli-auth")]

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use branchforge::events::{EventBus, EventKind, SessionChangedPayload};
use branchforge::session::SessionManager;
use branchforge::tools::{ExecutionContext, Tool};
use branchforge::types::ToolResult;
use branchforge::{Agent, AgentEvent, Auth, RunConfig, ToolSurface};
use futures::StreamExt;
use serde_json::{Value, json};
use std::pin::pin;
use tempfile::tempdir;

const HAIKU: &str = "claude-haiku-4-5-20251001";

async fn haiku_agent(dir: &std::path::Path, tools: &[&str], max_iter: usize) -> Agent {
    Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required — run: claude login")
        .model(HAIKU)
        .tools(ToolSurface::only(tools.iter().map(|s| s.to_string())))
        .working_dir(dir)
        .max_iterations(max_iter)
        .build()
        .await
        .expect("Agent build failed")
}

// =============================================================================
// T1: D1 — Timeout fires correctly (ExecutionConfig.timeout is Duration)
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_d1_timeout_fires() {
    let dir = tempdir().unwrap();
    let agent = haiku_agent(dir.path(), &[], 5).await;

    let start = Instant::now();
    let result = agent
        .execute_with(
            "Write a 5000-word essay about the history of computing. Be very detailed and thorough.",
            RunConfig::new().timeout(Duration::from_secs(2)),
        )
        .await;

    let elapsed = start.elapsed();
    println!("[D1] elapsed={:?}, result={:?}", elapsed, result.is_err());

    assert!(result.is_err(), "Short timeout should cause an error");
    assert!(
        elapsed < Duration::from_secs(10),
        "Should not hang past timeout"
    );
}

// =============================================================================
// T2: D2 — Structured output with output_schema
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_d2_structured_output() {
    let dir = tempdir().unwrap();
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer" },
            "city": { "type": "string" }
        },
        "required": ["name", "age", "city"]
    });

    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .model(HAIKU)
        .tools(ToolSurface::none())
        .working_dir(dir.path())
        .max_iterations(1)
        .output_schema(schema)
        .build()
        .await
        .expect("Agent build failed");

    let result = agent
        .execute("Generate a fictional person with name, age, and city. Respond with ONLY valid JSON, no markdown.")
        .await
        .expect("Execute failed");

    println!("[D2] text={}", result.text().trim());
    println!("[D2] structured_output={:?}", result.structured_output);

    // Try to parse JSON from the response text (strip markdown fences if present)
    let text = result.text().trim();
    let json_text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .unwrap_or(text)
        .strip_suffix("```")
        .unwrap_or(text)
        .trim();

    let parsed: Value = serde_json::from_str(json_text)
        .or_else(|_| {
            // Fall back to structured_output field
            result
                .structured_output
                .clone()
                .ok_or_else(|| serde_json::from_str::<Value>("null").unwrap_err())
        })
        .expect("Response should contain valid JSON (either in text or structured_output)");

    assert!(parsed["name"].is_string(), "Should have name field");
    assert!(
        parsed["age"].is_number() || parsed["age"].is_i64(),
        "Should have age field"
    );
    assert!(parsed["city"].is_string(), "Should have city field");
}

// =============================================================================
// T3: D3 — Execution log records tool calls
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_d3_execution_log() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("test.txt"), "hello world")
        .await
        .unwrap();

    let agent = haiku_agent(dir.path(), &["Read"], 3).await;
    let result = agent
        .execute("Read test.txt and tell me what it says.")
        .await
        .expect("Execute failed");

    println!("[D3] tool_calls={}", result.tool_calls);

    let log_count = agent.state().with_tool_executions(|e| e.len()).await;
    println!("[D3] execution_log_count={}", log_count);

    assert!(
        log_count > 0,
        "Tool execution log should have recorded the Read call"
    );
}

// =============================================================================
// T4: D4 — ToolOutput::Empty produces "(no output)" placeholder
// =============================================================================

#[derive(Debug)]
struct EmptyOutputTool;

#[async_trait::async_trait]
impl Tool for EmptyOutputTool {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> &str {
        "EmptyTool"
    }
    fn description(&self) -> &str {
        "A tool that returns no output. Use this when asked to run the empty tool."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }
    async fn execute(&self, _input: Value, _ctx: &ExecutionContext) -> ToolResult {
        ToolResult::empty()
    }
}

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_d4_empty_tool_output() {
    let dir = tempdir().unwrap();
    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .model(HAIKU)
        .tools(ToolSurface::only(["EmptyTool".to_string()]))
        .tool(EmptyOutputTool)
        .working_dir(dir.path())
        .max_iterations(3)
        .build()
        .await
        .expect("Agent build failed");

    let result = agent
        .execute("Run the EmptyTool and then tell me what happened.")
        .await
        .expect("Execute failed");

    println!(
        "[D4] tool_calls={}, text={}",
        result.tool_calls,
        result.text().trim()
    );
    assert!(result.tool_calls >= 1, "EmptyTool should have been called");
    // Model should handle "(no output)" gracefully and continue
    assert!(
        !result.text().is_empty(),
        "Model should respond after empty tool output"
    );
}

// =============================================================================
// T5: D5 — SessionChanged typed event fires
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_d5_session_changed_event() {
    let dir = tempdir().unwrap();
    let event_bus = Arc::new(EventBus::default());

    let changes = Arc::new(Mutex::new(Vec::new()));
    let changes_clone = changes.clone();
    event_bus.subscribe_typed(move |payload: SessionChangedPayload| {
        changes_clone.lock().unwrap().push(payload);
    });

    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .model(HAIKU)
        .tools(ToolSurface::none())
        .working_dir(dir.path())
        .max_iterations(1)
        .build()
        .await
        .expect("Agent build failed")
        .with_event_bus(event_bus.clone());

    agent.execute("Say hello").await.expect("Execute failed");

    // Give event bus drainers a moment to process
    tokio::time::sleep(Duration::from_millis(200)).await;

    let events = changes.lock().unwrap();
    println!("[D5] session_changed_events={}", events.len());
    assert!(
        events.len() >= 2,
        "Should have at least 2 SessionChanged events (user msg + assistant msg). Got: {}",
        events.len()
    );
    assert!(
        events.last().unwrap().message_count >= 2,
        "Last event should show at least 2 messages"
    );
}

// =============================================================================
// T6: D9 — execute_inner orchestrator: metrics correctly recorded
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_d9_orchestrator_metrics() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("info.txt"), "The answer is 42")
        .await
        .unwrap();

    let agent = haiku_agent(dir.path(), &["Read"], 5).await;
    let result = agent
        .execute("Read info.txt and tell me what the answer is.")
        .await
        .expect("Execute failed");

    println!(
        "[D9] iterations={}, api_calls={}, tool_calls={}, execution_time_ms={}",
        result.metrics.iterations,
        result.metrics.api_calls,
        result.metrics.tool_calls,
        result.metrics.execution_time_ms
    );

    assert!(
        result.metrics.iterations > 0,
        "Should have at least 1 iteration"
    );
    assert!(result.metrics.api_calls > 0, "Should have made API calls");
    assert!(result.metrics.tool_calls > 0, "Should have used Read tool");
    assert!(
        result.metrics.execution_time_ms > 0,
        "Execution time should be recorded"
    );
    assert!(result.text().contains("42"), "Should extract the answer");
}

// =============================================================================
// T7: D7 — Fork session with lock + sidechain consistency
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_d7_fork_session_consistency() {
    let dir = tempdir().unwrap();
    let agent = haiku_agent(dir.path(), &[], 1).await;

    // First turn
    let result1 = agent
        .execute("Remember the secret code: ALPHA-7. Reply with OK.")
        .await
        .expect("First execute failed");
    println!("[D7] turn1: {}", result1.text().trim());

    // Fork via SessionManager
    let manager = SessionManager::in_memory();
    let session = agent.state().session().await;
    manager.update(&session).await.expect("Save failed");

    let forked = manager.fork(&session.id).await.expect("Fork failed");

    let forked_msgs = forked.current_branch_messages();
    let original_msgs = session.current_branch_messages();

    println!(
        "[D7] original_msgs={}, forked_msgs={}",
        original_msgs.len(),
        forked_msgs.len()
    );

    assert_eq!(
        original_msgs.len(),
        forked_msgs.len(),
        "Fork should have same number of messages"
    );
    assert_ne!(
        forked.id, session.id,
        "Fork should have different session id"
    );
    assert_eq!(
        forked.parent_id,
        Some(session.id),
        "Fork should reference parent"
    );
    assert!(
        forked_msgs.iter().all(|m| m.is_sidechain),
        "All forked messages should be marked as sidechain"
    );
}

// =============================================================================
// T8: Combined pipeline — EventBus + tools + RunConfig + streaming
// =============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_combined_pipeline() {
    let dir = tempdir().unwrap();
    tokio::fs::write(dir.path().join("data.csv"), "name,score\nAlice,95\nBob,87")
        .await
        .unwrap();

    let event_bus = Arc::new(EventBus::default());
    let tool_events = Arc::new(AtomicUsize::new(0));
    let te = tool_events.clone();
    event_bus.subscribe(
        EventKind::ToolExecuted,
        Arc::new(move |_| {
            te.fetch_add(1, Ordering::SeqCst);
        }),
    );

    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .model(HAIKU)
        .tools(ToolSurface::only(["Read".to_string()]))
        .working_dir(dir.path())
        .max_iterations(5)
        .build()
        .await
        .expect("Agent build failed")
        .with_event_bus(event_bus.clone());

    // Streaming execution with RunConfig
    let config = RunConfig::new().max_iterations(5);
    let stream = agent
        .execute_stream_with(
            "Read data.csv and tell me who has the highest score.",
            config,
        )
        .await
        .expect("Stream start failed");

    let mut stream = pin!(stream);
    let mut text_chunks = 0;
    let mut tool_starts = 0;
    let mut completed = false;

    while let Some(event) = stream.next().await {
        match event.expect("Stream error") {
            AgentEvent::Text { .. } => text_chunks += 1,
            AgentEvent::ToolStart { name, .. } => {
                let _ = name;
                tool_starts += 1;
            }
            AgentEvent::Complete(result) => {
                println!(
                    "[COMBINED] text={}, tool_calls={}",
                    result.text().trim(),
                    result.tool_calls
                );
                completed = true;
            }
            _ => {}
        }
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    println!(
        "[COMBINED] text_chunks={}, tool_starts={}, event_tool_executed={}",
        text_chunks,
        tool_starts,
        tool_events.load(Ordering::SeqCst)
    );

    assert!(completed, "Stream should complete");
    assert!(text_chunks > 0, "Should have text chunks");
    assert!(tool_starts > 0, "Should have tool starts (Read)");
    assert!(
        tool_events.load(Ordering::SeqCst) > 0,
        "EventBus should have received ToolExecuted"
    );
}
