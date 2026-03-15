//! Todo tools for task tracking.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::SchemaTool;
use super::context::ExecutionContext;
use crate::session::SessionId;
use crate::session::session_state::ToolState;
use crate::session::types::{TodoItem, TodoStatus};
use crate::types::ToolResult;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TodoInputItem {
    #[schemars(length(min = 1))]
    pub content: String,
    pub status: TodoInputStatus,
    #[serde(rename = "activeForm")]
    #[schemars(length(min = 1))]
    pub active_form: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoInputStatus {
    Pending,
    InProgress,
    Completed,
}

impl From<TodoInputStatus> for TodoStatus {
    fn from(status: TodoInputStatus) -> Self {
        match status {
            TodoInputStatus::Pending => TodoStatus::Pending,
            TodoInputStatus::InProgress => TodoStatus::InProgress,
            TodoInputStatus::Completed => TodoStatus::Completed,
        }
    }
}

pub struct TodoWriteTool {
    state: ToolState,
    session_id: SessionId,
}

impl TodoWriteTool {
    pub fn new(state: ToolState, session_id: SessionId) -> Self {
        Self { state, session_id }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TodoWriteInput {
    /// The updated todo list
    pub todos: Vec<TodoInputItem>,
}

#[async_trait]
impl SchemaTool for TodoWriteTool {
    type Input = TodoWriteInput;

    const NAME: &'static str = "TodoWrite";
    const DESCRIPTION: &'static str = "Create and manage a structured task list for the current session. Each todo has a `content` (imperative: \"Fix bug\"), `activeForm` (continuous: \"Fixing bug\"), and `status` (pending | in_progress | completed). Only one task may be in_progress at a time.";

    async fn handle(&self, input: Self::Input, context: &ExecutionContext) -> ToolResult {
        let in_progress_count = input
            .todos
            .iter()
            .filter(|t| matches!(t.status, TodoInputStatus::InProgress))
            .count();

        if in_progress_count > 1 {
            return ToolResult::error(
                "Only one task can be in_progress at a time. Complete the current task first.",
            );
        }

        let todos: Vec<TodoItem> = input
            .todos
            .into_iter()
            .map(|t| {
                let mut item = TodoItem::new(self.session_id, &t.content, &t.active_form);
                match t.status {
                    TodoInputStatus::Pending => {}
                    TodoInputStatus::InProgress => item.start(),
                    TodoInputStatus::Completed => item.complete(),
                }
                item
            })
            .collect();

        self.state.set_todos(todos.clone()).await;
        if let Err(e) = context.persist_tool_state(&self.state).await {
            return ToolResult::error(format!("Failed to persist todo state: {}", e));
        }

        let mut response = String::from("Todo list updated:\n");
        for (i, todo) in todos.iter().enumerate() {
            response.push_str(&format!(
                "{}. {} {}\n",
                i + 1,
                todo.status_icon(),
                todo.content
            ));
        }

        ToolResult::success(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionAccessScope, SessionManager};
    use crate::tools::Tool;

    #[tokio::test]
    async fn test_todo_write() {
        let session_id = SessionId::new();
        let state = ToolState::new(session_id);
        let tool = TodoWriteTool::new(state, session_id);
        let execution_context = ExecutionContext::default();

        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [
                        {"content": "Fix bug", "status": "in_progress", "activeForm": "Fixing bug"},
                        {"content": "Write tests", "status": "pending", "activeForm": "Writing tests"}
                    ]
                }),
                &execution_context,
            )
            .await;

        assert!(!result.is_error());
        assert!(result.text().contains("Fix bug"));
    }

    #[tokio::test]
    async fn test_multiple_in_progress_rejected() {
        let session_id = SessionId::new();
        let state = ToolState::new(session_id);
        let tool = TodoWriteTool::new(state, session_id);
        let execution_context = ExecutionContext::default();

        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [
                        {"content": "Task 1", "status": "in_progress", "activeForm": "Doing 1"},
                        {"content": "Task 2", "status": "in_progress", "activeForm": "Doing 2"}
                    ]
                }),
                &execution_context,
            )
            .await;

        assert!(result.is_error());
    }

    #[tokio::test]
    async fn test_todo_write_persists_with_session_manager() {
        let manager = SessionManager::in_memory();
        let scope = SessionAccessScope::default()
            .tenant("tenant-a")
            .principal("user-1");
        let session_id = SessionId::new();
        let state = ToolState::new(session_id);
        let tool = TodoWriteTool::new(state.clone(), session_id);
        let execution_context = ExecutionContext::permissive()
            .session_manager(manager.clone())
            .session_scope(scope.clone());

        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [
                        {"content": "Fix bug", "status": "in_progress", "activeForm": "Fixing bug"}
                    ]
                }),
                &execution_context,
            )
            .await;

        assert!(!result.is_error());

        let stored = manager.scoped(scope).get(&session_id).await.unwrap();
        assert_eq!(stored.todos.len(), 1);
        assert_eq!(stored.todos[0].content, "Fix bug");
        assert_eq!(stored.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(stored.principal_id.as_deref(), Some("user-1"));
    }
}
