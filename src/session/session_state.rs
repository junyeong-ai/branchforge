//! Tool state for thread-safe state access.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Notify, RwLock, Semaphore};
use uuid::Uuid;

use super::queue::{MergedInput, QueueError, QueuedInput, SharedInputQueue};
use super::state::{Session, SessionConfig, SessionId};
use super::types::{CompactRecord, Plan, PlanStatus, TodoItem, ToolExecution};
use crate::graph::{NodeId, NodeKind};

const MAX_EXECUTION_LOG_SIZE: usize = 1000;

#[derive(Debug)]
struct ToolExecutionLog {
    entries: RwLock<VecDeque<ToolExecution>>,
}

impl Default for ToolExecutionLog {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolExecutionLog {
    fn new() -> Self {
        Self {
            entries: RwLock::new(VecDeque::with_capacity(64)),
        }
    }

    async fn append(&self, exec: ToolExecution) {
        let mut entries = self.entries.write().await;
        if entries.len() >= MAX_EXECUTION_LOG_SIZE {
            entries.pop_front();
        }
        entries.push_back(exec);
    }

    async fn with_entries<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&VecDeque<ToolExecution>) -> R,
    {
        let entries = self.entries.read().await;
        f(&entries)
    }

    async fn len(&self) -> usize {
        self.entries.read().await.len()
    }

    async fn clear(&self) {
        self.entries.write().await.clear();
    }
}

struct ToolStateInner {
    id: SessionId,
    session: RwLock<Session>,
    executions: ToolExecutionLog,
    input_queue: SharedInputQueue,
    execution_lock: Semaphore,
    executing: AtomicBool,
    queue_notify: Notify,
}

impl std::fmt::Debug for ToolStateInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolStateInner")
            .field("id", &self.id)
            .field("executions", &self.executions)
            .field("executing", &self.executing.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl ToolStateInner {
    fn new(session_id: SessionId) -> Self {
        Self {
            id: session_id,
            session: RwLock::new(Session::from_id(session_id, SessionConfig::default())),
            executions: ToolExecutionLog::new(),
            input_queue: SharedInputQueue::new(),
            execution_lock: Semaphore::new(1),
            executing: AtomicBool::new(false),
            queue_notify: Notify::new(),
        }
    }

    fn from_session(session: Session) -> Self {
        let id = session.id;
        Self {
            id,
            session: RwLock::new(session),
            executions: ToolExecutionLog::new(),
            input_queue: SharedInputQueue::new(),
            execution_lock: Semaphore::new(1),
            executing: AtomicBool::new(false),
            queue_notify: Notify::new(),
        }
    }
}

/// Thread-safe tool state handle.
#[derive(Debug, Clone)]
pub struct ToolState(Arc<ToolStateInner>);

impl ToolState {
    pub fn new(session_id: SessionId) -> Self {
        Self(Arc::new(ToolStateInner::new(session_id)))
    }

    pub fn from_session(session: Session) -> Self {
        Self(Arc::new(ToolStateInner::from_session(session)))
    }

    #[inline]
    pub fn session_id(&self) -> SessionId {
        self.0.id
    }

    pub async fn session(&self) -> Session {
        self.0.session.read().await.clone()
    }

    pub async fn update_session(&self, session: Session) {
        *self.0.session.write().await = session;
    }

    pub async fn enter_plan_mode(&self, name: Option<String>) -> Plan {
        self.0.session.write().await.enter_plan_mode(name).clone()
    }

    pub async fn current_plan(&self) -> Option<Plan> {
        self.0.session.read().await.current_plan.clone()
    }

    pub async fn update_plan_content(&self, content: String) {
        self.0.session.write().await.update_plan_content(content);
    }

    pub async fn exit_plan_mode(&self) -> Option<Plan> {
        self.0.session.write().await.exit_plan_mode()
    }

    pub async fn cancel_plan(&self) -> Option<Plan> {
        self.0.session.write().await.cancel_plan()
    }

    #[inline]
    pub async fn is_in_plan_mode(&self) -> bool {
        self.0.session.read().await.is_in_plan_mode()
    }

    pub async fn set_todos(&self, todos: Vec<TodoItem>) {
        self.0.session.write().await.set_todos(todos);
    }

    pub async fn todos(&self) -> Vec<TodoItem> {
        self.0.session.read().await.todos.clone()
    }

    #[inline]
    pub async fn todos_in_progress_count(&self) -> usize {
        self.0.session.read().await.todos_in_progress_count()
    }

    pub async fn record_tool_execution(&self, mut exec: ToolExecution) {
        let plan_id = {
            let mut session = self.0.session.write().await;
            let branch_id = session.graph.primary_branch;
            let plan_id = if let Some(ref plan) = session.current_plan
                && plan.status == PlanStatus::Executing
            {
                Some(plan.id)
            } else {
                None
            };
            session.graph.append_node(
                branch_id,
                NodeKind::ToolResult,
                serde_json::json!({
                    "tool_name": exec.tool_name,
                    "tool_input": exec.tool_input,
                    "tool_output": exec.tool_output,
                    "is_error": exec.is_error,
                    "error_message": exec.error_message,
                    "duration_ms": exec.duration_ms,
                    "message_id": exec.message_id,
                }),
            );
            plan_id
        };
        exec.plan_id = plan_id;
        self.0.executions.append(exec).await;
    }

    pub async fn append_graph_node(&self, kind: NodeKind, payload: serde_json::Value) -> NodeId {
        let mut session = self.0.session.write().await;
        let branch_id = session.graph.primary_branch;
        session.graph.append_node(branch_id, kind, payload)
    }

    pub async fn with_tool_executions<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&VecDeque<ToolExecution>) -> R,
    {
        self.0.executions.with_entries(f).await
    }

    pub async fn execution_log_len(&self) -> usize {
        self.0.executions.len().await
    }

    pub async fn clear_execution_log(&self) {
        self.0.executions.clear().await;
    }

    pub async fn record_compact(&self, record: CompactRecord) {
        self.0.session.write().await.record_compact(record);
    }

    pub async fn with_compact_history<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&VecDeque<CompactRecord>) -> R,
    {
        let session = self.0.session.read().await;
        f(&session.compact_history)
    }

    #[inline]
    pub async fn session_snapshot(&self) -> (SessionId, usize, Option<Plan>) {
        let session = self.0.session.read().await;
        (
            session.id,
            session.todos.len(),
            session.current_plan.clone(),
        )
    }

    #[inline]
    pub async fn execution_state(&self) -> (SessionId, bool, usize) {
        let session = self.0.session.read().await;
        (
            session.id,
            session.is_in_plan_mode(),
            session.todos_in_progress_count(),
        )
    }

    pub async fn record_execution_with_todos(
        &self,
        mut exec: ToolExecution,
        todos: Option<Vec<TodoItem>>,
    ) {
        let plan_id = {
            let mut session = self.0.session.write().await;
            let plan_id = if let Some(ref plan) = session.current_plan
                && plan.status == PlanStatus::Executing
            {
                Some(plan.id)
            } else {
                None
            };
            if let Some(todos) = todos {
                session.set_todos(todos);
            }
            plan_id
        };
        exec.plan_id = plan_id;
        self.0.executions.append(exec).await;
    }

    pub async fn enqueue(&self, content: impl Into<String>) -> Result<Uuid, QueueError> {
        let input = QueuedInput::new(self.session_id(), content);
        let result = self.0.input_queue.enqueue(input).await;
        if result.is_ok() {
            self.0.queue_notify.notify_waiters();
        }
        result
    }

    pub async fn dequeue_or_merge(&self) -> Option<MergedInput> {
        self.0.input_queue.merge_all().await
    }

    pub async fn pending_count(&self) -> usize {
        self.0.input_queue.pending_count().await
    }

    pub async fn cancel_pending(&self, id: Uuid) -> bool {
        self.0.input_queue.cancel(id).await.is_some()
    }

    pub async fn cancel_all_pending(&self) -> usize {
        self.0.input_queue.cancel_all().await.len()
    }

    pub fn is_executing(&self) -> bool {
        self.0.executing.load(Ordering::Acquire)
    }

    pub async fn wait_for_queue_signal(&self) {
        self.0.queue_notify.notified().await;
    }

    pub async fn acquire_execution(&self) -> ExecutionGuard<'_> {
        let permit = self
            .0
            .execution_lock
            .acquire()
            .await
            .expect("semaphore should not be closed");
        self.0.executing.store(true, Ordering::Release);
        ExecutionGuard {
            permit,
            executing: &self.0.executing,
            queue_notify: &self.0.queue_notify,
        }
    }

    pub fn try_acquire_execution(&self) -> Option<ExecutionGuard<'_>> {
        self.0.execution_lock.try_acquire().ok().map(|permit| {
            self.0.executing.store(true, Ordering::Release);
            ExecutionGuard {
                permit,
                executing: &self.0.executing,
                queue_notify: &self.0.queue_notify,
            }
        })
    }

    pub async fn with_session<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Session) -> R,
    {
        let session = self.0.session.read().await;
        f(&session)
    }

    pub async fn with_session_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Session) -> R,
    {
        let mut session = self.0.session.write().await;
        f(&mut session)
    }

    pub async fn compact(
        &self,
        client: &crate::Client,
        keep_messages: usize,
    ) -> crate::Result<crate::types::CompactResult> {
        let mut session = self.0.session.write().await;
        session.compact(client, keep_messages).await
    }
}

pub struct ExecutionGuard<'a> {
    #[allow(dead_code)]
    permit: tokio::sync::SemaphorePermit<'a>,
    executing: &'a AtomicBool,
    queue_notify: &'a Notify,
}

impl Drop for ExecutionGuard<'_> {
    fn drop(&mut self) {
        self.executing.store(false, Ordering::Release);
        self.queue_notify.notify_waiters();
    }
}

impl Default for ToolState {
    fn default() -> Self {
        Self::new(SessionId::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_plan_lifecycle() {
        let state = ToolState::new(SessionId::new());

        let plan = state.enter_plan_mode(Some("Test Plan".to_string())).await;
        assert_eq!(plan.status, PlanStatus::Draft);
        assert!(state.is_in_plan_mode().await);

        state
            .update_plan_content("Step 1\nStep 2".to_string())
            .await;

        let approved = state.exit_plan_mode().await;
        assert!(approved.is_some());
        assert_eq!(approved.unwrap().status, PlanStatus::Approved);
    }

    #[tokio::test]
    async fn test_todos() {
        let session_id = SessionId::new();
        let state = ToolState::new(session_id);

        let todos = vec![
            TodoItem::new(session_id, "Task 1", "Doing task 1"),
            TodoItem::new(session_id, "Task 2", "Doing task 2"),
        ];

        state.set_todos(todos).await;
        let loaded = state.todos().await;
        assert_eq!(loaded.len(), 2);
    }

    #[tokio::test]
    async fn test_tool_execution_recording() {
        let session_id = SessionId::new();
        let state = ToolState::new(session_id);

        let exec = ToolExecution::new(session_id, "Bash", serde_json::json!({"command": "ls"}))
            .output("file1\nfile2", false)
            .duration(100);

        state.record_tool_execution(exec).await;

        let count = state.with_tool_executions(|e| e.len()).await;
        assert_eq!(count, 1);

        let name = state
            .with_tool_executions(|e| e.front().map(|x| x.tool_name.clone()))
            .await;
        assert_eq!(name, Some("Bash".to_string()));
    }

    #[tokio::test]
    async fn test_session_persistence_ready() {
        let session_id = SessionId::new();
        let state = ToolState::new(session_id);

        let todos = vec![TodoItem::new(session_id, "Task 1", "Doing task 1")];
        state.set_todos(todos).await;
        state.enter_plan_mode(Some("My Plan".to_string())).await;

        let session = state.session().await;
        assert_eq!(session.todos.len(), 1);
        assert!(session.current_plan.is_some());
        assert_eq!(
            session.current_plan.unwrap().name,
            Some("My Plan".to_string())
        );
    }

    #[tokio::test]
    async fn test_resume_from_session() {
        let session_id = SessionId::new();

        let mut session = Session::new(SessionConfig::default());
        session.id = session_id;
        session.set_todos(vec![TodoItem::new(
            session_id,
            "Resumed Task",
            "Working on it",
        )]);
        session.enter_plan_mode(Some("Resumed Plan".to_string()));

        let state = ToolState::from_session(session);

        let todos = state.todos().await;
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].content, "Resumed Task");

        let plan = state.current_plan().await;
        assert!(plan.is_some());
        assert_eq!(plan.unwrap().name, Some("Resumed Plan".to_string()));
    }

    #[tokio::test]
    async fn test_concurrent_execution_recording() {
        let session_id = SessionId::new();
        let state = ToolState::new(session_id);

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let state = state.clone();
                let sid = session_id;
                tokio::spawn(async move {
                    let exec =
                        ToolExecution::new(sid, format!("Tool{}", i), serde_json::json!({"id": i}));
                    state.record_tool_execution(exec).await;
                })
            })
            .collect();

        for h in handles {
            h.await.unwrap();
        }

        let count = state.with_tool_executions(|e| e.len()).await;
        assert_eq!(count, 10);
    }

    #[tokio::test]
    async fn test_execution_log_limit() {
        let session_id = SessionId::new();
        let state = ToolState::new(session_id);

        for i in 0..MAX_EXECUTION_LOG_SIZE + 100 {
            let exec = ToolExecution::new(session_id, format!("Tool{}", i), serde_json::json!({}));
            state.record_tool_execution(exec).await;
        }

        let count = state.with_tool_executions(|e| e.len()).await;
        assert_eq!(count, MAX_EXECUTION_LOG_SIZE);

        let first_name = state
            .with_tool_executions(|e| e.front().map(|x| x.tool_name.clone()))
            .await;
        assert!(first_name.unwrap().contains("100"));
    }
}
