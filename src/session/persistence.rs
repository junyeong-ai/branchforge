//! Session Persistence Backends

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use super::state::{Session, SessionId, SessionMessage};
use super::types::{QueueItem, SummarySnapshot};
use super::{SessionError, SessionResult};
use crate::graph::{GraphEvent, GraphMaterializer, SessionGraph};

#[async_trait::async_trait]
pub trait Persistence: Send + Sync {
    fn name(&self) -> &str;

    // Core CRUD
    async fn save(&self, session: &Session) -> SessionResult<()>;
    async fn load(&self, id: &SessionId) -> SessionResult<Option<Session>>;
    async fn delete(&self, id: &SessionId) -> SessionResult<bool>;
    async fn list(&self, tenant_id: Option<&str>) -> SessionResult<Vec<SessionId>>;

    // Summaries
    async fn add_summary(&self, snapshot: SummarySnapshot) -> SessionResult<()>;
    async fn get_summaries(&self, session_id: &SessionId) -> SessionResult<Vec<SummarySnapshot>>;

    // Queue
    async fn enqueue(
        &self,
        session_id: &SessionId,
        content: String,
        priority: i32,
    ) -> SessionResult<QueueItem>;
    async fn dequeue(&self, session_id: &SessionId) -> SessionResult<Option<QueueItem>>;
    async fn cancel_queued(&self, item_id: Uuid) -> SessionResult<bool>;
    async fn pending_queue(&self, session_id: &SessionId) -> SessionResult<Vec<QueueItem>>;

    // Cleanup
    async fn cleanup_expired(&self) -> SessionResult<usize>;

    async fn append_graph_event(
        &self,
        session_id: &SessionId,
        event: GraphEvent,
    ) -> SessionResult<()> {
        let mut session = self
            .load(session_id)
            .await?
            .ok_or_else(|| SessionError::NotFound {
                id: session_id.to_string(),
            })?;
        let graph_id = session.graph.id;
        let created_at = session.graph.created_at;
        let primary_branch = session.graph.primary_branch;
        session.graph.events.push(event);
        session.graph = GraphMaterializer::from_events(&session.graph.events);
        session.graph.id = graph_id;
        session.graph.created_at = created_at;
        if session.graph.branches.contains_key(&primary_branch) {
            session.graph.primary_branch = primary_branch;
        }
        self.save(&session).await
    }

    async fn load_graph(&self, session_id: &SessionId) -> SessionResult<Option<SessionGraph>> {
        Ok(self
            .load(session_id)
            .await?
            .map(|session| session.to_graph()))
    }

    /// Append a message to an existing session.
    ///
    /// Concurrency contract: implementations may hold a write lock for the duration
    /// of this call. Callers must not hold other persistence locks to avoid deadlocks.
    /// The default implementation performs a load-modify-save cycle; backends should
    /// override this with a more efficient single-lock approach when possible.
    async fn add_message(
        &self,
        session_id: &SessionId,
        message: SessionMessage,
    ) -> SessionResult<()> {
        let mut session = self
            .load(session_id)
            .await?
            .ok_or_else(|| SessionError::NotFound {
                id: session_id.to_string(),
            })?;
        session.add_message(message);
        self.save(&session).await
    }
}

#[derive(Debug, Default)]
pub struct MemoryPersistence {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    summaries: Arc<RwLock<HashMap<String, Vec<SummarySnapshot>>>>,
    queue: Arc<RwLock<HashMap<String, Vec<QueueItem>>>>,
}

impl MemoryPersistence {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn count(&self) -> usize {
        self.sessions.read().await.len()
    }

    pub async fn clear(&self) {
        self.sessions.write().await.clear();
        self.summaries.write().await.clear();
        self.queue.write().await.clear();
    }

    pub async fn load_graph(&self, session_id: &SessionId) -> SessionResult<Option<SessionGraph>> {
        <Self as Persistence>::load_graph(self, session_id).await
    }
}

#[async_trait::async_trait]
impl Persistence for MemoryPersistence {
    fn name(&self) -> &str {
        "memory"
    }

    async fn save(&self, session: &Session) -> SessionResult<()> {
        self.sessions
            .write()
            .await
            .insert(session.id.to_string(), session.clone());
        Ok(())
    }

    async fn add_message(
        &self,
        session_id: &SessionId,
        message: SessionMessage,
    ) -> SessionResult<()> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(&session_id.to_string()) {
            session.add_message(message);
            Ok(())
        } else {
            Err(SessionError::NotFound {
                id: session_id.to_string(),
            })
        }
    }

    async fn load(&self, id: &SessionId) -> SessionResult<Option<Session>> {
        Ok(self.sessions.read().await.get(&id.to_string()).cloned())
    }

    async fn delete(&self, id: &SessionId) -> SessionResult<bool> {
        let key = id.to_string();
        let mut sessions = self.sessions.write().await;
        let mut summaries = self.summaries.write().await;
        let mut queue = self.queue.write().await;
        summaries.remove(&key);
        queue.remove(&key);
        Ok(sessions.remove(&key).is_some())
    }

    async fn list(&self, tenant_id: Option<&str>) -> SessionResult<Vec<SessionId>> {
        Ok(self
            .sessions
            .read()
            .await
            .values()
            .filter(|s| {
                tenant_id
                    .map(|t| s.tenant_id.as_deref() == Some(t))
                    .unwrap_or(true)
            })
            .map(|s| s.id)
            .collect())
    }

    async fn add_summary(&self, snapshot: SummarySnapshot) -> SessionResult<()> {
        self.summaries
            .write()
            .await
            .entry(snapshot.session_id.to_string())
            .or_default()
            .push(snapshot);
        Ok(())
    }

    async fn get_summaries(&self, session_id: &SessionId) -> SessionResult<Vec<SummarySnapshot>> {
        Ok(self
            .summaries
            .read()
            .await
            .get(&session_id.to_string())
            .cloned()
            .unwrap_or_default())
    }

    async fn enqueue(
        &self,
        session_id: &SessionId,
        content: String,
        priority: i32,
    ) -> SessionResult<QueueItem> {
        let item = QueueItem::enqueue(*session_id, content).priority(priority);
        self.queue
            .write()
            .await
            .entry(session_id.to_string())
            .or_default()
            .push(item.clone());
        Ok(item)
    }

    async fn dequeue(&self, session_id: &SessionId) -> SessionResult<Option<QueueItem>> {
        let mut queue = self.queue.write().await;
        if let Some(items) = queue.get_mut(&session_id.to_string()) {
            items.sort_by(|a, b| b.priority.cmp(&a.priority));
            if let Some(pos) = items
                .iter()
                .position(|i| i.status == super::types::QueueStatus::Pending)
            {
                items[pos].start_processing();
                return Ok(Some(items[pos].clone()));
            }
        }
        Ok(None)
    }

    async fn cancel_queued(&self, item_id: Uuid) -> SessionResult<bool> {
        for items in self.queue.write().await.values_mut() {
            if let Some(item) = items.iter_mut().find(|i| i.id == item_id) {
                item.cancel();
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn pending_queue(&self, session_id: &SessionId) -> SessionResult<Vec<QueueItem>> {
        Ok(self
            .queue
            .read()
            .await
            .get(&session_id.to_string())
            .map(|items| {
                items
                    .iter()
                    .filter(|i| i.status == super::types::QueueStatus::Pending)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn cleanup_expired(&self) -> SessionResult<usize> {
        // Hold all three write locks simultaneously to prevent races where a
        // concurrent operation could observe a session removed from `sessions`
        // but still present in `summaries` or `queue`.
        let mut sessions = self.sessions.write().await;
        let mut summaries = self.summaries.write().await;
        let mut queue = self.queue.write().await;

        let expired_keys: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| s.is_expired())
            .map(|(k, _)| k.clone())
            .collect();

        for key in &expired_keys {
            sessions.remove(key);
            summaries.remove(key);
            queue.remove(key);
        }

        Ok(expired_keys.len())
    }
}

pub struct PersistenceFactory;

impl PersistenceFactory {
    pub fn memory() -> Arc<dyn Persistence> {
        Arc::new(MemoryPersistence::new())
    }

    /// Create a JSONL persistence backend (requires `jsonl` feature).
    #[cfg(feature = "jsonl")]
    pub async fn jsonl(
        config: super::persistence_jsonl::JsonlConfig,
    ) -> SessionResult<Arc<dyn Persistence>> {
        Ok(Arc::new(
            super::persistence_jsonl::JsonlPersistence::new(config).await?,
        ))
    }

    /// Create a JSONL persistence backend with default configuration (requires `jsonl` feature).
    #[cfg(feature = "jsonl")]
    pub async fn jsonl_default() -> SessionResult<Arc<dyn Persistence>> {
        Ok(Arc::new(
            super::persistence_jsonl::JsonlPersistence::default_config().await?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphEvent, GraphEventBody, NodeKind};
    use crate::session::state::SessionConfig;
    use crate::types::ContentBlock;

    #[tokio::test]
    async fn test_save_load() {
        let persistence = MemoryPersistence::new();
        let session = Session::new(SessionConfig::default());
        let id = session.id;

        persistence.save(&session).await.unwrap();
        let loaded = persistence.load(&id).await.unwrap();

        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().id, id);
    }

    #[tokio::test]
    async fn test_delete() {
        let persistence = MemoryPersistence::new();
        let session = Session::new(SessionConfig::default());
        let id = session.id;

        persistence.save(&session).await.unwrap();
        assert!(persistence.delete(&id).await.unwrap());
        assert!(persistence.load(&id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_list_by_tenant() {
        let persistence = MemoryPersistence::new();

        let mut s1 = Session::new(SessionConfig::default());
        s1.tenant_id = Some("tenant-a".to_string());
        s1.principal_id = Some("user-1".to_string());

        let mut s2 = Session::new(SessionConfig::default());
        s2.tenant_id = Some("tenant-b".to_string());
        s2.principal_id = Some("user-2".to_string());

        persistence.save(&s1).await.unwrap();
        persistence.save(&s2).await.unwrap();

        assert_eq!(persistence.list(None).await.unwrap().len(), 2);
        assert_eq!(persistence.list(Some("tenant-a")).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_add_message() {
        let persistence = MemoryPersistence::new();
        let session = Session::new(SessionConfig::default());
        let id = session.id;

        persistence.save(&session).await.unwrap();
        persistence
            .add_message(&id, SessionMessage::user(vec![ContentBlock::text("Hello")]))
            .await
            .unwrap();

        let loaded = persistence.load(&id).await.unwrap().unwrap();
        assert_eq!(loaded.current_branch_messages().len(), 1);
    }

    #[tokio::test]
    async fn test_summaries() {
        let persistence = MemoryPersistence::new();
        let session = Session::new(SessionConfig::default());
        let id = session.id;

        persistence.save(&session).await.unwrap();
        persistence
            .add_summary(SummarySnapshot::new(id, "First"))
            .await
            .unwrap();
        persistence
            .add_summary(SummarySnapshot::new(id, "Second"))
            .await
            .unwrap();

        let summaries = persistence.get_summaries(&id).await.unwrap();
        assert_eq!(summaries.len(), 2);
    }

    #[tokio::test]
    async fn test_queue_priority() {
        let persistence = MemoryPersistence::new();
        let session = Session::new(SessionConfig::default());
        let id = session.id;

        persistence.save(&session).await.unwrap();
        persistence
            .enqueue(&id, "Low".to_string(), 1)
            .await
            .unwrap();
        persistence
            .enqueue(&id, "High".to_string(), 10)
            .await
            .unwrap();

        let next = persistence.dequeue(&id).await.unwrap().unwrap();
        assert_eq!(next.content, "High");
    }

    #[tokio::test]
    async fn test_cleanup_expired() {
        let persistence = MemoryPersistence::new();
        let config = SessionConfig {
            ttl_secs: Some(0),
            ..Default::default()
        };
        let session = Session::new(config);

        persistence.save(&session).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        assert_eq!(persistence.cleanup_expired().await.unwrap(), 1);
        assert_eq!(persistence.count().await, 0);
    }

    #[tokio::test]
    async fn test_append_graph_event() {
        let persistence = MemoryPersistence::new();
        let session = Session::new(SessionConfig::default());
        let id = session.id;

        persistence.save(&session).await.unwrap();
        persistence
            .append_graph_event(
                &id,
                GraphEvent::new(GraphEventBody::NodeAppended {
                    node_id: uuid::Uuid::new_v4(),
                    branch_id: session.graph.primary_branch,
                    parent_id: None,
                    kind: NodeKind::User,
                    tags: vec!["test".to_string()],
                    payload: serde_json::json!({"text": "hello"}),
                }),
            )
            .await
            .unwrap();

        let graph = persistence.load_graph(&id).await.unwrap().unwrap();
        assert_eq!(graph.events.len(), 1);
        assert_eq!(graph.branch_nodes(graph.primary_branch).len(), 1);
    }

    #[tokio::test]
    async fn test_graph_first_roundtrip_restores_projection() {
        let persistence = MemoryPersistence::new();
        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]));
        session.clear_messages();

        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.current_branch_messages().len(), 1);
    }

    #[tokio::test]
    async fn test_graph_first_roundtrip_preserves_bookmarks_and_checkpoints() {
        let persistence = MemoryPersistence::new();
        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]));
        session.graph.create_checkpoint(
            session.graph.primary_branch,
            "milestone",
            Some("saved".to_string()),
            vec!["tag".to_string()],
        );
        session.bookmark_current_head("head", Some("note".to_string()));
        session.clear_messages();

        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.current_branch_messages().len(), 1);
        assert_eq!(loaded.graph.checkpoints.len(), 1);
        assert_eq!(loaded.graph.bookmarks.len(), 1);
    }
}
