//! TaskOutputTool - retrieves results from running or completed tasks.

use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::task_registry::{
    TaskAssistantMetadata, TaskExecutionSummary, TaskRegistry, TaskResultSnapshot,
};
use crate::session::SessionState;
use crate::tools::{ExecutionContext, SchemaTool};
use crate::types::ToolResult;

#[derive(Clone)]
pub struct TaskOutputTool {
    registry: TaskRegistry,
}

impl TaskOutputTool {
    pub fn new(registry: TaskRegistry) -> Self {
        Self { registry }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TaskOutputInput {
    /// The task ID to get output from
    pub task_id: String,
    /// Whether to wait for completion
    #[serde(default = "default_block")]
    pub block: bool,
    /// Max wait time in ms
    #[serde(default = "default_timeout")]
    #[schemars(range(min = 0, max = 600000))]
    pub timeout: u64,
}

fn default_block() -> bool {
    true
}

fn default_timeout() -> u64 {
    30000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Finalizing,
    Completed,
    Failed,
    Cancelled,
    NotFound,
}

impl From<SessionState> for TaskStatus {
    fn from(state: SessionState) -> Self {
        match state {
            SessionState::Created | SessionState::Active | SessionState::WaitingForTools => {
                TaskStatus::Running
            }
            SessionState::Completing | SessionState::Failing | SessionState::Cancelling => {
                TaskStatus::Finalizing
            }
            SessionState::Completed => TaskStatus::Completed,
            SessionState::Failed => TaskStatus::Failed,
            SessionState::Cancelled => TaskStatus::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutputResult {
    pub task_id: String,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<crate::types::ContentBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_metadata: Option<TaskAssistantMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<TaskExecutionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[async_trait]
impl SchemaTool for TaskOutputTool {
    type Input = TaskOutputInput;

    const NAME: &'static str = "TaskOutput";
    const DESCRIPTION: &'static str = r#"
- Retrieves output from a running or completed Task session
- Takes a task_id parameter identifying the task
- Returns final assistant text, content blocks, response metadata, and execution summary
- Preserves structured output when the delegated agent produced schema-validated JSON
- Distinguishes actively running tasks from tasks that have finished execution but are still finalizing durable state
- Use block=true (default) to wait for task completion
- Use block=false for non-blocking check of current status
- Task IDs can be found using the Task tool response
- Task output is sourced from the TaskRegistry-backed child session for that task
- Important: task_id is the Task tool's returned session/task ID, not a process PID"#;

    async fn handle(&self, input: TaskOutputInput, _context: &ExecutionContext) -> ToolResult {
        let timeout = Duration::from_millis(input.timeout.min(600000));

        let result = if input.block {
            self.registry
                .wait_for_completion(&input.task_id, timeout)
                .await
        } else {
            self.registry.get_result(&input.task_id).await
        };

        let output = match result {
            Some(TaskResultSnapshot {
                status,
                text,
                content,
                structured_output,
                response_metadata,
                execution,
                error,
            }) => TaskOutputResult {
                task_id: input.task_id,
                status: status.into(),
                text,
                content,
                structured_output,
                response_metadata,
                execution,
                error,
            },
            None => TaskOutputResult {
                task_id: input.task_id,
                status: TaskStatus::NotFound,
                text: None,
                content: None,
                structured_output: None,
                response_metadata: None,
                execution: None,
                error: Some("Task not found".to_string()),
            },
        };

        ToolResult::success(
            serde_json::to_string_pretty(&output)
                .unwrap_or_else(|_| format!("Task status: {:?}", output.status)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentMetrics, AgentResult, AgentState};
    use crate::session::MemoryPersistence;
    use crate::tools::Tool;
    use crate::types::{ContentBlock, StopReason, ToolOutput, Usage};
    use std::sync::Arc;

    // Use valid UUIDs for tests to ensure consistent session IDs
    const TASK_1_UUID: &str = "00000000-0000-0000-0000-000000000011";
    const TASK_2_UUID: &str = "00000000-0000-0000-0000-000000000012";
    const TASK_3_UUID: &str = "00000000-0000-0000-0000-000000000013";

    fn test_registry() -> TaskRegistry {
        TaskRegistry::new(Arc::new(MemoryPersistence::new()))
    }

    fn mock_result(session_id: &str) -> AgentResult {
        AgentResult {
            text: "Completed successfully".to_string(),
            usage: Usage::default(),
            tool_calls: 0,
            iterations: 1,
            stop_reason: StopReason::EndTurn,
            state: AgentState::Completed,
            metrics: AgentMetrics::default(),
            session_id: session_id.to_string(),
            structured_output: None,
            messages: Vec::new(),
            uuid: "test-uuid".to_string(),
        }
    }

    #[tokio::test]
    async fn test_task_output_completed() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Test".into())
            .await
            .unwrap();
        registry
            .complete(TASK_1_UUID, mock_result(TASK_1_UUID))
            .await
            .unwrap();

        let tool = TaskOutputTool::new(registry);
        let context = crate::tools::ExecutionContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "task_id": TASK_1_UUID
                }),
                &context,
            )
            .await;

        assert!(!result.is_error());
        if let ToolOutput::Success(content) = &result.output {
            assert!(content.contains("completed"));
        }
    }

    #[tokio::test]
    async fn test_task_output_not_found() {
        let registry = test_registry();
        let tool = TaskOutputTool::new(registry);
        let context = crate::tools::ExecutionContext::default();

        let result = tool
            .execute(
                serde_json::json!({
                    "task_id": "nonexistent"
                }),
                &context,
            )
            .await;

        if let ToolOutput::Success(content) = &result.output {
            assert!(content.contains("not_found"));
        }
    }

    #[tokio::test]
    async fn test_task_output_non_blocking() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_2_UUID.into(), "Explore".into(), "Running".into())
            .await
            .unwrap();

        let tool = TaskOutputTool::new(registry);
        let context = crate::tools::ExecutionContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "task_id": TASK_2_UUID,
                    "block": false
                }),
                &context,
            )
            .await;

        if let ToolOutput::Success(content) = &result.output {
            assert!(content.contains("running"));
        }
    }

    #[tokio::test]
    async fn test_task_output_failed() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_3_UUID.into(), "Explore".into(), "Failing".into())
            .await
            .unwrap();
        registry
            .fail(TASK_3_UUID, "Something went wrong".into())
            .await
            .unwrap();

        let tool = TaskOutputTool::new(registry);
        let context = crate::tools::ExecutionContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "task_id": TASK_3_UUID
                }),
                &context,
            )
            .await;

        if let ToolOutput::Success(content) = &result.output {
            assert!(content.contains("failed"));
            assert!(content.contains("Something went wrong"));
        }
    }

    #[tokio::test]
    async fn test_task_output_includes_structured_output() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Structured".into())
            .await
            .unwrap();

        let manager = registry.session_manager();
        let session_id = crate::session::SessionId::parse(TASK_1_UUID).unwrap();
        manager
            .add_message(
                &session_id,
                crate::session::SessionMessage::assistant(vec![crate::types::ContentBlock::text(
                    "{\"value\":42}",
                )])
                .metadata(crate::session::MessageMetadata {
                    structured_output: Some(serde_json::json!({"value": 42})),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();

        registry
            .complete(TASK_1_UUID, mock_result(TASK_1_UUID))
            .await
            .unwrap();

        let tool = TaskOutputTool::new(registry);
        let context = crate::tools::ExecutionContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "task_id": TASK_1_UUID
                }),
                &context,
            )
            .await;

        if let ToolOutput::Success(content) = &result.output {
            assert!(content.contains("\"structured_output\""));
            assert!(content.contains("\"value\": 42"));
        }
    }

    #[tokio::test]
    async fn test_task_output_preserves_content_blocks_and_text() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Rich content".into())
            .await
            .unwrap();

        let manager = registry.session_manager();
        let session_id = crate::session::SessionId::parse(TASK_1_UUID).unwrap();
        manager
            .add_message(
                &session_id,
                crate::session::SessionMessage::assistant(vec![
                    ContentBlock::text("first "),
                    ContentBlock::text("second"),
                ]),
            )
            .await
            .unwrap();

        registry
            .complete(TASK_1_UUID, mock_result(TASK_1_UUID))
            .await
            .unwrap();

        let tool = TaskOutputTool::new(registry);
        let context = crate::tools::ExecutionContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "task_id": TASK_1_UUID
                }),
                &context,
            )
            .await;

        if let ToolOutput::Success(content) = &result.output {
            let parsed: TaskOutputResult = serde_json::from_str(content).unwrap();
            assert_eq!(parsed.text.as_deref(), Some("first second"));
            assert_eq!(parsed.content.as_ref().map(Vec::len), Some(2));
        }
    }

    #[tokio::test]
    async fn test_task_output_includes_execution_and_response_metadata() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Observed".into())
            .await
            .unwrap();

        let manager = registry.session_manager();
        let session_id = crate::session::SessionId::parse(TASK_1_UUID).unwrap();
        manager
            .add_message(
                &session_id,
                crate::session::SessionMessage::assistant(vec![ContentBlock::text("answer")])
                    .metadata(crate::session::MessageMetadata {
                        model: Some("claude-sonnet".to_string()),
                        request_id: Some("req_123".to_string()),
                        ..Default::default()
                    }),
            )
            .await
            .unwrap();

        let mut result = mock_result(TASK_1_UUID);
        result.uuid = "result-uuid".to_string();
        result.tool_calls = 3;
        result.iterations = 4;
        result.usage = Usage {
            input_tokens: 7,
            output_tokens: 9,
            cache_read_input_tokens: Some(2),
            cache_creation_input_tokens: Some(1),
            server_tool_use: None,
        };
        result.metrics.execution_time_ms = 125;
        result.metrics.api_calls = 2;
        registry.complete(TASK_1_UUID, result).await.unwrap();

        let tool = TaskOutputTool::new(registry);
        let context = crate::tools::ExecutionContext::default();
        let result = tool
            .execute(
                serde_json::json!({
                    "task_id": TASK_1_UUID
                }),
                &context,
            )
            .await;

        if let ToolOutput::Success(content) = &result.output {
            let parsed: TaskOutputResult = serde_json::from_str(content).unwrap();
            assert_eq!(
                parsed
                    .response_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.model.as_deref()),
                Some("claude-sonnet")
            );
            assert_eq!(
                parsed
                    .execution
                    .as_ref()
                    .and_then(|execution| execution.result_uuid.as_deref()),
                Some("result-uuid")
            );
            assert_eq!(
                parsed
                    .execution
                    .as_ref()
                    .and_then(|execution| execution.usage)
                    .map(|usage| usage.output_tokens),
                Some(9)
            );
        }
    }
}
