use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::persistence::Persistence;
use super::state::{Session, SessionId};
use super::types::{QueueItem, SummarySnapshot};
use super::{SessionError, SessionResult};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SurrealConfig {
    pub namespace: String,
    pub database: String,
    pub endpoint: String,
}

impl Default for SurrealConfig {
    fn default() -> Self {
        Self {
            namespace: "claude_agent".to_string(),
            database: "session_graph".to_string(),
            endpoint: "mem://graph-first-prototype".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SurrealGraphRecord {
    pub id: String,
    pub session_id: SessionId,
    pub tenant_id: Option<String>,
    pub principal_id: Option<String>,
    pub branch_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub tags: Vec<String>,
}

pub struct SurrealPersistence {
    config: SurrealConfig,
}

impl SurrealPersistence {
    pub fn new(config: SurrealConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &SurrealConfig {
        &self.config
    }

    pub fn export_graph_records(&self, session: &Session) -> Vec<SurrealGraphRecord> {
        let mut records = Vec::new();

        records.push(SurrealGraphRecord {
            id: format!("session:{}", session.id),
            session_id: session.id,
            tenant_id: session.tenant_id.clone(),
            principal_id: session.principal_id.clone(),
            branch_id: None,
            parent_id: None,
            kind: "session".to_string(),
            payload: serde_json::json!({
                "graph_id": session.graph.id,
                "primary_branch": session.graph.primary_branch,
                "tenant_id": session.tenant_id,
                "principal_id": session.principal_id,
                "state": session.state,
            }),
            created_at: session.created_at,
            tags: Vec::new(),
        });

        records.extend(
            session
                .graph
                .branches
                .values()
                .map(|branch| SurrealGraphRecord {
                    id: format!("branch:{}", branch.id),
                    session_id: session.id,
                    tenant_id: session.tenant_id.clone(),
                    principal_id: session.principal_id.clone(),
                    branch_id: Some(branch.id),
                    parent_id: branch.forked_from,
                    kind: "branch".to_string(),
                    payload: serde_json::json!({
                        "name": branch.name,
                        "head": branch.head,
                    }),
                    created_at: branch.created_at,
                    tags: Vec::new(),
                }),
        );

        records.extend(session.graph.nodes.values().map(|node| SurrealGraphRecord {
            id: format!("node:{}", node.id),
            session_id: session.id,
            tenant_id: session.tenant_id.clone(),
            principal_id: session.principal_id.clone(),
            branch_id: Some(node.branch_id),
            parent_id: node.parent_id,
            kind: format!("node:{:?}", node.kind).to_lowercase(),
            payload: node.payload.clone(),
            created_at: node.created_at,
            tags: node.tags.clone(),
        }));

        records.extend(
            session
                .graph
                .checkpoints
                .values()
                .map(|checkpoint| SurrealGraphRecord {
                    id: format!("checkpoint:{}", checkpoint.id),
                    session_id: session.id,
                    tenant_id: session.tenant_id.clone(),
                    principal_id: session.principal_id.clone(),
                    branch_id: Some(checkpoint.branch_id),
                    parent_id: Some(checkpoint.id),
                    kind: "checkpoint".to_string(),
                    payload: serde_json::json!({
                        "label": checkpoint.label,
                        "note": checkpoint.note,
                    }),
                    created_at: checkpoint.created_at,
                    tags: checkpoint.tags.clone(),
                }),
        );

        records.extend(
            session
                .graph
                .bookmarks
                .values()
                .map(|bookmark| SurrealGraphRecord {
                    id: format!("bookmark:{}", bookmark.id),
                    session_id: session.id,
                    tenant_id: session.tenant_id.clone(),
                    principal_id: session.principal_id.clone(),
                    branch_id: Some(bookmark.branch_id),
                    parent_id: Some(bookmark.node_id),
                    kind: "bookmark".to_string(),
                    payload: serde_json::json!({
                        "label": bookmark.label,
                        "note": bookmark.note,
                    }),
                    created_at: bookmark.created_at,
                    tags: Vec::new(),
                }),
        );

        records.sort_by_key(|record| record.created_at);
        records
    }
}

#[async_trait::async_trait]
impl Persistence for SurrealPersistence {
    fn name(&self) -> &str {
        "surreal-prototype"
    }

    async fn save(&self, _session: &Session) -> SessionResult<()> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }

    async fn load(&self, _id: &SessionId) -> SessionResult<Option<Session>> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }

    async fn delete(&self, _id: &SessionId) -> SessionResult<bool> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }

    async fn list(&self, _tenant_id: Option<&str>) -> SessionResult<Vec<SessionId>> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }

    async fn add_summary(&self, _snapshot: SummarySnapshot) -> SessionResult<()> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }

    async fn get_summaries(&self, _session_id: &SessionId) -> SessionResult<Vec<SummarySnapshot>> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }

    async fn enqueue(
        &self,
        _session_id: &SessionId,
        _content: String,
        _priority: i32,
    ) -> SessionResult<QueueItem> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }

    async fn dequeue(&self, _session_id: &SessionId) -> SessionResult<Option<QueueItem>> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }

    async fn cancel_queued(&self, _item_id: Uuid) -> SessionResult<bool> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }

    async fn pending_queue(&self, _session_id: &SessionId) -> SessionResult<Vec<QueueItem>> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }

    async fn cleanup_expired(&self) -> SessionResult<usize> {
        Err(SessionError::Storage {
            message: "SurrealPersistence is an experimental prototype and is not wired to a live SurrealDB client yet".to_string(),
        })
    }
}

pub fn surreal_prototype() -> Arc<dyn Persistence> {
    Arc::new(SurrealPersistence::new(SurrealConfig::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Session, SessionConfig, SessionMessage};
    use crate::types::ContentBlock;

    #[test]
    fn surreal_prototype_exports_graph_records() {
        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("hello")]));
        session.bookmark_current_head("start", None);
        session.graph.create_checkpoint(
            session.graph.primary_branch,
            "milestone",
            None,
            vec!["tag".to_string()],
        );

        let persistence = SurrealPersistence::new(SurrealConfig::default());
        let records = persistence.export_graph_records(&session);

        assert!(records.iter().any(|record| record.kind == "session"));
        assert!(records.iter().any(|record| record.kind == "bookmark"));
        assert!(records.iter().any(|record| record.kind == "checkpoint"));
    }
}
