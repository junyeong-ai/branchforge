//! Task registry for managing background agent tasks with Session-based persistence.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{RwLock, oneshot};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::session::{
    Persistence, Session, SessionConfig, SessionId, SessionMessage, SessionState, SessionType,
};
use crate::types::{ContentBlock, Message, Role};

use super::AgentResult;

struct TaskRuntime {
    handle: Option<JoinHandle<()>>,
    cancel_tx: Option<oneshot::Sender<()>>,
}

#[derive(Clone)]
pub struct TaskRegistry {
    runtime: Arc<RwLock<HashMap<String, TaskRuntime>>>,
    persistence: Arc<dyn Persistence>,
    parent_session_id: Option<SessionId>,
    default_ttl: Option<Duration>,
}

impl TaskRegistry {
    pub fn new(persistence: Arc<dyn Persistence>) -> Self {
        Self {
            runtime: Arc::new(RwLock::new(HashMap::new())),
            persistence,
            parent_session_id: None,
            default_ttl: Some(Duration::from_secs(3600)),
        }
    }

    pub fn parent_session(mut self, parent_id: SessionId) -> Self {
        self.parent_session_id = Some(parent_id);
        self
    }

    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    pub async fn register(
        &self,
        id: String,
        agent_type: String,
        description: String,
    ) -> oneshot::Receiver<()> {
        let (cancel_tx, cancel_rx) = oneshot::channel();

        let config = SessionConfig {
            ttl_secs: self.default_ttl.map(|d| d.as_secs()),
            ..Default::default()
        };

        let session = match self.parent_session_id {
            Some(parent_id) => Session::new_subagent(parent_id, &agent_type, &description, config),
            None => {
                let mut s = Session::new(config);
                s.session_type = SessionType::Subagent {
                    agent_type,
                    description,
                };
                s
            }
        };

        let session_id = SessionId::from(id.as_str());
        let mut session = session;
        session.id = session_id;
        session.state = SessionState::Active;
        session.principal_id = Some(id.clone());

        if let Err(e) = self.persistence.save(&session).await {
            warn!(session_id = %session_id, error = %e, "Failed to save task session on register");
        }

        let mut runtime = self.runtime.write().await;
        runtime.insert(
            id,
            TaskRuntime {
                handle: None,
                cancel_tx: Some(cancel_tx),
            },
        );

        cancel_rx
    }

    pub async fn set_handle(&self, id: &str, handle: JoinHandle<()>) {
        let mut runtime = self.runtime.write().await;
        if let Some(rt) = runtime.get_mut(id) {
            rt.handle = Some(handle);
        }
    }

    pub async fn complete(&self, id: &str, result: AgentResult) {
        let session_id = SessionId::from(id);

        match self.persistence.load(&session_id).await {
            Ok(Some(mut session)) => {
                session.state = SessionState::Completed;

                for msg in &result.messages {
                    let content: Vec<ContentBlock> = msg.content.clone();
                    let session_msg = match msg.role {
                        Role::User => SessionMessage::user(content),
                        Role::Assistant => SessionMessage::assistant(content),
                    };
                    session.add_message(session_msg);
                }

                if let Err(e) = self.persistence.save(&session).await {
                    warn!(session_id = %session_id, error = %e, "Failed to save completed task session");
                }
            }
            Ok(None) => {
                warn!(session_id = %session_id, "Task session not found on complete");
            }
            Err(e) => {
                warn!(session_id = %session_id, error = %e, "Failed to load task session on complete");
            }
        }

        let mut runtime = self.runtime.write().await;
        runtime.remove(id);
    }

    pub async fn fail(&self, id: &str, error: String) {
        let session_id = SessionId::from(id);

        match self.persistence.load(&session_id).await {
            Ok(Some(mut session)) => {
                session.state = SessionState::Failed;
                session.error = Some(error);
                if let Err(e) = self.persistence.save(&session).await {
                    warn!(session_id = %session_id, error = %e, "Failed to save failed task session");
                }
            }
            Ok(None) => {
                warn!(session_id = %session_id, "Task session not found on fail");
            }
            Err(e) => {
                warn!(session_id = %session_id, error = %e, "Failed to load task session on fail");
            }
        }

        let mut runtime = self.runtime.write().await;
        runtime.remove(id);
    }

    pub async fn cancel(&self, id: &str) -> bool {
        let session_id = SessionId::from(id);

        let cancelled = {
            let mut runtime = self.runtime.write().await;
            if let Some(rt) = runtime.get_mut(id) {
                if let Some(tx) = rt.cancel_tx.take() {
                    let _ = tx.send(());
                }
                if let Some(handle) = rt.handle.take() {
                    handle.abort();
                }
                runtime.remove(id);
                true
            } else {
                false
            }
        };

        if cancelled {
            match self.persistence.load(&session_id).await {
                Ok(Some(mut session)) => {
                    session.state = SessionState::Cancelled;
                    if let Err(e) = self.persistence.save(&session).await {
                        warn!(session_id = %session_id, error = %e, "Failed to save cancelled task session");
                    }
                }
                Ok(None) => {
                    warn!(session_id = %session_id, "Task session not found on cancel");
                }
                Err(e) => {
                    warn!(session_id = %session_id, error = %e, "Failed to load task session on cancel");
                }
            }
        }

        cancelled
    }

    pub async fn get_status(&self, id: &str) -> Option<SessionState> {
        let session_id = SessionId::from(id);
        self.persistence
            .load(&session_id)
            .await
            .ok()
            .flatten()
            .map(|s| s.state)
    }

    pub async fn get_result(
        &self,
        id: &str,
    ) -> Option<(SessionState, Option<String>, Option<String>)> {
        let session_id = SessionId::from(id);
        self.persistence
            .load(&session_id)
            .await
            .ok()
            .flatten()
            .map(|s| {
                let text = s.messages.last().and_then(|m| {
                    m.content.iter().find_map(|c| match c {
                        ContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                });
                (s.state, text, s.error)
            })
    }

    pub async fn wait_for_completion(
        &self,
        id: &str,
        timeout: Duration,
    ) -> Option<(SessionState, Option<String>, Option<String>)> {
        let deadline = std::time::Instant::now() + timeout;
        let poll_interval = Duration::from_millis(100);

        loop {
            if let Some((state, output, error)) = self.get_result(id).await {
                if state != SessionState::Active && state != SessionState::WaitingForTools {
                    return Some((state, output, error));
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
        let runtime = self.runtime.read().await;
        let mut result = Vec::new();

        for id in runtime.keys() {
            let session_id = SessionId::from(id.as_str());
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

    pub async fn cleanup_completed(&self) -> usize {
        self.persistence.cleanup_expired().await.unwrap_or(0)
    }

    pub async fn running_count(&self) -> usize {
        self.runtime.read().await.len()
    }

    pub async fn save_messages(&self, id: &str, messages: Vec<Message>) {
        let session_id = SessionId::from(id);

        match self.persistence.load(&session_id).await {
            Ok(Some(mut session)) => {
                for msg in messages {
                    let content: Vec<ContentBlock> = msg.content;
                    let session_msg = match msg.role {
                        Role::User => SessionMessage::user(content),
                        Role::Assistant => SessionMessage::assistant(content),
                    };
                    session.add_message(session_msg);
                }
                if let Err(e) = self.persistence.save(&session).await {
                    warn!(session_id = %session_id, error = %e, "Failed to save task messages");
                }
            }
            Ok(None) => {
                warn!(session_id = %session_id, "Task session not found when saving messages");
            }
            Err(e) => {
                warn!(session_id = %session_id, error = %e, "Failed to load task session when saving messages");
            }
        }
    }

    pub async fn get_messages(&self, id: &str) -> Option<Vec<Message>> {
        let session_id = SessionId::from(id);
        self.persistence
            .load(&session_id)
            .await
            .ok()
            .flatten()
            .map(|s| s.to_api_messages())
    }

    pub async fn get_session(&self, id: &str) -> Option<Session> {
        let session_id = SessionId::from(id);
        self.persistence.load(&session_id).await.ok().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentState;
    use crate::session::MemoryPersistence;
    use crate::types::{StopReason, Usage};

    fn test_registry() -> TaskRegistry {
        TaskRegistry::new(Arc::new(MemoryPersistence::new()))
    }

    // Use valid UUIDs for tests to ensure consistent session IDs
    const TASK_1_UUID: &str = "00000000-0000-0000-0000-000000000001";
    const TASK_2_UUID: &str = "00000000-0000-0000-0000-000000000002";
    const TASK_3_UUID: &str = "00000000-0000-0000-0000-000000000003";
    const TASK_4_UUID: &str = "00000000-0000-0000-0000-000000000004";

    fn mock_result() -> AgentResult {
        AgentResult {
            text: "Test result".to_string(),
            usage: Usage::default(),
            tool_calls: 0,
            iterations: 1,
            stop_reason: StopReason::EndTurn,
            state: AgentState::Completed,
            metrics: Default::default(),
            session_id: "test-session".to_string(),
            structured_output: None,
            messages: Vec::new(),
            uuid: "test-uuid".to_string(),
        }
    }

    #[tokio::test]
    async fn test_register_and_complete() {
        let registry = test_registry();
        let _cancel_rx = registry
            .register(TASK_1_UUID.into(), "Explore".into(), "Test task".into())
            .await;

        assert_eq!(
            registry.get_status(TASK_1_UUID).await,
            Some(SessionState::Active)
        );

        registry.complete(TASK_1_UUID, mock_result()).await;

        let (status, _, _) = registry.get_result(TASK_1_UUID).await.unwrap();
        assert_eq!(status, SessionState::Completed);
    }

    #[tokio::test]
    async fn test_fail_task() {
        let registry = test_registry();
        registry
            .register(TASK_2_UUID.into(), "Explore".into(), "Failing task".into())
            .await;

        registry
            .fail(TASK_2_UUID, "Something went wrong".into())
            .await;

        let (status, _, error) = registry.get_result(TASK_2_UUID).await.unwrap();
        assert_eq!(status, SessionState::Failed);
        assert_eq!(error, Some("Something went wrong".to_string()));
    }

    #[tokio::test]
    async fn test_cancel_task() {
        let registry = test_registry();
        registry
            .register(
                TASK_3_UUID.into(),
                "Explore".into(),
                "Cancellable task".into(),
            )
            .await;

        assert!(registry.cancel(TASK_3_UUID).await);
        assert_eq!(
            registry.get_status(TASK_3_UUID).await,
            Some(SessionState::Cancelled)
        );

        assert!(!registry.cancel(TASK_3_UUID).await);
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
            .register(TASK_4_UUID.into(), "Explore".into(), "Message test".into())
            .await;

        let messages = vec![
            Message::user("Hello"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::text("Hi there!")],
            },
        ];

        registry.save_messages(TASK_4_UUID, messages).await;

        let loaded = registry.get_messages(TASK_4_UUID).await.unwrap();
        assert_eq!(loaded.len(), 2);
    }
}
