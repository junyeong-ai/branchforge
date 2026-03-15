//! Agent integration tests.

mod helpers;

use super::events::{AgentEvent, AgentResult};
use super::executor::Agent;
use super::state::AgentMetrics;
use super::state_formatter::format_todo_summary;
use super::{AgentConfig, AgentState};
use crate::Client;
use crate::authorization::AuthorizationPolicy;
use crate::client::{DEFAULT_SMALL_MODEL, GatewayConfig, ModelConfig, ProviderConfig};
use crate::common::{ContentSource, IndexRegistry};
use crate::context::{PromptOrchestrator, StaticContext};
use crate::hooks::{HookContext, HookEvent, HookInput, HookManager, HookOutput};
use crate::session::types::TodoItem;
use crate::session::{Session, SessionAccessScope, SessionConfig, SessionId, SessionManager};
use crate::skills::{SkillIndex, SkillRuntime};
use crate::tools::{ExecutionContext, ToolOutput, ToolRegistry, ToolResult, ToolSurface};
use crate::types::{ContentBlock, StopReason, ToolResultBlock, ToolResultContent, Usage};

use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::RwLock;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn test_agent_result() {
    let metrics = AgentMetrics {
        iterations: 3,
        tool_calls: 2,
        ..Default::default()
    };

    let result = AgentResult {
        text: "Hello".to_string(),
        usage: Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        },
        tool_calls: 2,
        iterations: 3,
        stop_reason: StopReason::EndTurn,
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
        usage: Usage::default(),
        tool_calls: 0,
        iterations: 1,
        stop_reason: StopReason::EndTurn,
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
        usage: Usage::default(),
        tool_calls: 0,
        iterations: 1,
        stop_reason: StopReason::EndTurn,
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
        usage: Usage::default(),
        tool_calls: 0,
        iterations: 1,
        stop_reason: StopReason::EndTurn,
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
    let text_event = AgentEvent::Text("Hello".to_string());
    assert!(matches!(text_event, AgentEvent::Text(_)));

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
fn test_stop_reason_variants() {
    assert_eq!(StopReason::EndTurn, StopReason::EndTurn);
    assert_ne!(StopReason::EndTurn, StopReason::MaxTokens);
    assert_ne!(StopReason::EndTurn, StopReason::ToolUse);
}

#[test]
fn test_tool_result_block_from_tool_result() {
    let result = ToolResult::success("content");
    let block = ToolResultBlock::from_tool_result("tool_123", &result);
    assert_eq!(block.tool_use_id, "tool_123");
    assert!(!block.is_error.unwrap_or(false));
}

#[test]
fn test_session_messages_basic() {
    let mut session = Session::new(SessionConfig::default());
    assert!(session.current_branch_messages().is_empty());

    session.add_user_message("Hello");
    assert_eq!(session.current_branch_messages().len(), 1);
}

#[test]
fn test_session_usage_update() {
    let mut session = Session::new(SessionConfig::default());
    session.add_user_message("Test");

    session.update_usage(&Usage {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_input_tokens: Some(10),
        cache_creation_input_tokens: None,
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

async fn mock_client_with_message(text: &str) -> (MockServer, Client) {
    let server = MockServer::start().await;
    let response = serde_json::json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "content": [
            {"type": "text", "text": text}
        ],
        "model": "claude-sonnet-4-5-20250514",
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 12,
            "output_tokens": 6
        }
    });
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(&server)
        .await;

    let config = ProviderConfig::new(ModelConfig::new(
        "claude-sonnet-4-5-20250514",
        DEFAULT_SMALL_MODEL,
    ))
    .max_tokens(1024);
    let client = Client::builder()
        .auth("test-key")
        .await
        .expect("auth should initialize")
        .config(config)
        .gateway(GatewayConfig::base_url(server.uri()))
        .build()
        .await
        .expect("client should build");
    (server, client)
}

#[tokio::test]
async fn test_execute_persists_live_session_when_session_manager_is_configured() {
    let (_server, client) = mock_client_with_message("persisted reply").await;
    let manager = SessionManager::in_memory();
    let scope = SessionAccessScope::default()
        .tenant("tenant-a")
        .principal("user-1");
    let tools = Arc::new(ToolRegistry::default_tools(ToolSurface::All, None, None));
    let config = Arc::new(AgentConfig::default());
    let hooks = Arc::new(HookManager::new());
    let agent = Agent::from_parts(Arc::new(client), config, tools, hooks, None)
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

    let session_id = SessionId::parse(agent.get_session_id()).unwrap();
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
    let (_server, client) = mock_client_with_message("model reply").await;

    let mut skill_registry = IndexRegistry::new();
    let mut skill = SkillIndex::new("math-helper", "Perform calculations")
        .source(ContentSource::in_memory("Calculate: $ARGUMENTS"));
    skill.disable_model_invocation = true;
    skill_registry.register(skill);

    let tools = Arc::new(
        ToolRegistry::builder()
            .access(ToolSurface::only(["Skill"]))
            .policy(AuthorizationPolicy::permissive())
            .skill_executor(SkillRuntime::new(skill_registry.clone()))
            .build(),
    );

    let orchestrator = PromptOrchestrator::new(StaticContext::new(), "claude-sonnet-4-5")
        .skill_registry(skill_registry);

    let agent = Agent::from_parts(
        Arc::new(client),
        Arc::new(AgentConfig::default()),
        tools,
        Arc::new(HookManager::new()),
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
    assert!(matches!(messages[1].content[0], ContentBlock::ToolUse(_)));
    assert!(matches!(
        messages[2].content[0],
        ContentBlock::ToolResult(_)
    ));

    if let ContentBlock::ToolResult(result_block) = &messages[2].content[0] {
        match &result_block.content {
            Some(ToolResultContent::Text(text)) => {
                assert!(text.contains("Calculate: 15 * 23 + 47"));
            }
            _ => panic!("expected text tool result"),
        }
    }
}

#[tokio::test]
async fn test_execute_routes_explicit_skill_with_default_authorization_mode() {
    let (_server, client) = mock_client_with_message("model reply").await;

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
        .skill_registry(skill_registry);

    let agent = Agent::from_parts(
        Arc::new(client),
        Arc::new(AgentConfig::default()),
        tools,
        Arc::new(HookManager::new()),
        Some(Arc::new(RwLock::new(orchestrator))),
    );

    let result = agent.execute("/math-helper 15 * 23 + 47").await.unwrap();
    assert_eq!(result.text(), "model reply");

    let messages = agent
        .state()
        .with_session(|session| session.current_branch_messages())
        .await;
    assert!(messages.iter().any(|message| {
        message.content.iter().any(
            |block| matches!(block, ContentBlock::ToolUse(tool_use) if tool_use.name == "Skill"),
        )
    }));
}

#[tokio::test]
async fn test_execute_explicit_skill_respects_deny_rule() {
    let (_server, client) = mock_client_with_message("model reply").await;

    let mut skill_registry = IndexRegistry::new();
    let mut skill = SkillIndex::new("internal", "Internal skill")
        .source(ContentSource::in_memory("Internal: $ARGUMENTS"));
    skill.disable_model_invocation = true;
    skill_registry.register(skill);

    let policy = AuthorizationPolicy::builder()
        .mode(crate::authorization::AuthorizationMode::Rules)
        .deny("Skill(internal)")
        .build();

    let tools = Arc::new(
        ToolRegistry::builder()
            .access(ToolSurface::only(["Skill"]))
            .policy(policy)
            .skill_executor(SkillRuntime::new(skill_registry.clone()))
            .build(),
    );

    let orchestrator = PromptOrchestrator::new(StaticContext::new(), "claude-sonnet-4-5")
        .skill_registry(skill_registry);

    let agent = Agent::from_parts(
        Arc::new(client),
        Arc::new(AgentConfig::default()),
        tools,
        Arc::new(HookManager::new()),
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
    let mut usage = Usage::default();
    assert_eq!(usage.total(), 0);

    usage.input_tokens = 100;
    usage.output_tokens = 50;
    assert_eq!(usage.total(), 150);
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

    let mut hooks = HookManager::new();
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

    let mut hooks = HookManager::new();
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

    let mut hooks = HookManager::new();
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

    let mut registry = ToolRegistry::new();
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

    let mut registry = ToolRegistry::from_context(ExecutionContext::permissive());
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
