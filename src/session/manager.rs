//! Session lifecycle management.

use std::sync::Arc;

use super::persistence::{MemoryPersistence, Persistence};
use super::state::{Session, SessionConfig, SessionId, SessionMessage, SessionState};
use super::{SessionError, SessionResult};

#[derive(Clone)]
pub struct SessionManager {
    persistence: Arc<dyn Persistence>,
}

impl SessionManager {
    pub fn new(persistence: Arc<dyn Persistence>) -> Self {
        Self { persistence }
    }

    pub fn in_memory() -> Self {
        Self::new(Arc::new(MemoryPersistence::new()))
    }

    pub async fn create(&self, config: SessionConfig) -> SessionResult<Session> {
        let session = Session::new(config);
        self.persistence.save(&session).await?;
        Ok(session)
    }

    pub async fn create_with_tenant(
        &self,
        config: SessionConfig,
        tenant_id: impl Into<String>,
    ) -> SessionResult<Session> {
        let mut session = Session::new(config);
        session.tenant_id = Some(tenant_id.into());
        self.persistence.save(&session).await?;
        Ok(session)
    }

    pub async fn get(&self, id: &SessionId) -> SessionResult<Session> {
        let session = self
            .persistence
            .load(id)
            .await?
            .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;

        if session.is_expired() {
            self.persistence.delete(id).await?;
            return Err(SessionError::Expired { id: id.to_string() });
        }

        Ok(session)
    }

    pub async fn get_by_str(&self, id: &str) -> SessionResult<Session> {
        self.get(&SessionId::from(id)).await
    }

    pub async fn update(&self, session: &Session) -> SessionResult<()> {
        self.persistence.save(session).await
    }

    pub async fn add_message(
        &self,
        session_id: &SessionId,
        message: SessionMessage,
    ) -> SessionResult<()> {
        self.persistence.add_message(session_id, message).await
    }

    pub async fn delete(&self, id: &SessionId) -> SessionResult<bool> {
        self.persistence.delete(id).await
    }

    pub async fn list(&self) -> SessionResult<Vec<SessionId>> {
        self.persistence.list(None).await
    }

    pub async fn list_for_tenant(&self, tenant_id: &str) -> SessionResult<Vec<SessionId>> {
        self.persistence.list(Some(tenant_id)).await
    }

    pub async fn fork(&self, id: &SessionId) -> SessionResult<Session> {
        let original = self.get(id).await?;

        let mut forked = Session::new(original.config.clone());
        forked.parent_id = Some(original.id);
        forked.tenant_id = original.tenant_id.clone();
        forked.summary = original.summary.clone();

        // Copy messages up to current leaf
        for msg in original.current_branch_messages() {
            let mut cloned = msg;
            cloned.is_sidechain = true;
            forked.add_message(cloned);
        }

        self.persistence.save(&forked).await?;
        Ok(forked)
    }

    pub async fn fork_from_node(
        &self,
        id: &SessionId,
        from_node: crate::graph::NodeId,
    ) -> SessionResult<Session> {
        let original = self.get(id).await?;
        let replay = original.replay_input(Some(from_node));

        let mut forked = Session::new(original.config.clone());
        forked.parent_id = Some(original.id);
        forked.tenant_id = original.tenant_id.clone();
        forked.summary = original.summary.clone();

        for message in replay.messages {
            let mut session_message = match message.role {
                crate::types::Role::User => SessionMessage::user(message.content),
                crate::types::Role::Assistant => SessionMessage::assistant(message.content),
            };
            session_message.is_sidechain = true;
            forked.add_message(session_message);
        }

        self.persistence.save(&forked).await?;
        Ok(forked)
    }

    pub async fn export_branch(
        &self,
        id: &SessionId,
    ) -> SessionResult<Option<crate::graph::BranchExport>> {
        let session = self.get(id).await?;
        Ok(session.export_current_branch())
    }

    pub async fn replay_input(
        &self,
        id: &SessionId,
        from_node: Option<crate::graph::NodeId>,
    ) -> SessionResult<Option<crate::graph::ReplayInput>> {
        let session = self.get(id).await?;
        Ok(Some(
            session
                .graph
                .replay_input(session.graph.primary_branch, from_node),
        ))
    }

    pub async fn resume_prompt_with_replay(
        &self,
        id: &SessionId,
        from_node: Option<crate::graph::NodeId>,
        prompt: &str,
    ) -> SessionResult<Option<(crate::graph::ReplayInput, String)>> {
        let replay = self.replay_input(id, from_node).await?;
        Ok(replay.map(|replay| (replay, prompt.to_string())))
    }

    pub async fn export_branch_json(&self, id: &SessionId) -> SessionResult<Option<String>> {
        let export = self.export_branch(id).await?;
        export
            .as_ref()
            .map(crate::session::branch_export_to_json)
            .transpose()
            .map_err(|e| SessionError::Storage {
                message: e.to_string(),
            })
    }

    pub async fn export_branch_html(&self, id: &SessionId) -> SessionResult<Option<String>> {
        let export = self.export_branch(id).await?;
        Ok(export.as_ref().map(crate::session::branch_export_to_html))
    }

    pub async fn bookmark_current_head(
        &self,
        id: &SessionId,
        label: impl Into<String>,
        note: Option<String>,
    ) -> SessionResult<Option<uuid::Uuid>> {
        let mut session = self.get(id).await?;
        let bookmark = session.bookmark_current_head(label, note);
        self.persistence.save(&session).await?;
        Ok(bookmark)
    }

    pub async fn complete(&self, id: &SessionId) -> SessionResult<()> {
        let mut session = self.get(id).await?;
        session.set_state(SessionState::Completed);
        self.persistence.save(&session).await
    }

    pub async fn set_error(&self, id: &SessionId) -> SessionResult<()> {
        let mut session = self.get(id).await?;
        session.set_state(SessionState::Failed);
        self.persistence.save(&session).await
    }

    pub async fn cleanup_expired(&self) -> SessionResult<usize> {
        self.persistence.cleanup_expired().await
    }

    pub async fn exists(&self, id: &SessionId) -> SessionResult<bool> {
        match self.persistence.load(id).await? {
            Some(session) => Ok(!session.is_expired()),
            None => Ok(false),
        }
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::in_memory()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ContentBlock;

    #[tokio::test]
    async fn test_session_manager_create() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();

        assert_eq!(session.state, SessionState::Created);
        assert!(session.current_branch_messages().is_empty());
    }

    #[tokio::test]
    async fn test_session_manager_get() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        let session_id = session.id;

        let restored = manager.get(&session_id).await.unwrap();
        assert_eq!(restored.id, session_id);
    }

    #[tokio::test]
    async fn test_session_manager_not_found() {
        let manager = SessionManager::in_memory();
        let fake_id = SessionId::new();

        let result = manager.get(&fake_id).await;
        assert!(matches!(result, Err(SessionError::NotFound { .. })));
    }

    #[tokio::test]
    async fn test_session_manager_add_message() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        let session_id = session.id;

        let message = SessionMessage::user(vec![ContentBlock::text("Hello")]);
        manager.add_message(&session_id, message).await.unwrap();

        let restored = manager.get(&session_id).await.unwrap();
        assert_eq!(restored.current_branch_messages().len(), 1);
    }

    #[tokio::test]
    async fn test_session_manager_fork() {
        let manager = SessionManager::in_memory();

        // Create original session with messages
        let session = manager.create(SessionConfig::default()).await.unwrap();
        let session_id = session.id;

        let msg1 = SessionMessage::user(vec![ContentBlock::text("Hello")]);
        manager.add_message(&session_id, msg1).await.unwrap();

        let msg2 = SessionMessage::assistant(vec![ContentBlock::text("Hi!")]);
        manager.add_message(&session_id, msg2).await.unwrap();

        // Fork
        let forked = manager.fork(&session_id).await.unwrap();

        // Forked session should have the same messages
        let forked_messages = forked.current_branch_messages();
        assert_eq!(forked_messages.len(), 2);
        assert_ne!(forked.id, session_id);
        assert_eq!(forked.parent_id, Some(session_id));

        assert!(forked_messages.iter().all(|m| m.is_sidechain));
    }

    #[tokio::test]
    async fn test_session_manager_export_and_replay() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        let session_id = session.id;

        manager
            .add_message(
                &session_id,
                SessionMessage::user(vec![ContentBlock::text("hello")]),
            )
            .await
            .unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::assistant(vec![ContentBlock::text("world")]),
            )
            .await
            .unwrap();

        let export = manager
            .export_branch_json(&session_id)
            .await
            .unwrap()
            .unwrap();
        let replay = manager
            .replay_input(&session_id, None)
            .await
            .unwrap()
            .unwrap();

        assert!(export.contains("hello"));
        assert_eq!(replay.messages.len(), 2);
    }

    #[tokio::test]
    async fn test_session_manager_replay_survives_projection_refresh() {
        let manager = SessionManager::in_memory();
        let mut session = manager.create(SessionConfig::default()).await.unwrap();
        session.add_message(SessionMessage::user(vec![ContentBlock::text("hello")]));
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text("world")]));
        session.clear_messages();
        manager.persistence.save(&session).await.unwrap();

        let replay = manager
            .replay_input(&session.id, None)
            .await
            .unwrap()
            .unwrap();
        let export = manager
            .export_branch_json(&session.id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(replay.messages.len(), 2);
        assert!(export.contains("hello"));
    }

    #[tokio::test]
    async fn test_session_manager_export_html() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        let session_id = session.id;

        manager
            .add_message(
                &session_id,
                SessionMessage::user(vec![ContentBlock::text("hello")]),
            )
            .await
            .unwrap();

        let html = manager
            .export_branch_html(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert!(html.contains("Branch:"));
        assert!(html.contains("Timeline"));
    }

    #[tokio::test]
    async fn test_session_manager_bookmark_and_fork_from_node() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        let session_id = session.id;

        manager
            .add_message(
                &session_id,
                SessionMessage::user(vec![ContentBlock::text("one")]),
            )
            .await
            .unwrap();
        manager
            .add_message(
                &session_id,
                SessionMessage::assistant(vec![ContentBlock::text("two")]),
            )
            .await
            .unwrap();

        let loaded = manager.get(&session_id).await.unwrap();
        let from_node = loaded
            .graph
            .branch_head(loaded.graph.primary_branch)
            .unwrap();
        let bookmark = manager
            .bookmark_current_head(&session_id, "checkpoint", Some("saved".to_string()))
            .await
            .unwrap();
        let forked = manager
            .fork_from_node(&session_id, from_node)
            .await
            .unwrap();

        assert!(bookmark.is_some());
        let forked_messages = forked.current_branch_messages();
        assert_eq!(forked_messages.len(), 1);
        assert!(forked_messages[0].is_sidechain);
    }

    #[tokio::test]
    async fn test_session_manager_export_html_includes_bookmark() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        let session_id = session.id;

        manager
            .add_message(
                &session_id,
                SessionMessage::user(vec![ContentBlock::text("hello")]),
            )
            .await
            .unwrap();
        manager
            .bookmark_current_head(&session_id, "mark", Some("note".to_string()))
            .await
            .unwrap();

        let html = manager
            .export_branch_html(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert!(html.contains("Bookmarks"));
        assert!(html.contains("mark"));
    }

    #[tokio::test]
    async fn test_session_manager_complete() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        let session_id = session.id;

        manager.complete(&session_id).await.unwrap();

        let completed = manager.get(&session_id).await.unwrap();
        assert_eq!(completed.state, SessionState::Completed);
    }

    #[tokio::test]
    async fn test_session_manager_tenant_filtering() {
        let manager = SessionManager::in_memory();

        let _s1 = manager
            .create_with_tenant(SessionConfig::default(), "tenant-a")
            .await
            .unwrap();
        let _s2 = manager
            .create_with_tenant(SessionConfig::default(), "tenant-a")
            .await
            .unwrap();
        let _s3 = manager
            .create_with_tenant(SessionConfig::default(), "tenant-b")
            .await
            .unwrap();

        let all = manager.list().await.unwrap();
        assert_eq!(all.len(), 3);

        let tenant_a = manager.list_for_tenant("tenant-a").await.unwrap();
        assert_eq!(tenant_a.len(), 2);

        let tenant_b = manager.list_for_tenant("tenant-b").await.unwrap();
        assert_eq!(tenant_b.len(), 1);
    }

    #[tokio::test]
    async fn test_session_manager_expired() {
        let manager = SessionManager::in_memory();

        let config = SessionConfig {
            ttl_secs: Some(0), // Expire immediately
            ..Default::default()
        };
        let session = manager.create(config).await.unwrap();
        let session_id = session.id;

        // Wait for expiry
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let result = manager.get(&session_id).await;
        assert!(matches!(result, Err(SessionError::Expired { .. })));
    }
}
