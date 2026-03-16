//! Live model verification for SDK improvement roadmap.
//!
//! Verifies Phase 1-3 improvements against real Claude model via CLI OAuth:
//! - 1.1 Runtime Tool Registration (DashMap interior mutability)
//! - 1.3 Streaming Hook Events (PostStreamChunk)
//! - 2.3 Model Selection Hook
//! - 3.1 Pre/Post Message Hooks
//! - 3.2 Multi-Provider Cost Attribution
//! - 3.3 Event Bus (non-blocking observability)
//!
//! Requires Claude CLI OAuth credentials.
//! Run: cargo nextest run --test live_sdk_improvements_tests -- --ignored --nocapture

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use branchforge::hooks::{Hook, HookContext, HookEvent, HookEventData, HookInput, HookOutput};
use branchforge::tools::{ExecutionContext, Tool};
use branchforge::types::ToolResult;
use branchforge::{Agent, Auth, ToolRegistry, ToolSurface};
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::fs;

// ============================================================================
// Helpers
// ============================================================================

async fn build_agent(dir: &std::path::Path, tools: &[&str], max_iter: usize) -> Agent {
    Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::only(tools.iter().map(|s| s.to_string())))
        .working_dir(dir)
        .max_iterations(max_iter)
        .build()
        .await
        .expect("Agent build failed")
}

/// A minimal custom tool for testing runtime registration.
#[derive(Debug)]
struct PingTool;

#[async_trait::async_trait]
impl Tool for PingTool {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> &str {
        "Ping"
    }
    fn description(&self) -> &str {
        "Returns pong with a timestamp. Use this to test connectivity."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }
    async fn execute(&self, _input: Value, _ctx: &ExecutionContext) -> ToolResult {
        ToolResult::success(format!("pong-{}", chrono::Utc::now().timestamp()))
    }
}

// -- Hook implementations ---------------------------------------------------

struct StreamChunkCounter {
    count: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Hook for StreamChunkCounter {
    fn name(&self) -> &str {
        "stream_chunk_counter"
    }
    fn events(&self) -> &[HookEvent] {
        &[HookEvent::PostStreamChunk]
    }
    async fn execute(
        &self,
        _input: HookInput,
        _ctx: &HookContext,
    ) -> Result<HookOutput, branchforge::Error> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(HookOutput::allow())
    }
}

struct ModelSelectionObserver {
    seen: Arc<AtomicBool>,
    model_name: Arc<std::sync::Mutex<String>>,
}

#[async_trait::async_trait]
impl Hook for ModelSelectionObserver {
    fn name(&self) -> &str {
        "model_selection_observer"
    }
    fn events(&self) -> &[HookEvent] {
        &[HookEvent::ModelSelection]
    }
    async fn execute(
        &self,
        input: HookInput,
        _ctx: &HookContext,
    ) -> Result<HookOutput, branchforge::Error> {
        self.seen.store(true, Ordering::SeqCst);
        if let HookEventData::ModelSelection {
            requested_model, ..
        } = &input.data
        {
            *self.model_name.lock().unwrap() = requested_model.clone();
        }
        Ok(HookOutput::allow())
    }
}

struct MessageHookCounter {
    pre_count: Arc<AtomicU32>,
    post_count: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl Hook for MessageHookCounter {
    fn name(&self) -> &str {
        "message_hook_counter"
    }
    fn events(&self) -> &[HookEvent] {
        &[HookEvent::PreMessage, HookEvent::PostMessage]
    }
    async fn execute(
        &self,
        input: HookInput,
        _ctx: &HookContext,
    ) -> Result<HookOutput, branchforge::Error> {
        match &input.data {
            HookEventData::PreMessage { .. } => {
                self.pre_count.fetch_add(1, Ordering::SeqCst);
            }
            HookEventData::PostMessage { .. } => {
                self.post_count.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        }
        Ok(HookOutput::allow())
    }
}

// ============================================================================
// 1.1 Runtime Tool Registration via Interior Mutability
// ============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_runtime_tool_registration() {
    let dir = tempdir().unwrap();

    // Build agent with a custom tool
    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::only(["Ping"]))
        .tool(PingTool)
        .working_dir(dir.path())
        .max_iterations(3)
        .build()
        .await
        .expect("Agent build failed");

    let result = agent
        .execute("Use the Ping tool and tell me the pong response.")
        .await
        .expect("Live Ping failed");

    assert!(
        result.text.contains("pong"),
        "Model should use Ping tool. Got: {:?}",
        result.text
    );
    assert!(result.tool_calls > 0, "Model should have called Ping tool");

    // Verify runtime registration via DashMap (&self methods)
    let registry = ToolRegistry::new();
    let tool: Arc<dyn Tool> = Arc::new(PingTool);

    registry.register(tool.clone());
    assert!(registry.contains("Ping"));

    assert!(registry.register_dynamic(Arc::new(PingTool)).is_err());
    let removed = registry.unregister("Ping");
    assert!(removed.is_some());
    assert!(!registry.contains("Ping"));

    registry.register_dynamic(Arc::new(PingTool)).unwrap();
    assert!(registry.contains("Ping"));
}

// ============================================================================
// 1.3 + 2.3 + 3.1 Hook Events
// ============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_hook_events_fire_during_execution() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("test.txt"), "hook_test_data")
        .await
        .unwrap();

    let chunk_count = Arc::new(AtomicUsize::new(0));
    let model_seen = Arc::new(AtomicBool::new(false));
    let model_name = Arc::new(std::sync::Mutex::new(String::new()));
    let pre_count = Arc::new(AtomicU32::new(0));
    let post_count = Arc::new(AtomicU32::new(0));

    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::only(["Read"]))
        .working_dir(dir.path())
        .max_iterations(3)
        .hook(StreamChunkCounter {
            count: chunk_count.clone(),
        })
        .hook(ModelSelectionObserver {
            seen: model_seen.clone(),
            model_name: model_name.clone(),
        })
        .hook(MessageHookCounter {
            pre_count: pre_count.clone(),
            post_count: post_count.clone(),
        })
        .build()
        .await
        .expect("Agent build failed");

    let result = agent
        .execute("Read test.txt and tell me its content.")
        .await
        .expect("Live hook test failed");

    assert!(
        result.text.contains("hook_test_data"),
        "Agent should read file. Got: {:?}",
        result.text
    );

    // PostStreamChunk fires only in the streaming path (execute_stream).
    // execute() uses the batch path, so PostStreamChunk count may be 0.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let chunks = chunk_count.load(Ordering::SeqCst);
    println!(
        "[HOOK] PostStreamChunk events (batch path, expected 0): {}",
        chunks
    );

    assert!(
        model_seen.load(Ordering::SeqCst),
        "ModelSelection hook should have fired"
    );
    let observed_model = model_name.lock().unwrap().clone();
    println!("[HOOK] ModelSelection model: {}", observed_model);
    assert!(!observed_model.is_empty());

    let pre = pre_count.load(Ordering::SeqCst);
    let post = post_count.load(Ordering::SeqCst);
    println!("[HOOK] PreMessage: {}, PostMessage: {}", pre, post);
    assert!(pre > 0, "PreMessage hook should have fired");
    assert!(post > 0, "PostMessage hook should have fired");
}

// ============================================================================
// 1.3 PostStreamChunk via streaming path
// ============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_post_stream_chunk_via_streaming() {
    use futures::StreamExt;

    let dir = tempdir().unwrap();

    let chunk_count = Arc::new(AtomicUsize::new(0));

    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::none())
        .working_dir(dir.path())
        .max_iterations(1)
        .hook(StreamChunkCounter {
            count: chunk_count.clone(),
        })
        .build()
        .await
        .expect("Agent build failed");

    let stream = agent
        .execute_stream("Say hello in exactly 3 words.")
        .await
        .expect("Stream failed");

    let mut text = String::new();
    tokio::pin!(stream);
    while let Some(event) = stream.next().await {
        if let Ok(branchforge::AgentEvent::Text(t)) = event {
            text.push_str(&t);
        }
    }

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let chunks = chunk_count.load(Ordering::SeqCst);
    println!(
        "[STREAM] PostStreamChunk events: {}, text: {:?}",
        chunks, text
    );
    assert!(!text.is_empty(), "Should receive streamed text");
    assert!(chunks > 0, "PostStreamChunk should fire in streaming path");
}

// ============================================================================
// 3.2 Multi-Provider Cost Attribution
// ============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_cost_attribution() {
    use branchforge::budget::PricingTable;
    use branchforge::types::{Usage, UsageProvider};

    let dir = tempdir().unwrap();
    fs::write(dir.path().join("cost.txt"), "cost_test_42")
        .await
        .unwrap();

    let agent = build_agent(dir.path(), &["Read"], 2).await;
    let result = agent.execute("Read cost.txt").await.expect("failed");

    println!(
        "[COST] Tokens - input: {}, output: {}",
        result.usage.input_tokens, result.usage.output_tokens
    );
    assert!(result.usage.input_tokens > 0);
    assert!(result.usage.output_tokens > 0);

    // Verify multi-provider pricing
    let table = PricingTable::default();
    let usage = Usage {
        input_tokens: 1000,
        output_tokens: 500,
        ..Default::default()
    };

    let anthropic = table.calculate_for_provider("sonnet", &usage, &UsageProvider::Anthropic);
    let openai = table.calculate_for_provider("gpt-4o", &usage, &UsageProvider::OpenAi);
    let gemini = table.calculate_for_provider("gemini-2.0-flash", &usage, &UsageProvider::Gemini);

    println!(
        "[COST] Per 1K+500 - Anthropic: {}, OpenAI: {}, Gemini: {}",
        anthropic, openai, gemini
    );
    assert!(anthropic > rust_decimal::Decimal::ZERO);
    assert!(openai > rust_decimal::Decimal::ZERO);
    assert!(gemini > rust_decimal::Decimal::ZERO);

    // Date-suffix normalization
    let dated = table.calculate_for_provider("gpt-4o-2024-08-06", &usage, &UsageProvider::OpenAi);
    assert_eq!(dated, openai, "Date suffix should normalize to same price");
}

// ============================================================================
// 3.3 Event Bus
// ============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_event_bus() {
    use branchforge::events::{Event, EventBus, EventKind};

    let bus = Arc::new(EventBus::default());
    let mut rx = bus.subscribe_all();

    let tool_count = Arc::new(AtomicUsize::new(0));
    let tc = tool_count.clone();
    bus.subscribe(
        EventKind::ToolExecuted,
        Arc::new(move |_: Event| {
            tc.fetch_add(1, Ordering::SeqCst);
        }),
    );

    bus.emit_simple(EventKind::RequestSent, json!({"model": "claude-sonnet"}));
    bus.emit_simple(EventKind::ToolExecuted, json!({"tool": "Read"}));
    bus.emit(Event::new(EventKind::TokensConsumed, json!({"input": 500})).with_session("s1"));
    bus.emit_simple(EventKind::Custom("my_metric"), json!({"latency": 42}));

    let mut kinds = Vec::new();
    for _ in 0..4 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
            Ok(Ok(e)) => kinds.push(e.kind),
            _ => break,
        }
    }
    assert_eq!(kinds.len(), 4, "Should receive all 4 events");
    assert_eq!(kinds[3], EventKind::Custom("my_metric"));

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(tool_count.load(Ordering::SeqCst), 1);
}

// ============================================================================
// Integration: full agent cycle with all hooks
// ============================================================================

#[tokio::test]
#[ignore = "Requires CLI credentials"]
async fn live_full_agent_cycle_all_hooks() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("data.txt"), "payload_99")
        .await
        .unwrap();

    let chunks = Arc::new(AtomicUsize::new(0));
    let model_ok = Arc::new(AtomicBool::new(false));
    let pre = Arc::new(AtomicU32::new(0));
    let post = Arc::new(AtomicU32::new(0));

    let agent = Agent::builder()
        .auth(Auth::ClaudeCli)
        .await
        .expect("CLI credentials required")
        .tools(ToolSurface::only(["Read", "Glob"]))
        .working_dir(dir.path())
        .max_iterations(5)
        .hook(StreamChunkCounter {
            count: chunks.clone(),
        })
        .hook(ModelSelectionObserver {
            seen: model_ok.clone(),
            model_name: Arc::new(std::sync::Mutex::new(String::new())),
        })
        .hook(MessageHookCounter {
            pre_count: pre.clone(),
            post_count: post.clone(),
        })
        .build()
        .await
        .expect("Agent build failed");

    let result = agent
        .execute("Find .txt files, read data.txt, return only the number from it.")
        .await
        .expect("Integration test failed");

    assert!(
        result.text.contains("99"),
        "Should extract 99. Got: {:?}",
        result.text
    );
    assert!(result.tool_calls >= 2, "Should call Glob + Read");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    println!(
        "[INTEGRATION] chunks={}, model={}, pre={}, post={}, tools={}, usage={}+{}",
        chunks.load(Ordering::SeqCst),
        model_ok.load(Ordering::SeqCst),
        pre.load(Ordering::SeqCst),
        post.load(Ordering::SeqCst),
        result.tool_calls,
        result.usage.input_tokens,
        result.usage.output_tokens,
    );

    // PostStreamChunk only fires in streaming path; batch path won't emit it
    assert!(model_ok.load(Ordering::SeqCst));
    assert!(pre.load(Ordering::SeqCst) > 0);
    assert!(post.load(Ordering::SeqCst) > 0);
}
