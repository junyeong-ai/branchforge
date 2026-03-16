//! TaskTool - spawns and manages subagent tasks.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::select;
use tracing::debug;

use super::task_output::TaskStatus;
use super::task_registry::{TaskAssistantMetadata, TaskExecutionSummary, TaskRegistry};
use crate::common::{Index, IndexRegistry};
use crate::hooks::{HookEvent, HookInput};
use crate::session::{SessionId, SessionManager};
use crate::subagents::{SubagentIndex, builtin_subagents};
use crate::tools::{ExecutionContext, SchemaTool};
use crate::types::{ContentBlock, Message, Role, ToolResult};

pub struct TaskTool {
    registry: TaskRegistry,
    subagent_registry: IndexRegistry<SubagentIndex>,
    max_background_tasks: usize,
    session_manager: Option<SessionManager>,
    delegation_runtime: Option<crate::agent::DelegationRuntime>,
}

impl TaskTool {
    pub fn new(registry: TaskRegistry) -> Self {
        let mut subagent_registry = IndexRegistry::new();
        subagent_registry.register_all(builtin_subagents());
        Self {
            registry,
            subagent_registry,
            max_background_tasks: 10,
            session_manager: None,
            delegation_runtime: None,
        }
    }

    pub fn session_manager(mut self, session_manager: SessionManager) -> Self {
        self.session_manager = Some(session_manager);
        self
    }

    pub fn subagent_registry(mut self, subagent_registry: IndexRegistry<SubagentIndex>) -> Self {
        self.subagent_registry = subagent_registry;
        self
    }

    pub fn max_background_tasks(mut self, max: usize) -> Self {
        self.max_background_tasks = max;
        self
    }

    pub(crate) fn delegation_runtime(mut self, runtime: crate::agent::DelegationRuntime) -> Self {
        self.delegation_runtime = Some(runtime);
        self
    }

    /// Generate description with dynamic subagent list.
    ///
    /// Use this method when building system prompts to include all registered
    /// subagents (both built-in and custom) in the tool description.
    pub fn description_with_subagents(&self) -> String {
        let subagents_desc = self
            .subagent_registry
            .iter()
            .map(|subagent| subagent.to_summary_line())
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            r#"Launch a new agent to handle complex, multi-step tasks autonomously.

The Task tool launches specialized agents (subprocesses) that autonomously handle complex tasks. Each agent type has specific capabilities and tools available to it.

Available agent types and the tools they have access to:
{}

When using the Task tool, you must specify a subagent_type parameter to select which agent type to use.

When NOT to use the Task tool:
- If you want to read a specific file path, use the Read or Glob tool instead of the Task tool, to find the match more quickly
- If you are searching for a specific class definition like "class Foo", use the Grep tool instead, to find the match more quickly
- If you are searching for code within a specific file or set of 2-3 files, use the Read tool instead of the Task tool, to find the match more quickly
- Other tasks that are not related to the agent descriptions above

Usage notes:
- Always include a short description (3-5 words) summarizing what the agent will do
- Launch multiple agents concurrently whenever possible, to maximize performance; to do that, use a single message with multiple tool uses
- When the agent is done, it will return its final assistant text/content plus any structured output and its agent_id. You can use this ID to resume the agent later or inspect richer results with TaskOutput.
- You can optionally run agents in the background using the run_in_background parameter. When an agent runs in the background, you will need to use TaskOutput to retrieve its results once it's done. You can continue to work while background agents run - when you need their results to continue you can use TaskOutput in blocking mode to pause and wait for their results.
- Agents can be resumed using the `resume` parameter by passing the agent ID from a previous invocation. When resumed, the agent continues with its full previous context preserved. When NOT resuming, each invocation starts fresh and you should provide a detailed task description with all necessary context.
- Provide clear, detailed prompts so the agent can work autonomously and return exactly the information you need.
- The agent's outputs should generally be trusted
- Clearly tell the agent whether you expect it to write code or just to do research (search, file reads, web fetches, etc.), since it is not aware of the user's intent
- If you need to launch multiple agents in parallel, send a single message with multiple Task tool calls.
- Use model="haiku" for quick, straightforward tasks to minimize cost and latency"#,
            subagents_desc
        )
    }

    async fn spawn_agent(
        &self,
        input: &TaskInput,
        task_session_id: SessionId,
        replay_messages: Option<Vec<Message>>,
    ) -> crate::Result<super::AgentResult> {
        let subagent = self
            .subagent_registry
            .get(&input.subagent_type)
            .ok_or_else(|| {
                crate::Error::Config(format!("Unknown subagent type: {}", input.subagent_type))
            })?;

        let task_session_manager = self.registry.session_manager();
        if let Some(ref runtime) = self.delegation_runtime {
            if let Some(ref messages) = replay_messages
                && !messages.is_empty()
            {
                debug!(
                    message_count = messages.len(),
                    "Resuming agent with previous context"
                );
            }
            runtime
                .execute_task(
                    subagent,
                    input,
                    task_session_manager,
                    task_session_id,
                    replay_messages,
                )
                .await
        } else {
            Err(crate::Error::Config(
                "Task tool requires a bound delegation runtime".to_string(),
            ))
        }
    }
}

impl TaskTool {
    fn parse_session_id(value: &str, field: &str) -> crate::Result<SessionId> {
        SessionId::parse(value)
            .ok_or_else(|| crate::Error::Config(format!("Invalid {field} session UUID '{value}'")))
    }

    async fn fire_start_hook(
        context: &ExecutionContext,
        session_id: &str,
        agent_id: &str,
        subagent_type: &str,
        description: &str,
    ) {
        context
            .fire_hook(
                HookEvent::SubagentStart,
                HookInput::subagent_start(session_id, agent_id, subagent_type, description),
            )
            .await;
    }

    async fn fire_stop_hook(
        context: &ExecutionContext,
        session_id: &str,
        agent_id: &str,
        success: bool,
        error: Option<String>,
    ) {
        context
            .fire_hook(
                HookEvent::SubagentStop,
                HookInput::subagent_stop(session_id, agent_id, success, error),
            )
            .await;
    }

    async fn replay_messages(
        &self,
        input: &TaskInput,
        context: &ExecutionContext,
    ) -> crate::Result<Option<Vec<Message>>> {
        let Some(replay_session) = input.replay_session.as_ref() else {
            return Ok(None);
        };
        let manager = self.session_manager.as_ref().ok_or_else(|| {
            crate::Error::Config("Task replay requires a bound session manager".to_string())
        })?;
        let session_id = Self::parse_session_id(replay_session, "replay_session")?;
        let from_node = parse_replay_node_id(input.replay_from_node.as_deref())?;

        let replay = match context.session_scope_ref() {
            Some(scope) => {
                manager
                    .replay_input_scoped(&session_id, scope, from_node)
                    .await?
            }
            None => manager.replay_input(&session_id, from_node).await?,
        };

        Ok(Some(replay.messages))
    }
}

fn parse_replay_node_id(value: Option<&str>) -> crate::Result<Option<uuid::Uuid>> {
    value
        .map(|value| {
            uuid::Uuid::parse_str(value).map_err(|error| {
                crate::Error::Config(format!(
                    "Invalid replay_from_node UUID '{}': {}",
                    value, error
                ))
            })
        })
        .transpose()
}

impl Clone for TaskTool {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            subagent_registry: self.subagent_registry.clone(),
            max_background_tasks: self.max_background_tasks,
            session_manager: self.session_manager.clone(),
            delegation_runtime: self.delegation_runtime.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TaskInput {
    /// A short (3-5 word) description of the task
    pub description: String,
    /// The task for the agent to perform
    pub prompt: String,
    /// The type of specialized agent to use for this task
    pub subagent_type: String,
    /// Optional model to use (sonnet/opus/haiku). Prefer haiku for quick tasks.
    #[serde(default)]
    pub model: Option<String>,
    /// Set to true to run in background. Use TaskOutput to read the output later.
    #[serde(default)]
    pub run_in_background: Option<bool>,
    /// Optional agent ID to resume from. The agent continues with preserved context.
    #[serde(default)]
    pub resume: Option<String>,
    /// Optional session id to replay from.
    #[serde(default)]
    pub replay_session: Option<String>,
    /// Optional node id to replay from within the replay session.
    #[serde(default)]
    pub replay_from_node: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutput {
    pub agent_id: String,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_metadata: Option<TaskAssistantMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<TaskExecutionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn final_assistant_content(messages: &[Message]) -> Option<Vec<ContentBlock>> {
    messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(|message| message.content.clone())
}

fn final_assistant_text(content: &[ContentBlock]) -> Option<String> {
    let text = Message {
        role: Role::Assistant,
        content: content.to_vec(),
    }
    .text();
    if text.is_empty() { None } else { Some(text) }
}

fn completed_task_output(agent_id: String, result: &super::AgentResult) -> TaskOutput {
    let content = final_assistant_content(&result.messages);
    let text = content
        .as_ref()
        .and_then(|content| final_assistant_text(content));

    TaskOutput {
        agent_id,
        status: TaskStatus::Completed,
        text,
        content,
        structured_output: result.structured_output.clone(),
        response_metadata: None,
        execution: Some(TaskExecutionSummary {
            result_uuid: Some(result.uuid.clone()),
            stop_reason: Some(result.stop_reason),
            iterations: Some(result.iterations),
            tool_calls: Some(result.tool_calls),
            usage: Some(result.usage),
            execution_time_ms: Some(result.metrics.execution_time_ms),
            api_calls: Some(result.metrics.api_calls),
            compactions: Some(result.metrics.compactions),
            errors: Some(result.metrics.errors),
            total_cost_usd: Some(result.metrics.total_cost_usd),
        }),
        error: None,
    }
}

impl TaskOutput {
    fn from_snapshot(agent_id: String, snapshot: super::task_registry::TaskResultSnapshot) -> Self {
        Self {
            agent_id,
            status: snapshot.status.into(),
            text: snapshot.text,
            content: snapshot.content,
            structured_output: snapshot.structured_output,
            response_metadata: snapshot.response_metadata,
            execution: snapshot.execution,
            error: snapshot.error,
        }
    }
}

#[async_trait]
impl SchemaTool for TaskTool {
    type Input = TaskInput;

    const NAME: &'static str = "Task";
    const DESCRIPTION: &'static str = "Launch a new agent to handle complex, multi-step tasks autonomously. Use description_with_subagents() for the full dynamic description including available agent types.";

    fn custom_description(&self) -> Option<String> {
        Some(self.description_with_subagents())
    }

    async fn handle(&self, input: TaskInput, context: &ExecutionContext) -> ToolResult {
        if let (Some(manager), Some(session_id)) =
            (self.session_manager.as_ref(), context.session_id())
        {
            let session_id = match Self::parse_session_id(session_id, "session") {
                Ok(session_id) => session_id,
                Err(error) => return ToolResult::error(error.to_string()),
            };
            let session = match context.session_scope_ref() {
                Some(scope) => manager.get_scoped(&session_id, scope).await,
                None => manager.get(&session_id).await,
            };
            if let Ok(session) = session
                && session.is_subagent()
            {
                return ToolResult::error("Nested subagents are not supported");
            }
        }

        let replay_messages = match self.replay_messages(&input, context).await {
            Ok(messages) => messages,
            Err(error) => return ToolResult::error(error.to_string()),
        };

        let task_session_id = match input.resume.as_deref() {
            Some(id) => match Self::parse_session_id(id, "resume") {
                Ok(session_id) => session_id,
                Err(error) => return ToolResult::error(error.to_string()),
            },
            None => SessionId::new(),
        };
        let agent_id = task_session_id.to_string();

        let session_id = context.session_id().unwrap_or("").to_string();
        let run_in_background = input.run_in_background.unwrap_or(false);

        if run_in_background {
            let cancel_rx = match self
                .registry
                .register_or_resume_background(
                    agent_id.clone(),
                    input.subagent_type.clone(),
                    input.description.clone(),
                    self.max_background_tasks,
                )
                .await
            {
                Ok(cancel_rx) => cancel_rx,
                Err(error) => return ToolResult::error(error.to_string()),
            };

            Self::fire_start_hook(
                context,
                &session_id,
                &agent_id,
                &input.subagent_type,
                &input.description,
            )
            .await;

            let registry = self.registry.clone();
            let task_id = agent_id.clone();
            let tool_clone = self.clone();
            let input_clone = input.clone();
            let context_clone = context.clone();
            let session_id_clone = session_id.clone();

            let handle = tokio::spawn(async move {
                select! {
                    result = tool_clone.spawn_agent(&input_clone, task_session_id, replay_messages.clone()) => {
                        match result {
                            Ok(agent_result) => {
                                if let Err(error) = registry.complete(&task_id, agent_result).await {
                                    debug!(task_id = %task_id, error = %error, "Failed to persist completed background task state");
                                    Self::fire_stop_hook(
                                        &context_clone,
                                        &session_id_clone,
                                        &task_id,
                                        false,
                                        Some(format!(
                                            "Delegated agent completed, but durable task finalization failed: {}",
                                            error
                                        )),
                                    ).await;
                                    return;
                                }
                                Self::fire_stop_hook(&context_clone, &session_id_clone, &task_id, true, None).await;
                            }
                            Err(e) => {
                                let error_msg = e.to_string();
                                if let Err(persist_error) =
                                    registry.fail(&task_id, error_msg.clone()).await
                                {
                                    debug!(
                                        task_id = %task_id,
                                        error = %persist_error,
                                        "Failed to persist failed background task state"
                                    );
                                    Self::fire_stop_hook(
                                        &context_clone,
                                        &session_id_clone,
                                        &task_id,
                                        false,
                                        Some(format!(
                                            "{} (durable failure recording also failed: {})",
                                            error_msg,
                                            persist_error
                                        )),
                                    ).await;
                                    return;
                                }
                                Self::fire_stop_hook(&context_clone, &session_id_clone, &task_id, false, Some(error_msg)).await;
                            }
                        }
                    }
                    _ = cancel_rx => {
                        Self::fire_stop_hook(&context_clone, &session_id_clone, &task_id, false, Some("Cancelled".to_string())).await;
                    }
                }
            });

            self.registry.set_handle(&agent_id, handle).await;

            let output = TaskOutput {
                agent_id: agent_id.clone(),
                status: TaskStatus::Running,
                text: None,
                content: None,
                structured_output: None,
                response_metadata: None,
                execution: None,
                error: None,
            };

            ToolResult::success(serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                format!(
                    "Task '{}' started in background. Agent ID: {}",
                    input.description, agent_id
                )
            }))
        } else {
            if let Err(error) = self
                .registry
                .register_or_resume(
                    agent_id.clone(),
                    input.subagent_type.clone(),
                    input.description.clone(),
                )
                .await
            {
                return ToolResult::error(error.to_string());
            }

            Self::fire_start_hook(
                context,
                &session_id,
                &agent_id,
                &input.subagent_type,
                &input.description,
            )
            .await;

            match self
                .spawn_agent(&input, task_session_id, replay_messages)
                .await
            {
                Ok(agent_result) => {
                    if let Err(error) = self
                        .registry
                        .complete(&agent_id, agent_result.clone())
                        .await
                    {
                        Self::fire_stop_hook(
                            context,
                            &session_id,
                            &agent_id,
                            false,
                            Some(error.to_string()),
                        )
                        .await;
                        return ToolResult::error(error.to_string());
                    }
                    Self::fire_stop_hook(context, &session_id, &agent_id, true, None).await;

                    let output = self
                        .registry
                        .get_result(&agent_id)
                        .await
                        .map(|snapshot| TaskOutput::from_snapshot(agent_id.clone(), snapshot))
                        .unwrap_or_else(|| completed_task_output(agent_id, &agent_result));
                    ToolResult::success(
                        serde_json::to_string_pretty(&output)
                            .unwrap_or_else(|_| agent_result.text.clone()),
                    )
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    if let Err(persist_error) =
                        self.registry.fail(&agent_id, error_msg.clone()).await
                    {
                        Self::fire_stop_hook(
                            context,
                            &session_id,
                            &agent_id,
                            false,
                            Some(persist_error.to_string()),
                        )
                        .await;
                        return ToolResult::error(persist_error.to_string());
                    }
                    Self::fire_stop_hook(
                        context,
                        &session_id,
                        &agent_id,
                        false,
                        Some(error_msg.clone()),
                    )
                    .await;

                    let output = TaskOutput {
                        agent_id,
                        status: TaskStatus::Failed,
                        text: None,
                        content: None,
                        structured_output: None,
                        response_metadata: None,
                        execution: None,
                        error: Some(error_msg.clone()),
                    };
                    ToolResult::error(serde_json::to_string_pretty(&output).unwrap_or(error_msg))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentMetrics, AgentResult, AgentState};
    use crate::session::{MemoryPersistence, SessionConfig, SessionManager};
    use crate::tools::{ExecutionContext, Tool};
    use crate::types::{ContentBlock, StopReason, Usage};

    fn test_context() -> ExecutionContext {
        ExecutionContext::default()
    }

    #[test]
    fn test_task_input_parsing() {
        let input: TaskInput = serde_json::from_value(serde_json::json!({
            "description": "Search files",
            "prompt": "Find all Rust files",
            "subagent_type": "explore"
        }))
        .unwrap();

        assert_eq!(input.description, "Search files");
        assert_eq!(input.subagent_type, "explore");
    }

    #[tokio::test]
    async fn test_max_background_limit() {
        let registry = TaskRegistry::new(std::sync::Arc::new(MemoryPersistence::new()));
        let tool = TaskTool::new(registry.clone()).max_background_tasks(1);
        let context = test_context();

        registry
            .register_or_resume_background(
                "00000000-0000-0000-0000-0000000000aa".into(),
                "explore".into(),
                "Existing task".into(),
                1,
            )
            .await
            .unwrap();
        registry
            .set_handle(
                "00000000-0000-0000-0000-0000000000aa",
                tokio::spawn(async {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }),
            )
            .await;

        let result = tool
            .execute(
                serde_json::json!({
                    "description": "New task",
                    "prompt": "Do something",
                    "subagent_type": "general",
                    "run_in_background": true
                }),
                &context,
            )
            .await;

        assert!(result.is_error());
        let _ = registry
            .cancel("00000000-0000-0000-0000-0000000000aa")
            .await;
    }

    #[test]
    fn test_subagent_registry_integration() {
        let registry = TaskRegistry::new(std::sync::Arc::new(MemoryPersistence::new()));
        let mut subagent_registry = IndexRegistry::new();
        subagent_registry.register_all(builtin_subagents());

        assert!(subagent_registry.contains("bash"));
        assert!(subagent_registry.contains("explore"));
        assert!(subagent_registry.contains("plan"));
        assert!(subagent_registry.contains("general"));

        let _tool = TaskTool::new(registry).subagent_registry(subagent_registry);
    }

    #[test]
    fn test_completed_task_output_preserves_full_assistant_content() {
        let result = AgentResult {
            text: "primary text".to_string(),
            usage: Usage {
                input_tokens: 5,
                output_tokens: 8,
                cache_read_input_tokens: Some(2),
                cache_creation_input_tokens: Some(1),
                server_tool_use: None,
            },
            tool_calls: 2,
            iterations: 3,
            stop_reason: StopReason::EndTurn,
            state: AgentState::Completed,
            metrics: AgentMetrics {
                execution_time_ms: 99,
                api_calls: 1,
                ..AgentMetrics::default()
            },
            session_id: SessionId::new().to_string(),
            structured_output: Some(serde_json::json!({"value": 42})),
            messages: vec![
                Message::user("run task"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::text("first "), ContentBlock::text("second")],
                },
            ],
            uuid: "test-uuid".to_string(),
        };

        let output = completed_task_output("task-id".to_string(), &result);
        assert_eq!(output.status, TaskStatus::Completed);
        assert_eq!(output.text.as_deref(), Some("first second"));
        assert_eq!(output.content.as_ref().map(Vec::len), Some(2));
        assert_eq!(
            output.structured_output,
            Some(serde_json::json!({"value": 42}))
        );
        assert_eq!(
            output
                .execution
                .as_ref()
                .and_then(|execution| execution.result_uuid.as_deref()),
            Some("test-uuid")
        );
        assert_eq!(
            output
                .execution
                .as_ref()
                .and_then(|execution| execution.tool_calls),
            Some(2)
        );
    }

    #[tokio::test]
    async fn test_replay_requires_bound_session_manager() {
        let registry = TaskRegistry::new(std::sync::Arc::new(MemoryPersistence::new()));
        let tool = TaskTool::new(registry);
        let context = test_context();

        let result = tool
            .execute(
                serde_json::json!({
                    "description": "Replay task",
                    "prompt": "Inspect prior session",
                    "subagent_type": "explore",
                    "replay_session": "session-123"
                }),
                &context,
            )
            .await;

        assert!(result.is_error());
        assert!(result.error_message().contains("bound session manager"));
    }

    #[tokio::test]
    async fn test_replay_respects_session_scope() {
        let registry = TaskRegistry::new(std::sync::Arc::new(MemoryPersistence::new()));
        let manager = SessionManager::in_memory();
        let session = manager
            .create_with_identity(SessionConfig::default(), "tenant-a", "user-1")
            .await
            .unwrap();
        manager
            .add_message(
                &session.id,
                crate::session::SessionMessage::user(vec![ContentBlock::text("hello")]),
            )
            .await
            .unwrap();

        let tool = TaskTool::new(registry).session_manager(manager);
        let context = test_context().session_scope(
            crate::session::SessionAccessScope::default()
                .tenant("tenant-a")
                .principal("user-2"),
        );

        let result = tool
            .execute(
                serde_json::json!({
                    "description": "Replay task",
                    "prompt": "Inspect prior session",
                    "subagent_type": "explore",
                    "replay_session": session.id.to_string()
                }),
                &context,
            )
            .await;

        assert!(result.is_error());
        assert!(
            result
                .error_message()
                .contains("outside the requested tenant/principal scope")
        );
    }

    #[tokio::test]
    async fn test_replay_rejects_invalid_node_uuid() {
        let registry = TaskRegistry::new(std::sync::Arc::new(MemoryPersistence::new()));
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        let tool = TaskTool::new(registry).session_manager(manager);
        let context = test_context();

        let result = tool
            .execute(
                serde_json::json!({
                    "description": "Replay task",
                    "prompt": "Inspect prior session",
                    "subagent_type": "explore",
                    "replay_session": session.id.to_string(),
                    "replay_from_node": "not-a-uuid"
                }),
                &context,
            )
            .await;

        assert!(result.is_error());
        assert!(
            result
                .error_message()
                .contains("Invalid replay_from_node UUID")
        );
    }

    #[tokio::test]
    async fn test_replay_rejects_invalid_session_uuid() {
        let registry = TaskRegistry::new(std::sync::Arc::new(MemoryPersistence::new()));
        let tool = TaskTool::new(registry).session_manager(SessionManager::in_memory());
        let context = test_context();

        let result = tool
            .execute(
                serde_json::json!({
                    "description": "Replay task",
                    "prompt": "Inspect prior session",
                    "subagent_type": "explore",
                    "replay_session": "not-a-uuid"
                }),
                &context,
            )
            .await;

        assert!(result.is_error());
        assert!(
            result
                .error_message()
                .contains("Invalid replay_session session UUID")
        );
    }
}
