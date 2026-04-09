//! Agent integration tests.

mod helpers;

use super::events::{AgentEvent, AgentResult};
use super::executor::Agent;
use super::state::AgentMetrics;
use super::state_formatter::format_todo_summary;
use super::{AgentConfig, AgentState};
use crate::authorization::ToolPolicy;
use crate::client::LlmCall;
use crate::common::{ContentSource, IndexRegistry};
use crate::context::{PromptOrchestrator, StaticContext};
use crate::hooks::{HookContext, HookEvent, HookInput, HookOutput, HookRegistry};
use crate::ir::{self, ContentPart, FinishReason};
use crate::session::types::TodoItem;
use crate::session::{Session, SessionAccessScope, SessionConfig, SessionId, SessionManager};
use crate::skills::{SkillIndex, SkillRuntime};
use crate::tools::{ExecutionContext, ToolOutput, ToolRegistry, ToolResult, ToolSurface};

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::RwLock;

#[test]
fn test_agent_result() {
    let metrics = AgentMetrics {
        iterations: 3,
        tool_calls: 2,
        ..Default::default()
    };

    let result = AgentResult {
        text: "Hello".to_string(),
        usage: crate::ir::Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        },
        tool_calls: 2,
        iterations: 3,
        stop_reason: FinishReason::Stop,
        state: AgentState::Completed,
        metrics,
        session_id: "test-session".to_string(),
        structured_output: None,
        messages: Vec::new(),
        uuid: "test-uuid".to_string(),
    };

    assert_eq!(result.text(), "Hello");
    assert_eq!(result.total_tokens(), 150);
    assert!(result.state.is_terminal());
    assert_eq!(result.metrics().iterations, 3);
}

#[test]
fn test_agent_result_session_id() {
    let result = AgentResult {
        text: String::new(),
        usage: crate::ir::Usage::default(),
        tool_calls: 0,
        iterations: 1,
        stop_reason: FinishReason::Stop,
        state: AgentState::Completed,
        metrics: AgentMetrics::default(),
        session_id: "my-session-123".to_string(),
        structured_output: None,
        messages: Vec::new(),
        uuid: "test-uuid".to_string(),
    };

    assert_eq!(result.session_id(), "my-session-123");
}

#[test]
fn test_agent_result_extract_success() {
    #[derive(serde::Deserialize, PartialEq, Debug)]
    struct TestOutput {
        value: i32,
    }

    let result = AgentResult {
        text: String::new(),
        usage: crate::ir::Usage::default(),
        tool_calls: 0,
        iterations: 1,
        stop_reason: FinishReason::Stop,
        state: AgentState::Completed,
        metrics: AgentMetrics::default(),
        session_id: "test".to_string(),
        structured_output: Some(serde_json::json!({"value": 42})),
        messages: Vec::new(),
        uuid: "test-uuid".to_string(),
    };

    let extracted: TestOutput = result.extract().unwrap();
    assert_eq!(extracted.value, 42);
}

#[test]
fn test_agent_result_extract_no_output() {
    let result = AgentResult {
        text: String::new(),
        usage: crate::ir::Usage::default(),
        tool_calls: 0,
        iterations: 1,
        stop_reason: FinishReason::Stop,
        state: AgentState::Completed,
        metrics: AgentMetrics::default(),
        session_id: "test".to_string(),
        structured_output: None,
        messages: Vec::new(),
        uuid: "test-uuid".to_string(),
    };

    let extracted: Result<serde_json::Value, _> = result.extract();
    assert!(extracted.is_err());
}

#[test]
fn test_agent_event_variants() {
    let text_event = AgentEvent::Text {
        delta: "Hello".to_string(),
    };
    assert!(matches!(text_event, AgentEvent::Text { .. }));

    let tool_complete = AgentEvent::ToolComplete {
        id: "tool_1".to_string(),
        name: "Read".to_string(),
        output: "file content".to_string(),
        is_error: false,
        duration_ms: 50,
    };
    assert!(matches!(
        tool_complete,
        AgentEvent::ToolComplete {
            is_error: false,
            ..
        }
    ));

    let tool_blocked = AgentEvent::ToolBlocked {
        id: "tool_2".to_string(),
        name: "Bash".to_string(),
        reason: "Authorization denied".to_string(),
    };
    assert!(matches!(tool_blocked, AgentEvent::ToolBlocked { .. }));
}

#[test]
fn test_agent_event_serialization_roundtrip() {
    let events = vec![
        AgentEvent::Text {
            delta: "Hello".to_string(),
        },
        AgentEvent::Thinking {
            content: "Let me think...".to_string(),
        },
        AgentEvent::ToolStart {
            id: "t1".to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"path": "/tmp/test.txt"}),
        },
        AgentEvent::ToolComplete {
            id: "t1".to_string(),
            name: "Read".to_string(),
            output: "file contents".to_string(),
            is_error: false,
            duration_ms: 42,
        },
        AgentEvent::ToolBlocked {
            id: "t2".to_string(),
            name: "Bash".to_string(),
            reason: "Denied".to_string(),
        },
        AgentEvent::TurnUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 10,
            cache_creation_tokens: 5,
            total_input_tokens: 100,
            total_output_tokens: 50,
        },
    ];

    for event in &events {
        let json = serde_json::to_value(event).expect("serialize");
        assert!(
            json.get("type").is_some(),
            "Missing 'type' tag in {:?}",
            event
        );

        let roundtrip: AgentEvent = serde_json::from_value(json.clone()).expect("deserialize");
        let json2 = serde_json::to_value(&roundtrip).expect("re-serialize");
        assert_eq!(json, json2, "Round-trip mismatch for {:?}", event);
    }
}

#[test]
fn test_agent_event_type_method() {
    assert_eq!(AgentEvent::Text { delta: "hi".into() }.event_type(), "text");
    assert_eq!(
        AgentEvent::Thinking { content: "".into() }.event_type(),
        "thinking"
    );
    assert_eq!(
        AgentEvent::ToolStart {
            id: "".into(),
            name: "".into(),
            input: serde_json::Value::Null
        }
        .event_type(),
        "tool_start"
    );
    assert_eq!(
        AgentEvent::ToolComplete {
            id: "".into(),
            name: "".into(),
            output: "".into(),
            is_error: false,
            duration_ms: 0
        }
        .event_type(),
        "tool_complete"
    );
    assert_eq!(
        AgentEvent::ToolBlocked {
            id: "".into(),
            name: "".into(),
            reason: "".into()
        }
        .event_type(),
        "tool_blocked"
    );
    assert_eq!(
        AgentEvent::TurnUsage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            total_input_tokens: 0,
            total_output_tokens: 0
        }
        .event_type(),
        "turn_usage"
    );
}

#[test]
fn test_agent_event_tagged_format() {
    let event = AgentEvent::Text {
        delta: "Hello".to_string(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "text");
    assert_eq!(json["delta"], "Hello");

    let event = AgentEvent::ToolStart {
        id: "t1".into(),
        name: "Read".into(),
        input: serde_json::json!({"path": "test.txt"}),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "tool_start");
    assert_eq!(json["name"], "Read");
    assert_eq!(json["input"]["path"], "test.txt");
}

#[test]
fn test_agent_metrics_serialization() {
    let metrics = AgentMetrics::default();
    let json = serde_json::to_value(&metrics).expect("serialize AgentMetrics");
    assert_eq!(json["iterations"], 0);
    assert_eq!(json["tool_calls"], 0);

    let roundtrip: AgentMetrics = serde_json::from_value(json).expect("deserialize");
    assert_eq!(roundtrip.iterations, 0);
}

#[test]
fn test_stop_reason_variants() {
    assert_eq!(FinishReason::Stop, FinishReason::Stop);
    assert_ne!(FinishReason::Stop, FinishReason::Length);
    assert_ne!(FinishReason::Stop, FinishReason::ToolCalls);
}

#[test]
fn test_content_part_from_tool_result() {
    let result = ToolResult::success("content");
    let part = ContentPart::from_tool_result("tool_123", &result);
    match part {
        ContentPart::ToolResult {
            tool_call_id,
            is_error,
            ..
        } => {
            assert_eq!(tool_call_id, "tool_123");
            assert!(!is_error);
        }
        other => panic!("Expected ToolResult, got {:?}", other),
    }
}

#[test]
fn test_session_messages_basic() {
    let mut session = Session::new(SessionConfig::default());
    assert!(session.current_branch_messages().is_empty());

    session.add_user_message("Hello").unwrap();
    assert_eq!(session.current_branch_messages().len(), 1);
}

#[test]
fn test_session_usage_update() {
    let mut session = Session::new(SessionConfig::default());
    session.add_user_message("Test").unwrap();

    session.update_usage(&ir::Usage {
        input_tokens: 100,
        output_tokens: 50,
        cached_input_tokens: Some(10),
        cache_creation_tokens: None,
        ..Default::default()
    });

    // current_input_tokens now tracks context_usage() = input + cache_read + cache_write
    assert_eq!(session.current_input_tokens, 110);
    assert_eq!(session.total_usage.input_tokens, 100);
    assert_eq!(session.total_usage.output_tokens, 50);
}

#[test]
fn test_agent_metrics_recording() {
    let mut metrics = AgentMetrics {
        iterations: 5,
        ..Default::default()
    };
    metrics.record_api_call();
    metrics.record_api_call();
    assert_eq!(metrics.api_calls, 2);

    metrics.record_tool("tu_1", "Read", 50, false);
    metrics.record_tool("tu_2", "Read", 30, false);
    metrics.record_tool("tu_3", "Bash", 100, true);

    assert_eq!(metrics.tool_calls, 3);
    assert_eq!(metrics.errors, 1);
}

#[test]
fn test_agent_state_transitions() {
    assert!(AgentState::Initializing.can_continue());
    assert!(AgentState::Running.can_continue());
    assert!(!AgentState::Completed.can_continue());
    assert!(!AgentState::Failed.can_continue());

    assert!(AgentState::WaitingForToolResults.is_waiting());
    assert!(AgentState::WaitingForUserInput.is_waiting());
    assert!(!AgentState::Running.is_waiting());

    assert!(AgentState::Completed.is_terminal());
    assert!(AgentState::Failed.is_terminal());
    assert!(!AgentState::Running.is_terminal());
}

#[test]
fn test_hook_context_builder() {
    let hook_context = HookContext::new("session-1")
        .cwd(std::path::PathBuf::from("/test/dir"))
        .env([("KEY".to_string(), "VALUE".to_string())].into());

    assert_eq!(hook_context.session_id, "session-1");
    assert_eq!(
        hook_context.cwd,
        Some(std::path::PathBuf::from("/test/dir"))
    );
    assert_eq!(hook_context.env.get("KEY"), Some(&"VALUE".to_string()));
}

#[test]
fn test_hook_output_builder() {
    let allow = HookOutput::allow();
    assert!(allow.continue_execution);

    let block = HookOutput::block("reason");
    assert!(!block.continue_execution);
    assert_eq!(block.stop_reason, Some("reason".to_string()));
}

#[test]
fn test_hook_event_can_block() {
    // Blockable events (fail-closed)
    assert!(HookEvent::PreToolUse.can_block());
    assert!(HookEvent::UserPromptSubmit.can_block());
    assert!(HookEvent::SessionStart.can_block());
    assert!(!HookEvent::PreCompact.can_block());
    assert!(HookEvent::SubagentStart.can_block());

    // Non-blockable events (fail-open)
    assert!(!HookEvent::PostToolUse.can_block());
    assert!(!HookEvent::SessionEnd.can_block());
}

#[test]
fn test_tool_result_variants() {
    let success = ToolResult::success("content");
    assert!(!success.is_error());
    assert_eq!(success.text(), "content");

    let error = ToolResult::error("failed");
    assert!(error.is_error());
    assert!(error.error_message().contains("failed"));

    let empty = ToolResult::empty();
    assert!(!empty.is_error());
    assert_eq!(empty.text(), "");
}

#[derive(Debug, Clone)]
struct MockLlmCall {
    response: ir::ModelResponse,
}

impl MockLlmCall {
    fn with_text(text: &str) -> Self {
        Self {
            response: ir::ModelResponse {
                id: "msg_test".to_string(),
                model: "claude-sonnet-4-5-20250514".to_string(),
                content: vec![ir::ContentPart::Text {
                    text: text.to_string(),
                }],
                finish_reason: ir::FinishReason::Stop,
                usage: ir::Usage {
                    input_tokens: 12,
                    output_tokens: 6,
                    ..Default::default()
                },
                continuation: None,
                warnings: Vec::new(),
                raw: None,
            },
        }
    }
}

#[async_trait]
impl LlmCall for MockLlmCall {
    async fn send(&self, _request: &ir::ModelRequest) -> crate::Result<ir::ModelResponse> {
        Ok(self.response.clone())
    }

    async fn send_stream(
        &self,
        _request: &ir::ModelRequest,
    ) -> crate::Result<crate::client::provider_client::ChunkStream> {
        Err(crate::Error::Config(
            "streaming not supported in mock".into(),
        ))
    }
}

fn mock_llm_with_message(text: &str) -> Arc<dyn LlmCall> {
    Arc::new(MockLlmCall::with_text(text))
}

#[tokio::test]
async fn test_execute_persists_live_session_when_session_manager_is_configured() {
    let llm = mock_llm_with_message("persisted reply");
    let manager = SessionManager::in_memory();
    let scope = SessionAccessScope::default()
        .tenant("tenant-a")
        .principal("user-1");
    let tools = Arc::new(ToolRegistry::default_tools(ToolSurface::All, None, None));
    let config = Arc::new(AgentConfig::default());
    let hooks = Arc::new(HookRegistry::new());
    let agent = Agent::from_parts(llm, config, tools, hooks, None)
        .session_persistence(manager.clone(), Some(scope.clone()));

    agent
        .state()
        .with_session_mut(|session| {
            session.tenant_id = Some("tenant-a".to_string());
            session.principal_id = Some("user-1".to_string());
        })
        .await;

    let result = agent.execute("hello persistence").await.unwrap();
    assert_eq!(result.text(), "persisted reply");

    let session_id = SessionId::parse(agent.session_id()).unwrap();
    let stored = manager.scoped(scope).get(&session_id).await.unwrap();
    let messages = stored.current_branch_messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content[0].as_text(), Some("hello persistence"));
    assert_eq!(messages[1].content[0].as_text(), Some("persisted reply"));
    assert_eq!(stored.total_usage.input_tokens, 12);
    assert_eq!(stored.total_usage.output_tokens, 6);
}

#[tokio::test]
async fn test_execute_routes_explicit_manual_only_skill_before_model_request() {
    let llm = mock_llm_with_message("model reply");

    let mut skill_registry = IndexRegistry::new();
    let mut skill = SkillIndex::new("math-helper", "Perform calculations")
        .source(ContentSource::in_memory("Calculate: $ARGUMENTS"));
    skill.disable_model_invocation = true;
    skill_registry.register(skill);

    let tools = Arc::new(
        ToolRegistry::builder()
            .access(ToolSurface::only(["Skill"]))
            .policy(ToolPolicy::permissive())
            .skill_executor(SkillRuntime::new(skill_registry.clone()))
            .build(),
    );

    let orchestrator = PromptOrchestrator::new(StaticContext::new(), "claude-sonnet-4-5")
        .with_skill_registry(skill_registry);

    let agent = Agent::from_parts(
        llm,
        Arc::new(AgentConfig::default()),
        tools,
        Arc::new(HookRegistry::new()),
        Some(Arc::new(RwLock::new(orchestrator))),
    );

    let result = agent.execute("/math-helper 15 * 23 + 47").await.unwrap();
    assert_eq!(result.text(), "model reply");

    let messages = agent
        .state()
        .with_session(|session| session.current_branch_messages())
        .await;
    assert_eq!(messages.len(), 4);
    assert_eq!(
        messages[0].content[0].as_text(),
        Some("/math-helper 15 * 23 + 47")
    );
    assert!(matches!(
        messages[1].content[0],
        ContentPart::ToolCall { .. }
    ));
    assert!(matches!(
        messages[2].content[0],
        ContentPart::ToolResult { .. }
    ));

    if let ContentPart::ToolResult { ref content, .. } = messages[2].content[0] {
        match content {
            crate::ir::ToolResultContent::Text(text) => {
                assert!(text.contains("Calculate: 15 * 23 + 47"));
            }
            _ => panic!("expected text tool result"),
        }
    }
}

#[tokio::test]
async fn test_execute_routes_explicit_skill_with_default_authorization_mode() {
    let llm = mock_llm_with_message("model reply");

    let mut skill_registry = IndexRegistry::new();
    let mut skill = SkillIndex::new("math-helper", "Perform calculations")
        .source(ContentSource::in_memory("Calculate: $ARGUMENTS"));
    skill.disable_model_invocation = true;
    skill_registry.register(skill);

    let tools = Arc::new(
        ToolRegistry::builder()
            .access(ToolSurface::only(["Skill"]))
            .skill_executor(SkillRuntime::new(skill_registry.clone()))
            .build(),
    );

    let orchestrator = PromptOrchestrator::new(StaticContext::new(), "claude-sonnet-4-5")
        .with_skill_registry(skill_registry);

    let agent = Agent::from_parts(
        llm,
        Arc::new(AgentConfig::default()),
        tools,
        Arc::new(HookRegistry::new()),
        Some(Arc::new(RwLock::new(orchestrator))),
    );

    let result = agent.execute("/math-helper 15 * 23 + 47").await.unwrap();
    assert_eq!(result.text(), "model reply");

    let messages = agent
        .state()
        .with_session(|session| session.current_branch_messages())
        .await;
    assert!(messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentPart::ToolCall { name, .. } if name == "Skill"))
    }));
}

#[tokio::test]
async fn test_execute_by_name_skill_respects_deny_rule() {
    let llm = mock_llm_with_message("model reply");

    let mut skill_registry = IndexRegistry::new();
    let mut skill = SkillIndex::new("internal", "Internal skill")
        .source(ContentSource::in_memory("Internal: $ARGUMENTS"));
    skill.disable_model_invocation = true;
    skill_registry.register(skill);

    let policy = ToolPolicy::builder().deny("Skill(internal)").build();

    let tools = Arc::new(
        ToolRegistry::builder()
            .access(ToolSurface::only(["Skill"]))
            .policy(policy)
            .skill_executor(SkillRuntime::new(skill_registry.clone()))
            .build(),
    );

    let orchestrator = PromptOrchestrator::new(StaticContext::new(), "claude-sonnet-4-5")
        .with_skill_registry(skill_registry);

    let agent = Agent::from_parts(
        llm,
        Arc::new(AgentConfig::default()),
        tools,
        Arc::new(HookRegistry::new()),
        Some(Arc::new(RwLock::new(orchestrator))),
    );

    let error = agent.execute("/internal inspect auth").await.unwrap_err();
    assert!(error.to_string().contains("Denied by rule"));
}

#[test]
fn test_agent_config_default_values() {
    let config = AgentConfig::default();
    assert_eq!(config.execution.max_iterations, 100);
    assert!(config.execution.auto_compact);
    assert!(config.execution.timeout.is_some());
}

#[test]
fn test_usage_accumulation() {
    let mut usage = ir::Usage::default();
    assert_eq!(usage.input_tokens + usage.output_tokens, 0);

    usage.input_tokens = 100;
    usage.output_tokens = 50;
    assert_eq!(usage.input_tokens + usage.output_tokens, 150);
}

#[test]
fn test_format_todo_summary_empty() {
    let todos: Vec<TodoItem> = vec![];
    let summary = format_todo_summary(&todos);
    assert!(summary.is_empty());
}

#[test]
fn test_format_todo_summary_with_items() {
    use crate::session::SessionId;

    let session_id = SessionId::new();
    let mut todo1 = TodoItem::new(session_id, "Fix bug", "Fixing bug");
    todo1.start();
    let todo2 = TodoItem::new(session_id, "Write tests", "Writing tests");
    let mut todo3 = TodoItem::new(session_id, "Deploy", "Deploying");
    todo3.complete();

    let todos = vec![todo1, todo2, todo3];
    let summary = format_todo_summary(&todos);

    assert!(summary.contains("1."));
    assert!(summary.contains("Fix bug"));
    assert!(summary.contains("Write tests"));
}

#[tokio::test]
async fn test_hook_manager_integration() {
    use helpers::TestTrackingHook;

    let mut hooks = HookRegistry::new();
    let hook = TestTrackingHook::new("test-hook", vec![HookEvent::PreToolUse]);
    let call_count = hook.call_count.clone();

    hooks.register(hook);

    let input = HookInput::pre_tool_use("session", "Read", serde_json::json!({}));
    let hook_context = HookContext::new("session");
    let output = hooks
        .execute(HookEvent::PreToolUse, input, &hook_context)
        .await
        .unwrap();

    assert!(output.continue_execution);
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_hook_blocking() {
    use helpers::BlockingHook;

    let mut hooks = HookRegistry::new();
    hooks.register(BlockingHook {
        reason: "blocked".to_string(),
    });

    let input = HookInput::user_prompt_submit("session", "test");
    let hook_context = HookContext::new("session");
    let output = hooks
        .execute(HookEvent::UserPromptSubmit, input, &hook_context)
        .await
        .unwrap();

    assert!(!output.continue_execution);
    assert_eq!(output.stop_reason, Some("blocked".to_string()));
}

#[tokio::test]
async fn test_hook_input_modification() {
    use helpers::InputModifyingHook;

    let mut hooks = HookRegistry::new();
    hooks.register(InputModifyingHook);

    let input = HookInput::pre_tool_use(
        "session",
        "Read",
        serde_json::json!({"file_path": "/original/path"}),
    );
    let hook_context = HookContext::new("session");
    let output = hooks
        .execute(HookEvent::PreToolUse, input, &hook_context)
        .await
        .unwrap();

    assert!(output.continue_execution);
    assert!(output.updated_input.is_some());
    let updated = output.updated_input.unwrap();
    assert_eq!(updated["file_path"], "/modified/path");
}

#[test]
fn test_tool_registry_with_dummy() {
    use helpers::DummyTool;

    let registry = ToolRegistry::new();
    let tool = Arc::new(DummyTool {
        name: "TestTool".to_string(),
        output: ToolOutput::Success("success".to_string()),
    });

    registry.register(tool);
    assert!(registry.contains("TestTool"));
    assert_eq!(registry.names().len(), 1);
}

#[tokio::test]
async fn test_tool_registry_execute() {
    use helpers::DummyTool;

    let registry = ToolRegistry::from_context(
        ExecutionContext::try_permissive().expect("failed to create permissive context"),
    );
    let tool = Arc::new(DummyTool {
        name: "TestTool".to_string(),
        output: ToolOutput::Success("test output".to_string()),
    });

    registry.register(tool);
    let result = registry.execute("TestTool", serde_json::json!({})).await;

    assert!(!result.is_error());
    assert_eq!(result.text(), "test output");
}

#[tokio::test]
async fn test_tool_registry_execute_unknown() {
    let registry = ToolRegistry::new();
    let result = registry.execute("UnknownTool", serde_json::json!({})).await;

    assert!(result.is_error());
    assert!(result.error_message().contains("unknown tool"));
}

// ── Human-in-the-loop approval tests ────────────────────────────────

struct ScriptedMockLlm {
    responses: std::sync::Mutex<std::collections::VecDeque<ir::ModelResponse>>,
}

impl ScriptedMockLlm {
    fn new(responses: Vec<ir::ModelResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
        }
    }
}

impl std::fmt::Debug for ScriptedMockLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedMockLlm").finish()
    }
}

#[async_trait]
impl LlmCall for ScriptedMockLlm {
    async fn send(&self, _request: &ir::ModelRequest) -> crate::Result<ir::ModelResponse> {
        let mut queue = self.responses.lock().unwrap();
        Ok(queue
            .pop_front()
            .expect("ScriptedMockLlm: no more responses"))
    }

    async fn send_stream(
        &self,
        _request: &ir::ModelRequest,
    ) -> crate::Result<crate::client::provider_client::ChunkStream> {
        Err(crate::Error::Config("not supported".into()))
    }
}

fn make_tool_call_response() -> ir::ModelResponse {
    ir::ModelResponse {
        id: "msg_1".into(),
        model: "test-model".into(),
        content: vec![ir::ContentPart::ToolCall {
            id: "tc_1".into(),
            name: "TestTool".into(),
            arguments: serde_json::json!({}),
            origin: Default::default(),
        }],
        finish_reason: ir::FinishReason::ToolCalls,
        usage: ir::Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
        continuation: None,
        warnings: Vec::new(),
        raw: None,
    }
}

fn make_text_response(text: &str) -> ir::ModelResponse {
    ir::ModelResponse {
        id: "msg_2".into(),
        model: "test-model".into(),
        content: vec![ir::ContentPart::Text { text: text.into() }],
        finish_reason: ir::FinishReason::Stop,
        usage: ir::Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
        continuation: None,
        warnings: Vec::new(),
        raw: None,
    }
}

fn build_supervised_agent_with_approval(
    approval_sender: crate::authorization::ApprovalSender,
) -> Agent {
    use crate::authorization::ExecutionMode;
    use helpers::DummyTool;

    let mock = ScriptedMockLlm::new(vec![
        make_tool_call_response(),
        make_text_response("All done."),
    ]);

    let tools =
        ToolRegistry::from_context(ExecutionContext::try_permissive().expect("permissive context"));
    tools.register(Arc::new(DummyTool {
        name: "TestTool".into(),
        output: ToolOutput::Success("test output".into()),
    }));
    let tools = Arc::new(tools);

    let config = Arc::new(AgentConfig::default());
    let hooks = Arc::new(HookRegistry::new());
    let mut agent = Agent::from_parts(Arc::new(mock), config, tools, hooks, None);

    agent.runtime_mut().execution_mode = ExecutionMode::Supervised;
    agent.runtime_mut().approval_sender = Some(approval_sender);
    agent
}

#[tokio::test]
async fn test_approval_approve_proceeds() {
    use crate::authorization::approval::{ApprovalResponse, approval_channel};

    let (tx, mut rx) = approval_channel(16);

    tokio::spawn(async move {
        while let Some((_request, responder)) = rx.recv().await {
            let _ = responder.send(ApprovalResponse::Approve);
        }
    });

    let agent = build_supervised_agent_with_approval(tx);
    let result = agent.execute("Run the tool").await.unwrap();

    assert_eq!(result.text(), "All done.");
    assert!(result.iterations >= 2);
}

#[tokio::test]
async fn test_approval_deny_blocks() {
    use crate::authorization::approval::{ApprovalResponse, approval_channel};

    let (tx, mut rx) = approval_channel(16);

    tokio::spawn(async move {
        while let Some((_request, responder)) = rx.recv().await {
            let _ = responder.send(ApprovalResponse::Deny {
                reason: "User said no".into(),
            });
        }
    });

    let agent = build_supervised_agent_with_approval(tx);
    let result = agent.execute("Run the tool").await.unwrap();

    assert_eq!(result.text(), "All done.");
    assert_eq!(result.metrics().authorization_denials.len(), 1);
    assert!(
        result.metrics().authorization_denials[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("User said no")
    );
}

#[tokio::test]
async fn test_approval_timeout_defaults_to_deny() {
    use crate::authorization::approval::approval_channel;

    let (tx, _rx) = approval_channel(16);

    let agent = build_supervised_agent_with_approval(tx);
    let result = agent.execute("Run the tool").await.unwrap();

    assert_eq!(result.text(), "All done.");
    assert_eq!(result.metrics().authorization_denials.len(), 1);
    assert!(
        result.metrics().authorization_denials[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("timed out")
    );
}
