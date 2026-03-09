//! TaskTool - spawns and manages subagent tasks.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::select;
use tracing::debug;

use super::AgentBuilder;
use super::task_registry::TaskRegistry;
use crate::auth::Auth;
use crate::client::CloudProvider;
use crate::common::{Index, IndexRegistry};
use crate::hooks::{HookEvent, HookInput};
use crate::session::SessionManager;
use crate::subagents::{SubagentIndex, builtin_subagents};
use crate::tools::{ExecutionContext, SchemaTool};
use crate::types::{Message, ToolResult};

pub struct TaskTool {
    registry: TaskRegistry,
    subagent_registry: IndexRegistry<SubagentIndex>,
    max_background_tasks: usize,
    session_manager: Option<SessionManager>,
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
- When the agent is done, it will return a single message back to you along with its agent_id. You can use this ID to resume the agent later if needed for follow-up work.
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
        previous_messages: Option<Vec<Message>>,
        replay_messages: Option<Vec<Message>>,
    ) -> crate::Result<super::AgentResult> {
        let subagent = self
            .subagent_registry
            .get(&input.subagent_type)
            .ok_or_else(|| {
                crate::Error::Config(format!("Unknown subagent type: {}", input.subagent_type))
            })?;

        let provider = CloudProvider::from_env();
        let model_config = provider.default_models();

        let model = input
            .model
            .as_deref()
            .map(|m| model_config.resolve_alias(m))
            .or(subagent.model.as_deref())
            .unwrap_or_else(|| subagent.resolve_model(&model_config))
            .to_string();

        let agent = AgentBuilder::new()
            .auth(Auth::FromEnv)
            .await?
            .model(&model)
            .max_iterations(50)
            .build()
            .await?;

        let boot_messages = replay_messages.or(previous_messages);

        match boot_messages {
            Some(messages) if !messages.is_empty() => {
                debug!(
                    message_count = messages.len(),
                    "Resuming agent with previous context"
                );
                agent.execute_with_messages(messages, &input.prompt).await
            }
            _ => agent.execute(&input.prompt).await,
        }
    }
}

impl TaskTool {
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
}

impl Clone for TaskTool {
    fn clone(&self) -> Self {
        Self {
            registry: self.registry.clone(),
            subagent_registry: self.subagent_registry.clone(),
            max_background_tasks: self.max_background_tasks,
            session_manager: self.session_manager.clone(),
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
    pub result: String,
    pub is_running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
        let previous_messages = if let Some(ref resume_id) = input.resume {
            self.registry.get_messages(resume_id).await
        } else {
            None
        };
        let replay_messages = if let Some(ref replay_session) = input.replay_session {
            let manager = self.session_manager.clone().unwrap_or_default();
            let session_id = crate::session::SessionId::from(replay_session.clone());
            let from_node = input
                .replay_from_node
                .as_ref()
                .and_then(|node| uuid::Uuid::parse_str(node).ok());
            manager
                .replay_input(&session_id, from_node)
                .await
                .ok()
                .flatten()
                .map(|replay| replay.messages)
        } else {
            None
        };

        let agent_id = input
            .resume
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()[..7].to_string());

        let session_id = context.session_id().unwrap_or("").to_string();
        let run_in_background = input.run_in_background.unwrap_or(false);

        if run_in_background {
            let running = self.registry.running_count().await;
            if running >= self.max_background_tasks {
                return ToolResult::error(format!(
                    "Maximum background tasks ({}) reached. Wait for existing tasks to complete.",
                    self.max_background_tasks
                ));
            }

            let cancel_rx = self
                .registry
                .register(
                    agent_id.clone(),
                    input.subagent_type.clone(),
                    input.description.clone(),
                )
                .await;

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
            let prev_messages = previous_messages.clone();
            let context_clone = context.clone();
            let session_id_clone = session_id.clone();

            let handle = tokio::spawn(async move {
                select! {
                    result = tool_clone.spawn_agent(&input_clone, prev_messages, replay_messages.clone()) => {
                        match result {
                            Ok(agent_result) => {
                                registry.save_messages(&task_id, agent_result.messages.clone()).await;
                                registry.complete(&task_id, agent_result).await;
                                Self::fire_stop_hook(&context_clone, &session_id_clone, &task_id, true, None).await;
                            }
                            Err(e) => {
                                let error_msg = e.to_string();
                                registry.fail(&task_id, error_msg.clone()).await;
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
                result: String::new(),
                is_running: true,
                error: None,
            };

            ToolResult::success(serde_json::to_string_pretty(&output).unwrap_or_else(|_| {
                format!(
                    "Task '{}' started in background. Agent ID: {}",
                    input.description, agent_id
                )
            }))
        } else {
            Self::fire_start_hook(
                context,
                &session_id,
                &agent_id,
                &input.subagent_type,
                &input.description,
            )
            .await;

            match self
                .spawn_agent(&input, previous_messages, replay_messages)
                .await
            {
                Ok(agent_result) => {
                    self.registry
                        .save_messages(&agent_id, agent_result.messages.clone())
                        .await;
                    Self::fire_stop_hook(context, &session_id, &agent_id, true, None).await;

                    let output = TaskOutput {
                        agent_id,
                        result: agent_result.text.clone(),
                        is_running: false,
                        error: None,
                    };
                    ToolResult::success(
                        serde_json::to_string_pretty(&output).unwrap_or(agent_result.text),
                    )
                }
                Err(e) => {
                    let error_msg = e.to_string();
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
                        result: String::new(),
                        is_running: false,
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
    use crate::tools::{ExecutionContext, Tool};

    fn test_context() -> ExecutionContext {
        ExecutionContext::default()
    }

    #[test]
    fn test_task_input_parsing() {
        let input: TaskInput = serde_json::from_value(serde_json::json!({
            "description": "Search files",
            "prompt": "Find all Rust files",
            "subagent_type": "Explore"
        }))
        .unwrap();

        assert_eq!(input.description, "Search files");
        assert_eq!(input.subagent_type, "Explore");
    }

    #[tokio::test]
    async fn test_max_background_limit() {
        use crate::session::MemoryPersistence;
        let registry = TaskRegistry::new(std::sync::Arc::new(MemoryPersistence::new()));
        let tool = TaskTool::new(registry.clone()).max_background_tasks(1);
        let context = test_context();

        registry
            .register("existing".into(), "Explore".into(), "Existing task".into())
            .await;

        let result = tool
            .execute(
                serde_json::json!({
                    "description": "New task",
                    "prompt": "Do something",
                    "subagent_type": "general-purpose",
                    "run_in_background": true
                }),
                &context,
            )
            .await;

        assert!(result.is_error());
    }

    #[test]
    fn test_subagent_registry_integration() {
        use crate::session::MemoryPersistence;
        let registry = TaskRegistry::new(std::sync::Arc::new(MemoryPersistence::new()));
        let mut subagent_registry = IndexRegistry::new();
        subagent_registry.register_all(builtin_subagents());

        assert!(subagent_registry.contains("Bash"));
        assert!(subagent_registry.contains("Explore"));
        assert!(subagent_registry.contains("Plan"));
        assert!(subagent_registry.contains("general-purpose"));

        let _tool = TaskTool::new(registry).subagent_registry(subagent_registry);
    }
}
