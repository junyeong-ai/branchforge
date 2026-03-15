//! Task registry for managing background agent tasks with Session-based persistence.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rust_decimal::Decimal;
use tokio::sync::{OnceCell, RwLock, oneshot};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::session::{
    ExecutionMetadata, MessageMetadata, Persistence, Session, SessionConfig, SessionError,
    SessionId, SessionManager, SessionResult, SessionState, SessionType, ThinkingMetadata,
    ToolResultMeta,
};
use crate::types::{ContentBlock, Message, Role, StopReason, Usage};

use super::AgentResult;

#[derive(Clone)]
enum PendingTaskTransition {
    Completed(Box<AgentResult>),
    Failed(String),
    Cancelled,
}

impl PendingTaskTransition {
    fn intent_state(&self) -> SessionState {
        match self {
            Self::Completed(_) => SessionState::Completing,
            Self::Failed(_) => SessionState::Failing,
            Self::Cancelled => SessionState::Cancelling,
        }
    }

    fn terminal_state(&self) -> SessionState {
        match self {
            Self::Completed(_) => SessionState::Completed,
            Self::Failed(_) => SessionState::Failed,
            Self::Cancelled => SessionState::Cancelled,
        }
    }
}

struct TaskRuntime {
    handle: Option<JoinHandle<()>>,
    cancel_tx: Option<oneshot::Sender<()>>,
    pending_transition: Option<PendingTaskTransition>,
    background_slot: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskAssistantMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_results: Option<Vec<ToolResultMeta>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingMetadata>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskExecutionSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iterations: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_calls: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compactions: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<Decimal>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskResultSnapshot {
    pub status: SessionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_metadata: Option<TaskAssistantMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<TaskExecutionSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct TaskRegistry {
    runtime: Arc<RwLock<HashMap<String, TaskRuntime>>>,
    reconciled: Arc<OnceCell<()>>,
    persistence: Arc<dyn Persistence>,
    parent_session_id: Option<SessionId>,
    default_ttl: Option<Duration>,
}

impl TaskRegistry {
    fn orphaned_task_error() -> String {
        "Task runtime no longer exists; marked failed during registry recovery".to_string()
    }

    fn invalid_task_id_error(id: &str) -> SessionError {
        SessionError::Storage {
            message: format!("Task IDs must be valid session UUIDs, got '{id}'"),
        }
    }

    fn parse_task_session_id(id: &str) -> Option<SessionId> {
        SessionId::parse(id)
    }

    pub fn new(persistence: Arc<dyn Persistence>) -> Self {
        Self {
            runtime: Arc::new(RwLock::new(HashMap::new())),
            reconciled: Arc::new(OnceCell::new()),
            persistence,
            parent_session_id: None,
            default_ttl: Some(Duration::from_secs(3600)),
        }
    }

    pub fn parent_session(mut self, parent_id: SessionId) -> Self {
        self.parent_session_id = Some(parent_id);
        self.reconciled = Arc::new(OnceCell::new());
        self
    }

    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    pub(crate) fn session_manager(&self) -> SessionManager {
        SessionManager::new(self.persistence.clone())
    }

    async fn ensure_reconciled(&self) {
        let registry = self.clone();
        self.reconciled
            .get_or_init(|| async move {
                registry.reconcile_all_orphaned().await;
            })
            .await;
    }

    async fn refresh_runtime_state(&self) {
        self.reconcile_finished_runtime_entries().await;
        self.ensure_reconciled().await;
    }

    async fn runtime_is_live(&self, id: &str) -> bool {
        let mut runtime = self.runtime.write().await;
        let Some(entry) = runtime.get_mut(id) else {
            return false;
        };

        if entry.pending_transition.is_some() {
            if entry
                .handle
                .as_ref()
                .is_some_and(|handle| handle.is_finished())
            {
                entry.handle.take();
                entry.cancel_tx.take();
            }
            return true;
        }

        let finished = entry
            .handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished());
        if finished {
            entry.handle.take();
            entry.cancel_tx.take();
            runtime.remove(id);
            return false;
        }

        true
    }

    async fn reconcile_finished_runtime_entries(&self) {
        let (finished_ids, retry_ids) = {
            let mut runtime = self.runtime.write().await;
            let finished_ids: Vec<String> = runtime
                .iter()
                .filter_map(|(id, entry)| {
                    entry
                        .handle
                        .as_ref()
                        .is_some_and(|handle| handle.is_finished())
                        .then_some(id.clone())
                })
                .collect();

            for id in &finished_ids {
                if let Some(entry) = runtime.get_mut(id) {
                    entry.handle.take();
                    entry.cancel_tx.take();
                }
            }

            let retry_ids: Vec<String> = runtime
                .iter()
                .filter_map(|(id, entry)| {
                    if entry.pending_transition.is_some() && entry.handle.is_none() {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();

            for id in &finished_ids {
                if let Some(entry) = runtime.get(id)
                    && entry.pending_transition.is_none()
                {
                    runtime.remove(id);
                }
            }

            (finished_ids, retry_ids)
        };

        for id in retry_ids {
            let _ = self.retry_pending_transition(&id).await;
        }

        for id in finished_ids {
            if self.runtime_is_live(&id).await {
                continue;
            }
            let Some(session_id) = Self::parse_task_session_id(&id) else {
                warn!(task_id = %id, "Skipping finished task reconciliation for invalid task id");
                continue;
            };
            let Some(session) = self.persistence.load(&session_id).await.ok().flatten() else {
                continue;
            };
            let _ = self.reconcile_orphaned_task(&id, session).await;
        }
    }

    async fn candidate_session_ids(&self) -> Vec<SessionId> {
        let result = match self.parent_session_id {
            Some(parent_session_id) => self.persistence.list_children(&parent_session_id).await,
            None => self.persistence.list(None).await,
        };

        match result {
            Ok(ids) => ids,
            Err(e) => {
                warn!(error = %e, "Failed to list sessions during orphaned task reconciliation");
                Vec::new()
            }
        }
    }

    async fn reconcile_orphaned_task(&self, id: &str, mut session: Session) -> Option<Session> {
        if let Some(parent_session_id) = self.parent_session_id
            && session.parent_id != Some(parent_session_id)
        {
            return Some(session);
        }
        if !session.is_subagent() {
            return Some(session);
        }

        if session.is_finalizing() {
            return self.finalize_persisted_transition(id, session).await;
        }

        if !session.is_running() || self.runtime_is_live(id).await {
            return Some(session);
        }

        session.state = SessionState::Failed;
        if session.error.is_none() {
            session.error = Some(Self::orphaned_task_error());
        }

        if let Err(e) = self.persistence.save(&session).await {
            warn!(
                session_id = %session.id,
                error = %e,
                "Failed to reconcile orphaned task session"
            );
            return None;
        }

        Some(session)
    }

    async fn finalize_persisted_transition(
        &self,
        id: &str,
        mut session: Session,
    ) -> Option<Session> {
        let original_session = session.clone();
        let Some(target_state) = session.state.terminal_from_finalizing() else {
            return Some(session);
        };
        session.set_state(target_state);

        if let Err(e) = self.persistence.save(&session).await {
            warn!(
                task_id = %id,
                session_id = %session.id,
                target_state = ?target_state,
                error = %e,
                "Failed to finalize persisted task transition"
            );
            return Some(original_session);
        }

        self.clear_runtime_entry(id).await;
        Some(session)
    }

    async fn load_reconciled(&self, id: &str) -> Option<Session> {
        let session_id = Self::parse_task_session_id(id)?;
        let session = self.persistence.load(&session_id).await.ok().flatten()?;
        self.reconcile_orphaned_task(id, session).await
    }

    async fn remember_pending_transition(&self, id: &str, transition: PendingTaskTransition) {
        let mut runtime = self.runtime.write().await;
        let entry = runtime.entry(id.to_string()).or_insert(TaskRuntime {
            handle: None,
            cancel_tx: None,
            pending_transition: None,
            background_slot: false,
        });
        entry.cancel_tx.take();
        entry.pending_transition = Some(transition);
        if entry
            .handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            entry.handle.take();
        }
    }

    async fn clear_runtime_entry(&self, id: &str) {
        let mut runtime = self.runtime.write().await;
        runtime.remove(id);
    }

    async fn reserve_runtime_entry(
        &self,
        id: &str,
        cancel_tx: oneshot::Sender<()>,
        background_slot: bool,
        max_background_tasks: Option<usize>,
    ) -> SessionResult<()> {
        let mut runtime = self.runtime.write().await;

        if runtime.contains_key(id) {
            return Err(SessionError::Storage {
                message: format!("Task {id} is already running"),
            });
        }

        if let Some(max_background_tasks) = max_background_tasks {
            let reserved_background_slots = runtime
                .values()
                .filter(|entry| {
                    entry.background_slot
                        && entry.pending_transition.is_none()
                        && !entry
                            .handle
                            .as_ref()
                            .is_some_and(|handle| handle.is_finished())
                })
                .count();

            if reserved_background_slots >= max_background_tasks {
                return Err(SessionError::Storage {
                    message: format!(
                        "Maximum background tasks ({max_background_tasks}) reached. Wait for existing tasks to complete."
                    ),
                });
            }
        }

        runtime.insert(
            id.to_string(),
            TaskRuntime {
                handle: None,
                cancel_tx: Some(cancel_tx),
                pending_transition: None,
                background_slot,
            },
        );

        Ok(())
    }

    async fn persist_transition(
        &self,
        id: &str,
        transition: &PendingTaskTransition,
    ) -> SessionResult<()> {
        let session_id =
            Self::parse_task_session_id(id).ok_or_else(|| Self::invalid_task_id_error(id))?;

        let mut session =
            self.persistence
                .load(&session_id)
                .await?
                .ok_or_else(|| SessionError::NotFound {
                    id: session_id.to_string(),
                })?;

        if session.state == transition.terminal_state() {
            return Ok(());
        }

        if session.state != transition.intent_state() {
            match transition {
                PendingTaskTransition::Completed(result) => {
                    if result.session_id != session_id.to_string() {
                        return Err(SessionError::Storage {
                            message: format!(
                                "Task completed with mismatched delegated session id {} (expected {})",
                                result.session_id, session_id
                            ),
                        });
                    }

                    session.set_state(SessionState::Completing);
                    session.error = None;
                    let _ = session.update_latest_assistant_metadata(Self::merge_result_metadata(
                        &session, result,
                    ));
                }
                PendingTaskTransition::Failed(error) => {
                    session.set_state(SessionState::Failing);
                    session.error = Some(error.clone());
                }
                PendingTaskTransition::Cancelled => {
                    session.set_state(SessionState::Cancelling);
                    session.error = None;
                }
            }

            self.persistence.save(&session).await?;
        }

        session.set_state(transition.terminal_state());
        match transition {
            PendingTaskTransition::Completed(result) => {
                if result.session_id != session_id.to_string() {
                    return Err(SessionError::Storage {
                        message: format!(
                            "Task completed with mismatched delegated session id {} (expected {})",
                            result.session_id, session_id
                        ),
                    });
                }

                session.error = None;
                self.persistence.save(&session).await
            }
            PendingTaskTransition::Failed(error) => {
                session.error = Some(error.clone());
                self.persistence.save(&session).await
            }
            PendingTaskTransition::Cancelled => {
                session.error = None;
                self.persistence.save(&session).await
            }
        }
    }

    async fn apply_transition(
        &self,
        id: &str,
        transition: PendingTaskTransition,
    ) -> SessionResult<()> {
        match self.persist_transition(id, &transition).await {
            Ok(()) => {
                self.clear_runtime_entry(id).await;
                Ok(())
            }
            Err(error) => {
                self.remember_pending_transition(id, transition).await;
                Err(error)
            }
        }
    }

    async fn retry_pending_transition(&self, id: &str) -> SessionResult<()> {
        let transition = {
            self.runtime
                .read()
                .await
                .get(id)
                .and_then(|entry| entry.pending_transition.clone())
        };

        let Some(transition) = transition else {
            return Ok(());
        };

        match self.persist_transition(id, &transition).await {
            Ok(()) => {
                self.clear_runtime_entry(id).await;
                Ok(())
            }
            Err(error) => {
                warn!(
                    task_id = %id,
                    error = %error,
                    "Retrying pending task terminal transition failed"
                );
                Err(error)
            }
        }
    }

    async fn reconcile_all_orphaned(&self) {
        for session_id in self.candidate_session_ids().await {
            let id = session_id.to_string();
            let Some(session) = self.persistence.load(&session_id).await.ok().flatten() else {
                continue;
            };
            let _ = self.reconcile_orphaned_task(&id, session).await;
        }
    }

    async fn register_or_resume_internal(
        &self,
        id: String,
        agent_type: String,
        description: String,
        background_slot: bool,
        max_background_tasks: Option<usize>,
    ) -> SessionResult<oneshot::Receiver<()>> {
        self.refresh_runtime_state().await;

        let session_id =
            Self::parse_task_session_id(&id).ok_or_else(|| Self::invalid_task_id_error(&id))?;
        let (cancel_tx, cancel_rx) = oneshot::channel();

        self.reserve_runtime_entry(&id, cancel_tx, background_slot, max_background_tasks)
            .await?;

        let existing_session = match self.persistence.load(&session_id).await {
            Ok(session) => session,
            Err(error) => {
                self.clear_runtime_entry(&id).await;
                return Err(error);
            }
        };

        let session = match existing_session {
            Some(mut session) => {
                if let Some(parent_id) = self.parent_session_id
                    && session.parent_id != Some(parent_id)
                {
                    self.clear_runtime_entry(&id).await;
                    return Err(SessionError::Storage {
                        message: format!(
                            "Task session {} is not a child of parent session {}",
                            session_id, parent_id
                        ),
                    });
                }
                if session.is_finalizing() {
                    self.clear_runtime_entry(&id).await;
                    return Err(SessionError::Storage {
                        message: format!(
                            "Task session {} is still finalizing durable state",
                            session_id
                        ),
                    });
                }
                if !session.is_subagent() {
                    self.clear_runtime_entry(&id).await;
                    return Err(SessionError::Storage {
                        message: format!(
                            "Task session {} is not a subagent session and cannot be resumed",
                            session_id
                        ),
                    });
                }
                session.set_state(SessionState::Active);
                session.error = None;
                session
            }
            None => {
                let config = SessionConfig {
                    ttl_secs: self.default_ttl.map(|d| d.as_secs()),
                    ..Default::default()
                };

                let mut session = match self.parent_session_id {
                    Some(parent_id) => Session::new_subagent_with_id(
                        session_id,
                        parent_id,
                        &agent_type,
                        &description,
                        config,
                    ),
                    None => {
                        let mut s = Session::from_id(session_id, config);
                        s.session_type = SessionType::Subagent {
                            agent_type,
                            description,
                        };
                        s
                    }
                };

                session.set_state(SessionState::Active);
                if let Some(parent_id) = self.parent_session_id
                    && let Ok(Some(parent)) = self.persistence.load(&parent_id).await
                {
                    session.tenant_id = parent.tenant_id;
                    session.principal_id = parent.principal_id;
                }
                session
            }
        };

        if let Err(error) = self.persistence.save(&session).await {
            self.clear_runtime_entry(&id).await;
            return Err(error);
        }

        Ok(cancel_rx)
    }

    pub async fn register_or_resume(
        &self,
        id: String,
        agent_type: String,
        description: String,
    ) -> SessionResult<oneshot::Receiver<()>> {
        self.register_or_resume_internal(id, agent_type, description, false, None)
            .await
    }

    pub async fn register_or_resume_background(
        &self,
        id: String,
        agent_type: String,
        description: String,
        max_background_tasks: usize,
    ) -> SessionResult<oneshot::Receiver<()>> {
        self.register_or_resume_internal(
            id,
            agent_type,
            description,
            true,
            Some(max_background_tasks),
        )
        .await
    }

    pub async fn set_handle(&self, id: &str, handle: JoinHandle<()>) {
        let mut runtime = self.runtime.write().await;
        if let Some(rt) = runtime.get_mut(id) {
            rt.handle = Some(handle);
        }
    }

    pub async fn complete(&self, id: &str, result: AgentResult) -> SessionResult<()> {
        self.refresh_runtime_state().await;
        let session_id =
            Self::parse_task_session_id(id).ok_or_else(|| Self::invalid_task_id_error(id))?;

        if result.session_id != session_id.to_string() {
            let error = format!(
                "Task completed with mismatched delegated session id {} (expected {})",
                result.session_id, session_id
            );
            warn!(
                task_id = %id,
                result_session_id = %result.session_id,
                expected_session_id = %session_id,
                "Task completion rejected due to mismatched delegated session id"
            );
            return self
                .apply_transition(id, PendingTaskTransition::Failed(error))
                .await;
        }

        self.apply_transition(id, PendingTaskTransition::Completed(Box::new(result)))
            .await
    }

    pub async fn fail(&self, id: &str, error: String) -> SessionResult<()> {
        self.refresh_runtime_state().await;
        self.apply_transition(id, PendingTaskTransition::Failed(error))
            .await
    }

    pub async fn cancel(&self, id: &str) -> SessionResult<bool> {
        self.refresh_runtime_state().await;
        let Some(_session_id) = Self::parse_task_session_id(id) else {
            return Err(Self::invalid_task_id_error(id));
        };

        let cancelled = {
            let mut runtime = self.runtime.write().await;
            if let Some(rt) = runtime.get_mut(id) {
                if let Some(tx) = rt.cancel_tx.take() {
                    let _ = tx.send(());
                }
                if let Some(handle) = rt.handle.take() {
                    handle.abort();
                }
                true
            } else {
                false
            }
        };

        if cancelled {
            self.apply_transition(id, PendingTaskTransition::Cancelled)
                .await?;
        }

        Ok(cancelled)
    }

    pub async fn get_status(&self, id: &str) -> Option<SessionState> {
        self.refresh_runtime_state().await;
        self.load_reconciled(id).await.map(|s| s.state)
    }

    fn result_snapshot(session: Session) -> TaskResultSnapshot {
        let assistant_message = session
            .current_branch_messages()
            .into_iter()
            .rev()
            .find(|message| message.role == Role::Assistant);
        let response_metadata = assistant_message
            .as_ref()
            .and_then(|message| Self::assistant_metadata(&message.metadata));
        let execution = assistant_message.as_ref().and_then(Self::execution_summary);
        let content = assistant_message
            .as_ref()
            .map(|message| message.content.clone());
        let text = assistant_message
            .as_ref()
            .map(|message| message.to_api_message().text())
            .filter(|text| !text.is_empty());
        let structured_output =
            assistant_message.and_then(|message| message.metadata.structured_output);

        TaskResultSnapshot {
            status: session.state,
            text,
            content,
            structured_output,
            response_metadata,
            execution,
            error: session.error,
        }
    }

    fn assistant_metadata(metadata: &MessageMetadata) -> Option<TaskAssistantMetadata> {
        (metadata.model.is_some()
            || metadata.request_id.is_some()
            || metadata.tool_results.is_some()
            || metadata.thinking.is_some())
        .then(|| TaskAssistantMetadata {
            model: metadata.model.clone(),
            request_id: metadata.request_id.clone(),
            tool_results: metadata.tool_results.clone(),
            thinking: metadata.thinking.clone(),
        })
    }

    fn execution_summary(message: &crate::session::SessionMessage) -> Option<TaskExecutionSummary> {
        let execution = message.metadata.execution.as_ref();
        let usage = execution.and_then(|metadata| metadata.usage).or_else(|| {
            message.usage.as_ref().map(|usage| Usage {
                input_tokens: usage.input_tokens as u32,
                output_tokens: usage.output_tokens as u32,
                cache_read_input_tokens: Some(usage.cache_read_input_tokens as u32),
                cache_creation_input_tokens: Some(usage.cache_creation_input_tokens as u32),
                server_tool_use: None,
            })
        });

        (execution.is_some() || usage.is_some()).then(|| TaskExecutionSummary {
            result_uuid: execution.and_then(|metadata| metadata.result_uuid.clone()),
            stop_reason: execution.and_then(|metadata| metadata.stop_reason),
            iterations: execution.and_then(|metadata| metadata.iterations),
            tool_calls: execution.and_then(|metadata| metadata.tool_calls),
            usage,
            execution_time_ms: execution.and_then(|metadata| metadata.execution_time_ms),
            api_calls: execution.and_then(|metadata| metadata.api_calls),
            compactions: execution.and_then(|metadata| metadata.compactions),
            errors: execution.and_then(|metadata| metadata.errors),
            total_cost_usd: execution.and_then(|metadata| metadata.total_cost_usd),
        })
    }

    fn merge_result_metadata(session: &Session, result: &AgentResult) -> MessageMetadata {
        let existing = session
            .current_branch_messages()
            .into_iter()
            .rev()
            .find(|message| message.role == Role::Assistant)
            .map(|message| message.metadata)
            .unwrap_or_default();

        MessageMetadata {
            structured_output: existing
                .structured_output
                .or_else(|| result.structured_output.clone()),
            execution: Some(ExecutionMetadata {
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
            ..existing
        }
    }

    pub async fn get_result(&self, id: &str) -> Option<TaskResultSnapshot> {
        self.refresh_runtime_state().await;
        self.load_reconciled(id).await.map(Self::result_snapshot)
    }

    pub async fn wait_for_completion(
        &self,
        id: &str,
        timeout: Duration,
    ) -> Option<TaskResultSnapshot> {
        let deadline = std::time::Instant::now() + timeout;
        let poll_interval = Duration::from_millis(100);

        loop {
            if let Some(snapshot) = self.get_result(id).await {
                if !snapshot.status.is_running() && !snapshot.status.is_finalizing() {
                    return Some(snapshot);
                }
            } else {
                return None;
            }

            if std::time::Instant::now() >= deadline {
                return self.get_result(id).await;
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    pub async fn list_running(&self) -> Vec<(String, String, Duration)> {
        self.refresh_runtime_state().await;
        let runtime = self.runtime.read().await;
        let mut result = Vec::new();

        for id in runtime.keys() {
            let Some(session_id) = Self::parse_task_session_id(id.as_str()) else {
                continue;
            };
            let Some(entry) = runtime.get(id) else {
                continue;
            };
            if !entry.background_slot {
                continue;
            }
            if let Ok(Some(session)) = self.persistence.load(&session_id).await
                && session.is_running()
            {
                let description = match &session.session_type {
                    SessionType::Subagent { description, .. } => description.clone(),
                    _ => String::new(),
                };
                let elapsed = (chrono::Utc::now() - session.created_at)
                    .to_std()
                    .unwrap_or_default();
                result.push((id.clone(), description, elapsed));
            }
        }

        result
    }

    pub async fn cleanup_completed(&self) -> SessionResult<usize> {
        self.persistence.cleanup_expired().await
    }

    pub async fn running_count(&self) -> usize {
        self.refresh_runtime_state().await;
        self.runtime
            .read()
            .await
            .values()
            .filter(|entry| {
                entry.background_slot
                    && entry.pending_transition.is_none()
                    && !entry
                        .handle
                        .as_ref()
                        .is_some_and(|handle| handle.is_finished())
            })
            .count()
    }

    pub async fn get_messages(&self, id: &str) -> Option<Vec<Message>> {
        self.refresh_runtime_state().await;
        self.load_reconciled(id).await.map(|s| s.to_api_messages())
    }

    pub async fn get_session(&self, id: &str) -> Option<Session> {
        self.refresh_runtime_state().await;
        self.load_reconciled(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentState;
    use crate::session::{MemoryPersistence, QueueItem, SessionMessage};
    use crate::types::{Role, StopReason, Usage};
    use std::sync::atomic::{AtomicBool, Ordering};
    use uuid::Uuid;

    #[derive(Clone)]
    struct FailingTerminalSavePersistence {
        inner: Arc<MemoryPersistence>,
        fail_terminal_save: Arc<AtomicBool>,
    }

    impl FailingTerminalSavePersistence {
        fn new() -> Self {
            Self {
                inner: Arc::new(MemoryPersistence::new()),
                fail_terminal_save: Arc::new(AtomicBool::new(false)),
            }
        }

        fn set_fail_terminal_save(&self, fail: bool) {
            self.fail_terminal_save.store(fail, Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl Persistence for FailingTerminalSavePersistence {
        fn name(&self) -> &str {
            "failing-memory"
        }

        async fn save(&self, session: &Session) -> SessionResult<()> {
            if self.fail_terminal_save.load(Ordering::SeqCst)
                && session.is_subagent()
                && matches!(
                    session.state,
                    SessionState::Completed | SessionState::Failed | SessionState::Cancelled
                )
            {
                return Err(SessionError::Storage {
                    message: "Injected terminal task persistence failure".to_string(),
                });
            }

            self.inner.save(session).await
        }

        async fn load(&self, id: &SessionId) -> SessionResult<Option<Session>> {
            self.inner.load(id).await
        }

        async fn delete(&self, id: &SessionId) -> SessionResult<bool> {
            self.inner.delete(id).await
        }

        async fn list(&self, tenant_id: Option<&str>) -> SessionResult<Vec<SessionId>> {
            self.inner.list(tenant_id).await
        }

        async fn list_children(&self, parent_id: &SessionId) -> SessionResult<Vec<SessionId>> {
            self.inner.list_children(parent_id).await
        }

        async fn enqueue(
            &self,
            session_id: &SessionId,
            content: String,
            priority: i32,
        ) -> SessionResult<QueueItem> {
            self.inner.enqueue(session_id, content, priority).await
        }

        async fn dequeue(&self, session_id: &SessionId) -> SessionResult<Option<QueueItem>> {
            self.inner.dequeue(session_id).await
        }

        async fn cancel_queued(&self, item_id: Uuid) -> SessionResult<bool> {
            self.inner.cancel_queued(item_id).await
        }

        async fn pending_queue(&self, session_id: &SessionId) -> SessionResult<Vec<QueueItem>> {
            self.inner.pending_queue(session_id).await
        }

        async fn replace_pending_queue(
            &self,
            session_id: &SessionId,
            items: &[QueueItem],
        ) -> SessionResult<()> {
            self.inner.replace_pending_queue(session_id, items).await
        }

        async fn restore_bundle(
            &self,
            session: &Session,
            pending_queue: &[QueueItem],
        ) -> SessionResult<()> {
            self.inner.restore_bundle(session, pending_queue).await
        }

        async fn cleanup_expired(&self) -> SessionResult<usize> {
            self.inner.cleanup_expired().await
        }
    }

    fn test_registry() -> TaskRegistry {
        TaskRegistry::new(Arc::new(MemoryPersistence::new()))
    }

    // Use valid UUIDs for tests to ensure consistent session IDs
    const TASK_1_UUID: &str = "00000000-0000-0000-0000-000000000001";
    const TASK_2_UUID: &str = "00000000-0000-0000-0000-000000000002";
    const TASK_3_UUID: &str = "00000000-0000-0000-0000-000000000003";
    const TASK_4_UUID: &str = "00000000-0000-0000-0000-000000000004";

    fn mock_result(session_id: &str) -> AgentResult {
        AgentResult {
            text: "Test result".to_string(),
            usage: Usage::default(),
            tool_calls: 0,
            iterations: 1,
            stop_reason: StopReason::EndTurn,
            state: AgentState::Completed,
            metrics: Default::default(),
            session_id: session_id.to_string(),
            structured_output: None,
            messages: Vec::new(),
            uuid: "test-uuid".to_string(),
        }
    }

    #[tokio::test]
    async fn test_register_and_complete() {
        let registry = test_registry();
        let _cancel_rx = registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Test task".into())
            .await
            .unwrap();

        assert_eq!(
            registry.get_status(TASK_1_UUID).await,
            Some(SessionState::Active)
        );

        registry
            .complete(TASK_1_UUID, mock_result(TASK_1_UUID))
            .await
            .unwrap();

        let result = registry.get_result(TASK_1_UUID).await.unwrap();
        assert_eq!(result.status, SessionState::Completed);
    }

    #[tokio::test]
    async fn test_complete_retries_pending_transition_after_persistence_failure() {
        let persistence = Arc::new(FailingTerminalSavePersistence::new());
        let registry = TaskRegistry::new(persistence.clone());

        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Retry".into())
            .await
            .unwrap();

        persistence.set_fail_terminal_save(true);
        let error = registry
            .complete(TASK_1_UUID, mock_result(TASK_1_UUID))
            .await
            .expect_err("terminal persistence failure should surface");
        assert!(
            error
                .to_string()
                .contains("Injected terminal task persistence failure")
        );
        assert_eq!(registry.running_count().await, 0);
        assert_eq!(
            registry.get_status(TASK_1_UUID).await,
            Some(SessionState::Completing)
        );

        persistence.set_fail_terminal_save(false);
        let snapshot = registry
            .wait_for_completion(TASK_1_UUID, Duration::from_millis(500))
            .await
            .expect("pending transition should be retried");
        assert_eq!(snapshot.status, SessionState::Completed);
        assert_eq!(registry.running_count().await, 0);
    }

    #[tokio::test]
    async fn test_restarted_registry_finalizes_durable_transition() {
        let persistence = Arc::new(FailingTerminalSavePersistence::new());
        let registry = TaskRegistry::new(persistence.clone());

        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Restart".into())
            .await
            .unwrap();

        persistence.set_fail_terminal_save(true);
        registry
            .complete(TASK_1_UUID, mock_result(TASK_1_UUID))
            .await
            .expect_err("terminal persistence failure should surface");
        assert_eq!(
            registry.get_status(TASK_1_UUID).await,
            Some(SessionState::Completing)
        );

        persistence.set_fail_terminal_save(false);
        let restarted = TaskRegistry::new(persistence);
        assert_eq!(
            restarted.get_status(TASK_1_UUID).await,
            Some(SessionState::Completed)
        );
    }

    #[tokio::test]
    async fn test_orphaned_active_task_is_reconciled_to_failed() {
        let persistence = Arc::new(MemoryPersistence::new());
        let registry = TaskRegistry::new(persistence.clone());
        let restarted = TaskRegistry::new(persistence);

        let _cancel_rx = registry
            .register_or_resume(TASK_2_UUID.into(), "Explore".into(), "Test task".into())
            .await
            .unwrap();

        let status = restarted.get_status(TASK_2_UUID).await;
        assert_eq!(status, Some(SessionState::Failed));

        let result = restarted.get_result(TASK_2_UUID).await.unwrap();
        assert_eq!(result.error, Some(TaskRegistry::orphaned_task_error()));
    }

    #[tokio::test]
    async fn test_finished_handle_without_cleanup_is_reconciled_to_failed() {
        let persistence = Arc::new(MemoryPersistence::new());
        let registry = TaskRegistry::new(persistence);

        let _cancel_rx = registry
            .register_or_resume(TASK_2_UUID.into(), "Explore".into(), "Finished task".into())
            .await
            .unwrap();

        let handle = tokio::spawn(async {});
        registry.set_handle(TASK_2_UUID, handle).await;
        tokio::task::yield_now().await;

        let status = registry.get_status(TASK_2_UUID).await;
        assert_eq!(status, Some(SessionState::Failed));

        let result = registry.get_result(TASK_2_UUID).await.unwrap();
        assert_eq!(result.error, Some(TaskRegistry::orphaned_task_error()));
    }

    #[tokio::test]
    async fn test_finished_handle_is_removed_from_running_views() {
        let registry = test_registry();

        let _cancel_rx = registry
            .register_or_resume(TASK_3_UUID.into(), "Explore".into(), "Finished task".into())
            .await
            .unwrap();

        let handle = tokio::spawn(async {});
        registry.set_handle(TASK_3_UUID, handle).await;
        tokio::task::yield_now().await;

        assert_eq!(registry.running_count().await, 0);
        assert!(registry.list_running().await.is_empty());
        assert_eq!(
            registry.get_status(TASK_3_UUID).await,
            Some(SessionState::Failed)
        );
    }

    #[tokio::test]
    async fn test_background_registration_reserves_slot_before_handle_is_bound() {
        let registry = test_registry();

        registry
            .register_or_resume_background(
                TASK_3_UUID.into(),
                "Explore".into(),
                "Background slot".into(),
                1,
            )
            .await
            .unwrap();

        assert_eq!(registry.running_count().await, 1);
        assert_eq!(registry.list_running().await.len(), 1);

        let error = registry
            .register_or_resume_background(
                TASK_4_UUID.into(),
                "Explore".into(),
                "Second background slot".into(),
                1,
            )
            .await
            .expect_err("background limit should apply before handle binding");
        assert!(
            error
                .to_string()
                .contains("Maximum background tasks (1) reached")
        );
    }

    #[tokio::test]
    async fn test_fail_task() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_2_UUID.into(), "Explore".into(), "Failing task".into())
            .await
            .unwrap();

        registry
            .fail(TASK_2_UUID, "Something went wrong".into())
            .await
            .unwrap();

        let result = registry.get_result(TASK_2_UUID).await.unwrap();
        assert_eq!(result.status, SessionState::Failed);
        assert_eq!(result.error, Some("Something went wrong".to_string()));
    }

    #[tokio::test]
    async fn test_cancel_task() {
        let registry = test_registry();
        registry
            .register_or_resume(
                TASK_3_UUID.into(),
                "Explore".into(),
                "Cancellable task".into(),
            )
            .await
            .unwrap();

        assert!(registry.cancel(TASK_3_UUID).await.unwrap());
        assert_eq!(
            registry.get_status(TASK_3_UUID).await,
            Some(SessionState::Cancelled)
        );

        assert!(!registry.cancel(TASK_3_UUID).await.unwrap());
    }

    #[tokio::test]
    async fn test_not_found() {
        let registry = test_registry();
        assert!(registry.get_status("nonexistent").await.is_none());
        assert!(registry.get_result("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn test_messages() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_4_UUID.into(), "Explore".into(), "Message test".into())
            .await
            .unwrap();

        let manager = registry.session_manager();
        let session_id = SessionId::parse(TASK_4_UUID).unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::user(vec![ContentBlock::text("Hello")]),
            )
            .await
            .unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::assistant(vec![ContentBlock::text("Hi there!")]),
            )
            .await
            .unwrap();

        let loaded = registry.get_messages(TASK_4_UUID).await.unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[tokio::test]
    async fn test_complete_preserves_existing_session_messages() {
        let registry = test_registry();
        registry
            .register_or_resume(
                TASK_1_UUID.into(),
                "Explore".into(),
                "Completion messages".into(),
            )
            .await
            .unwrap();

        let manager = registry.session_manager();
        let session_id = SessionId::parse(TASK_1_UUID).unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::user(vec![ContentBlock::text("question")]),
            )
            .await
            .unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::assistant(vec![ContentBlock::text("answer")]),
            )
            .await
            .unwrap();

        registry
            .complete(TASK_1_UUID, mock_result(TASK_1_UUID))
            .await
            .unwrap();

        let loaded = registry.get_messages(TASK_1_UUID).await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].role, Role::User);
        assert_eq!(loaded[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn test_invalid_task_ids_are_rejected() {
        let registry = test_registry();
        let error = registry
            .register_or_resume("not-a-uuid".into(), "Explore".into(), "Invalid".into())
            .await
            .expect_err("invalid task ids should fail");

        assert!(
            error
                .to_string()
                .contains("Task IDs must be valid session UUIDs")
        );
        assert!(registry.get_status("not-a-uuid").await.is_none());
    }

    #[tokio::test]
    async fn test_register_or_resume_reuses_existing_session() {
        let registry = test_registry();
        let _cancel_rx = registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Reusable".into())
            .await
            .unwrap();
        registry
            .fail(TASK_1_UUID, "Needs resume".into())
            .await
            .unwrap();

        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Reusable".into())
            .await
            .unwrap();

        assert_eq!(
            registry.get_status(TASK_1_UUID).await,
            Some(SessionState::Active)
        );
    }

    #[tokio::test]
    async fn test_get_result_preserves_structured_output() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Structured".into())
            .await
            .unwrap();

        let manager = registry.session_manager();
        let session_id = SessionId::parse(TASK_1_UUID).unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::assistant(vec![ContentBlock::text("{\"value\":42}")]).metadata(
                    crate::session::MessageMetadata {
                        structured_output: Some(serde_json::json!({"value": 42})),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();

        registry
            .complete(TASK_1_UUID, mock_result(TASK_1_UUID))
            .await
            .unwrap();

        let snapshot = registry.get_result(TASK_1_UUID).await.unwrap();
        assert_eq!(
            snapshot.structured_output,
            Some(serde_json::json!({"value": 42}))
        );
    }

    #[tokio::test]
    async fn test_get_result_preserves_full_assistant_content() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Rich".into())
            .await
            .unwrap();

        let manager = registry.session_manager();
        let session_id = SessionId::parse(TASK_1_UUID).unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::assistant(vec![
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

        let snapshot = registry.get_result(TASK_1_UUID).await.unwrap();
        assert_eq!(snapshot.text.as_deref(), Some("first second"));
        assert_eq!(snapshot.content.as_ref().map(Vec::len), Some(2));
    }

    #[tokio::test]
    async fn test_get_result_includes_response_and_execution_metadata() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Observed".into())
            .await
            .unwrap();

        let manager = registry.session_manager();
        let session_id = SessionId::parse(TASK_1_UUID).unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::assistant(vec![ContentBlock::text("answer")]).metadata(
                    crate::session::MessageMetadata {
                        model: Some("claude-sonnet".to_string()),
                        request_id: Some("req_123".to_string()),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();

        let mut result = mock_result(TASK_1_UUID);
        result.uuid = "result-uuid".to_string();
        result.tool_calls = 3;
        result.iterations = 4;
        result.usage = Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_input_tokens: Some(3),
            cache_creation_input_tokens: Some(1),
            server_tool_use: None,
        };
        result.metrics.execution_time_ms = 250;
        result.metrics.api_calls = 2;
        result.metrics.compactions = 1;
        result.metrics.errors = 0;
        registry.complete(TASK_1_UUID, result).await.unwrap();

        let snapshot = registry.get_result(TASK_1_UUID).await.unwrap();
        assert_eq!(
            snapshot
                .response_metadata
                .as_ref()
                .and_then(|metadata| metadata.model.as_deref()),
            Some("claude-sonnet")
        );
        assert_eq!(
            snapshot
                .response_metadata
                .as_ref()
                .and_then(|metadata| metadata.request_id.as_deref()),
            Some("req_123")
        );
        assert_eq!(
            snapshot
                .execution
                .as_ref()
                .and_then(|execution| execution.result_uuid.as_deref()),
            Some("result-uuid")
        );
        assert_eq!(
            snapshot
                .execution
                .as_ref()
                .and_then(|execution| execution.iterations),
            Some(4)
        );
        assert_eq!(
            snapshot
                .execution
                .as_ref()
                .and_then(|execution| execution.tool_calls),
            Some(3)
        );
        assert_eq!(
            snapshot
                .execution
                .as_ref()
                .and_then(|execution| execution.usage)
                .map(|usage| usage.output_tokens),
            Some(20)
        );
    }

    #[tokio::test]
    async fn test_complete_rejects_mismatched_session_id() {
        let registry = test_registry();
        registry
            .register_or_resume(TASK_1_UUID.into(), "Explore".into(), "Mismatch".into())
            .await
            .unwrap();

        let mut result = mock_result(TASK_1_UUID);
        result.session_id = TASK_2_UUID.to_string();
        registry.complete(TASK_1_UUID, result).await.unwrap();

        let snapshot = registry.get_result(TASK_1_UUID).await.unwrap();
        assert_eq!(snapshot.status, SessionState::Failed);
        assert!(
            snapshot
                .error
                .as_deref()
                .is_some_and(|message| message.contains("mismatched delegated session id"))
        );
    }
}
