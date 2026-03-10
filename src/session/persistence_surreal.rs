use std::sync::Arc;

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::persistence::Persistence;
use super::state::{Session, SessionId, SessionPermissions, SessionState, SessionType};
use super::types::{QueueItem, SummarySnapshot};
use super::{SessionError, SessionResult};
use crate::graph::{Bookmark, Branch, Checkpoint, GraphNode, NodeKind, SessionGraph};
use rust_decimal::Decimal;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SurrealConfig {
    pub namespace: String,
    pub database: String,
    pub endpoint: String,
    pub username: String,
    pub password: String,
}

impl Default for SurrealConfig {
    fn default() -> Self {
        Self {
            namespace: "claude_agent".to_string(),
            database: "session_graph".to_string(),
            endpoint: "http://127.0.0.1:58000/sql".to_string(),
            username: "root".to_string(),
            password: "root".to_string(),
        }
    }
}

impl SurrealConfig {
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    pub fn namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    pub fn database(mut self, database: impl Into<String>) -> Self {
        self.database = database.into();
        self
    }

    pub fn credentials(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = username.into();
        self.password = password.into();
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SurrealGraphRecord {
    pub id: String,
    #[serde(default)]
    pub logical_id: Option<String>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SurrealSessionSnapshot {
    pub id: SessionId,
    pub parent_id: Option<SessionId>,
    pub session_type: SessionType,
    pub tenant_id: Option<String>,
    pub principal_id: Option<String>,
    pub state: SessionState,
    pub config: super::state::SessionConfig,
    pub permissions: SessionPermissions,
    pub summary: Option<String>,
    pub total_usage: crate::types::TokenUsage,
    pub current_input_tokens: u64,
    pub total_cost_usd: Decimal,
    pub static_context_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub todos: Vec<super::types::TodoItem>,
    pub current_plan: Option<super::types::Plan>,
    pub compact_history: std::collections::VecDeque<super::types::CompactRecord>,
    pub primary_branch: Uuid,
}

impl SurrealSessionSnapshot {
    fn from_session(session: &Session) -> Self {
        Self {
            id: session.id,
            parent_id: session.parent_id,
            session_type: session.session_type.clone(),
            tenant_id: session.tenant_id.clone(),
            principal_id: session.principal_id.clone(),
            state: session.state,
            config: session.config.clone(),
            permissions: session.permissions.clone(),
            summary: session.summary.clone(),
            total_usage: session.total_usage.clone(),
            current_input_tokens: session.current_input_tokens,
            total_cost_usd: session.total_cost_usd,
            static_context_hash: session.static_context_hash.clone(),
            created_at: session.created_at,
            updated_at: session.updated_at,
            expires_at: session.expires_at,
            error: session.error.clone(),
            todos: session.todos.clone(),
            current_plan: session.current_plan.clone(),
            compact_history: session.compact_history.clone(),
            primary_branch: session.graph.primary_branch,
        }
    }

    fn into_session(self, graph: SessionGraph) -> Session {
        let mut session = Session {
            id: self.id,
            parent_id: self.parent_id,
            session_type: self.session_type,
            tenant_id: self.tenant_id,
            principal_id: self.principal_id,
            state: self.state,
            config: self.config,
            permissions: self.permissions,
            messages: Vec::new(),
            current_leaf_id: None,
            summary: self.summary,
            total_usage: self.total_usage,
            current_input_tokens: self.current_input_tokens,
            total_cost_usd: self.total_cost_usd,
            static_context_hash: self.static_context_hash,
            graph,
            created_at: self.created_at,
            updated_at: self.updated_at,
            expires_at: self.expires_at,
            error: self.error,
            todos: self.todos,
            current_plan: self.current_plan,
            compact_history: self.compact_history,
        };
        session.refresh_message_projection();
        session
    }
}

pub struct SurrealPersistence {
    config: SurrealConfig,
    client: Client,
}

impl SurrealPersistence {
    pub fn new(config: SurrealConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    pub fn config(&self) -> &SurrealConfig {
        &self.config
    }

    pub fn export_graph_records(&self, session: &Session) -> Vec<SurrealGraphRecord> {
        let mut records = Vec::new();

        records.push(SurrealGraphRecord {
            id: format!("session:{}", session.id),
            logical_id: Some(format!("session:{}", session.id)),
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
                    logical_id: Some(format!("branch:{}", branch.id)),
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
            logical_id: Some(format!("node:{}", node.id)),
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
                    logical_id: Some(format!("checkpoint:{}", checkpoint.id)),
                    session_id: session.id,
                    tenant_id: session.tenant_id.clone(),
                    principal_id: checkpoint.created_by_principal_id.clone(),
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
                    logical_id: Some(format!("bookmark:{}", bookmark.id)),
                    session_id: session.id,
                    tenant_id: session.tenant_id.clone(),
                    principal_id: bookmark.created_by_principal_id.clone(),
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

    fn surreal_graph_records_payload(&self, session: &Session) -> SessionResult<String> {
        let records = self
            .export_graph_records(session)
            .into_iter()
            .map(|record| {
                let mut value =
                    serde_json::to_value(record).map_err(SessionError::Serialization)?;
                if let Some(object) = value.as_object_mut()
                    && let Some(id) = object.remove("id")
                {
                    object.insert("logical_id".to_string(), id);
                }
                Ok(value)
            })
            .collect::<SessionResult<Vec<_>>>()?;
        serde_json::to_string(&records).map_err(SessionError::Serialization)
    }

    fn rebuild_graph(
        &self,
        session_id: SessionId,
        primary_branch: Uuid,
        rows: Vec<serde_json::Value>,
    ) -> SessionResult<SessionGraph> {
        let mut graph = SessionGraph::new("main");
        graph.id = session_id.0;
        graph.primary_branch = primary_branch;
        graph.branches.clear();
        graph.nodes.clear();
        graph.checkpoints.clear();
        graph.bookmarks.clear();
        graph.events.clear();

        for row in rows {
            let record: SurrealGraphRecord =
                serde_json::from_value(row).map_err(SessionError::Serialization)?;
            match record.kind.as_str() {
                "session" => {
                    graph.created_at = record.created_at;
                }
                "branch" => {
                    let branch_id = record.branch_id.ok_or_else(|| SessionError::Storage {
                        message: "Missing branch_id in Surreal branch record".to_string(),
                    })?;
                    graph.branches.insert(
                        branch_id,
                        Branch {
                            id: branch_id,
                            name: record
                                .payload
                                .get("name")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("branch")
                                .to_string(),
                            forked_from: record.parent_id,
                            created_at: record.created_at,
                            head: record
                                .payload
                                .get("head")
                                .and_then(|value| serde_json::from_value(value.clone()).ok()),
                        },
                    );
                }
                "checkpoint" => {
                    let checkpoint_id = logical_uuid(&record)?;
                    let branch_id = record.branch_id.ok_or_else(|| SessionError::Storage {
                        message: "Missing branch_id in Surreal checkpoint record".to_string(),
                    })?;
                    let checkpoint = Checkpoint {
                        id: checkpoint_id,
                        branch_id,
                        label: record
                            .payload
                            .get("label")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        note: record
                            .payload
                            .get("note")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        tags: record.tags.clone(),
                        created_by_principal_id: record.principal_id.clone(),
                        created_at: record.created_at,
                    };
                    graph.checkpoints.insert(checkpoint_id, checkpoint.clone());
                    graph.nodes.insert(
                        checkpoint_id,
                        GraphNode {
                            id: checkpoint_id,
                            branch_id,
                            kind: NodeKind::Checkpoint,
                            parent_id: record.parent_id,
                            created_by_principal_id: record.principal_id,
                            created_at: record.created_at,
                            tags: record.tags,
                            payload: record.payload,
                        },
                    );
                }
                "bookmark" => {
                    let bookmark_id = logical_uuid(&record)?;
                    let branch_id = record.branch_id.ok_or_else(|| SessionError::Storage {
                        message: "Missing branch_id in Surreal bookmark record".to_string(),
                    })?;
                    let node_id = record.parent_id.ok_or_else(|| SessionError::Storage {
                        message: "Missing parent_id in Surreal bookmark record".to_string(),
                    })?;
                    graph.bookmarks.insert(
                        bookmark_id,
                        Bookmark {
                            id: bookmark_id,
                            node_id,
                            branch_id,
                            label: record
                                .payload
                                .get("label")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            note: record
                                .payload
                                .get("note")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string),
                            created_by_principal_id: record.principal_id,
                            created_at: record.created_at,
                        },
                    );
                }
                kind if kind.starts_with("node:") => {
                    let node_id = logical_uuid(&record)?;
                    let branch_id = record.branch_id.ok_or_else(|| SessionError::Storage {
                        message: "Missing branch_id in Surreal node record".to_string(),
                    })?;
                    let node_kind = parse_node_kind(kind.trim_start_matches("node:"))?;
                    graph.nodes.insert(
                        node_id,
                        GraphNode {
                            id: node_id,
                            branch_id,
                            kind: node_kind,
                            parent_id: record.parent_id,
                            created_by_principal_id: record.principal_id,
                            created_at: record.created_at,
                            tags: record.tags,
                            payload: record.payload,
                        },
                    );
                }
                _ => {}
            }
        }

        if graph.branches.is_empty() {
            graph.branches.insert(
                primary_branch,
                Branch {
                    id: primary_branch,
                    name: "main".to_string(),
                    forked_from: None,
                    created_at: graph.created_at,
                    head: graph
                        .nodes
                        .keys()
                        .copied()
                        .max_by_key(|id| graph.nodes.get(id).map(|node| node.created_at)),
                },
            );
        }

        Ok(graph)
    }

    async fn ensure_schema(&self) -> SessionResult<()> {
        self.bootstrap_namespace_database().await?;
        self.execute_unit(
            "DEFINE TABLE IF NOT EXISTS session_snapshot SCHEMALESS;\n\
             DEFINE TABLE IF NOT EXISTS graph_record SCHEMALESS;\n\
             DEFINE TABLE IF NOT EXISTS summary_record SCHEMALESS;\n\
             DEFINE TABLE IF NOT EXISTS queue_record SCHEMALESS;",
        )
        .await
    }

    async fn bootstrap_namespace_database(&self) -> SessionResult<()> {
        let sql = format!(
            "DEFINE NAMESPACE IF NOT EXISTS {};\nDEFINE DATABASE IF NOT EXISTS {};",
            self.config.namespace, self.config.database
        );

        let response = self
            .client
            .post(&self.config.endpoint)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .header("Accept", "application/json")
            .body(sql)
            .send()
            .await
            .map_err(|e| SessionError::Storage {
                message: format!("SurrealDB bootstrap request failed: {e}"),
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|e| SessionError::Storage {
            message: format!("SurrealDB bootstrap response read failed: {e}"),
        })?;
        if !status.is_success() {
            return Err(SessionError::Storage {
                message: format!("SurrealDB bootstrap failed with {status}: {body}"),
            });
        }
        Ok(())
    }

    async fn execute_unit(&self, sql: &str) -> SessionResult<()> {
        let response = self
            .client
            .post(&self.config.endpoint)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .header("surreal-ns", &self.config.namespace)
            .header("surreal-db", &self.config.database)
            .header("Accept", "application/json")
            .body(sql.to_string())
            .send()
            .await
            .map_err(|e| SessionError::Storage {
                message: format!("SurrealDB request failed: {e}"),
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|e| SessionError::Storage {
            message: format!("SurrealDB response read failed: {e}"),
        })?;
        if !status.is_success() {
            return Err(SessionError::Storage {
                message: format!("SurrealDB request failed with {status}: {body}"),
            });
        }

        let parsed: serde_json::Value =
            serde_json::from_str(&body).map_err(SessionError::Serialization)?;
        if parsed.to_string().contains("\"status\":\"ERR\"") {
            return Err(SessionError::Storage {
                message: format!("SurrealDB returned error response: {body}"),
            });
        }
        Ok(())
    }

    async fn execute_query(&self, sql: &str) -> SessionResult<serde_json::Value> {
        let response = self
            .client
            .post(&self.config.endpoint)
            .basic_auth(&self.config.username, Some(&self.config.password))
            .header("surreal-ns", &self.config.namespace)
            .header("surreal-db", &self.config.database)
            .header("Accept", "application/json")
            .body(sql.to_string())
            .send()
            .await
            .map_err(|e| SessionError::Storage {
                message: format!("SurrealDB request failed: {e}"),
            })?;

        let status = response.status();
        let body = response.text().await.map_err(|e| SessionError::Storage {
            message: format!("SurrealDB response read failed: {e}"),
        })?;
        if !status.is_success() {
            return Err(SessionError::Storage {
                message: format!("SurrealDB request failed with {status}: {body}"),
            });
        }

        serde_json::from_str(&body).map_err(SessionError::Serialization)
    }

    fn extract_result_rows(value: &serde_json::Value) -> SessionResult<Vec<serde_json::Value>> {
        let array = value.as_array().ok_or_else(|| SessionError::Storage {
            message: format!("Unexpected SurrealDB response shape: {value}"),
        })?;
        let mut rows = Vec::new();
        for statement in array {
            match statement.get("status").and_then(serde_json::Value::as_str) {
                Some("OK") | None => {}
                Some(_) => {
                    return Err(SessionError::Storage {
                        message: format!("SurrealDB statement failed: {statement}"),
                    });
                }
            }
            if let Some(result) = statement.get("result")
                && let Some(result_rows) = result.as_array()
            {
                rows.extend(result_rows.iter().cloned());
            }
        }
        Ok(rows)
    }
}

fn parse_record_uuid(record_id: &str) -> SessionResult<Uuid> {
    record_id
        .rsplit(':')
        .next()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(|| SessionError::Storage {
            message: format!("Invalid SurrealDB logical record id: {record_id}"),
        })
}

fn logical_uuid(record: &SurrealGraphRecord) -> SessionResult<Uuid> {
    record
        .logical_id
        .as_deref()
        .map(parse_record_uuid)
        .unwrap_or_else(|| parse_record_uuid(&record.id))
}

fn parse_node_kind(kind: &str) -> SessionResult<NodeKind> {
    serde_json::from_str(&format!("\"{}\"", kind)).map_err(SessionError::Serialization)
}

#[async_trait::async_trait]
impl Persistence for SurrealPersistence {
    fn name(&self) -> &str {
        "surrealdb"
    }

    async fn save(&self, session: &Session) -> SessionResult<()> {
        self.ensure_schema().await?;

        let snapshot = serde_json::to_string(&SurrealSessionSnapshot::from_session(session))
            .map_err(SessionError::Serialization)?;
        let graph_records = self.surreal_graph_records_payload(session)?;
        let sql = format!(
            "BEGIN TRANSACTION;\n\
             DELETE session_snapshot WHERE session_id = {};\n\
             CREATE session_snapshot CONTENT {{ snapshot_id: {}, session_id: {}, tenant_id: {}, principal_id: {}, payload: {}, updated_at: time::now() }};\n\
             DELETE graph_record WHERE session_id = {};\n\
             FOR $record IN {} {{ CREATE graph_record CONTENT $record; }};\n\
             COMMIT TRANSACTION;",
            serde_json::to_string(&session.id.to_string()).unwrap(),
            serde_json::to_string(&Uuid::new_v4().to_string()).unwrap(),
            serde_json::to_string(&session.id.to_string()).unwrap(),
            serde_json::to_string(&session.tenant_id).unwrap(),
            serde_json::to_string(&session.principal_id).unwrap(),
            snapshot,
            serde_json::to_string(&session.id.to_string()).unwrap(),
            graph_records,
        );

        self.execute_unit(&sql).await
    }

    async fn load(&self, id: &SessionId) -> SessionResult<Option<Session>> {
        self.ensure_schema().await?;
        let sql = format!(
            "SELECT payload FROM session_snapshot WHERE session_id = {} LIMIT 1;",
            serde_json::to_string(&id.to_string()).unwrap()
        );
        let value = self.execute_query(&sql).await?;
        let rows = Self::extract_result_rows(&value)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let payload = row
            .get("payload")
            .cloned()
            .ok_or_else(|| SessionError::Storage {
                message: format!("Missing SurrealDB session payload for {id}"),
            })?;
        let snapshot: SurrealSessionSnapshot =
            serde_json::from_value(payload).map_err(SessionError::Serialization)?;
        let graph_rows = Self::extract_result_rows(
            &self
                .execute_query(&format!(
                    "SELECT * FROM graph_record WHERE session_id = {} ORDER BY created_at ASC;",
                    serde_json::to_string(&id.to_string()).unwrap()
                ))
                .await?,
        )?;
        let graph = self.rebuild_graph(snapshot.id, snapshot.primary_branch, graph_rows)?;
        Ok(Some(snapshot.into_session(graph)))
    }

    async fn delete(&self, id: &SessionId) -> SessionResult<bool> {
        self.ensure_schema().await?;
        let sql = format!(
            "BEGIN TRANSACTION;\n\
             DELETE session_snapshot WHERE session_id = {};\n\
             DELETE graph_record WHERE session_id = {};\n\
             DELETE summary_record WHERE session_id = {};\n\
             DELETE queue_record WHERE session_id = {};\n\
             COMMIT TRANSACTION;",
            serde_json::to_string(&id.to_string()).unwrap(),
            serde_json::to_string(&id.to_string()).unwrap(),
            serde_json::to_string(&id.to_string()).unwrap(),
            serde_json::to_string(&id.to_string()).unwrap(),
        );
        self.execute_unit(&sql).await?;
        Ok(true)
    }

    async fn list(&self, tenant_id: Option<&str>) -> SessionResult<Vec<SessionId>> {
        self.ensure_schema().await?;
        let sql = match tenant_id {
            Some(tenant_id) => format!(
                "SELECT session_id FROM session_snapshot WHERE tenant_id = {};",
                serde_json::to_string(tenant_id).unwrap()
            ),
            None => "SELECT session_id FROM session_snapshot;".to_string(),
        };
        let rows = Self::extract_result_rows(&self.execute_query(&sql).await?)?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                row.get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .map(SessionId::from)
            })
            .collect())
    }

    async fn add_summary(&self, snapshot: SummarySnapshot) -> SessionResult<()> {
        self.ensure_schema().await?;
        let content = serde_json::to_string(&snapshot).map_err(SessionError::Serialization)?;
        let sql = format!(
            "CREATE summary_record CONTENT {{ summary_id: {}, session_id: {}, payload: {}, created_at: time::now() }};",
            serde_json::to_string(&Uuid::new_v4().to_string()).unwrap(),
            serde_json::to_string(&snapshot.session_id.to_string()).unwrap(),
            content,
        );
        self.execute_unit(&sql).await
    }

    async fn get_summaries(&self, session_id: &SessionId) -> SessionResult<Vec<SummarySnapshot>> {
        self.ensure_schema().await?;
        let sql = format!(
            "SELECT payload FROM summary_record WHERE session_id = {} ORDER BY created_at ASC;",
            serde_json::to_string(&session_id.to_string()).unwrap()
        );
        let rows = Self::extract_result_rows(&self.execute_query(&sql).await?)?;
        rows.into_iter()
            .map(|row| {
                row.get("payload")
                    .cloned()
                    .ok_or_else(|| SessionError::Storage {
                        message: "Missing summary payload".to_string(),
                    })
                    .and_then(|payload| {
                        serde_json::from_value(payload).map_err(SessionError::Serialization)
                    })
            })
            .collect()
    }

    async fn enqueue(
        &self,
        session_id: &SessionId,
        content: String,
        priority: i32,
    ) -> SessionResult<QueueItem> {
        self.ensure_schema().await?;
        let item = QueueItem::enqueue(*session_id, content).priority(priority);
        let payload = serde_json::to_string(&item).map_err(SessionError::Serialization)?;
        let sql = format!(
            "CREATE queue_record CONTENT {{ item_id: {}, session_id: {}, payload: {}, created_at: time::now() }};",
            serde_json::to_string(&item.id.to_string()).unwrap(),
            serde_json::to_string(&session_id.to_string()).unwrap(),
            payload,
        );
        self.execute_unit(&sql).await?;
        Ok(item)
    }

    async fn dequeue(&self, session_id: &SessionId) -> SessionResult<Option<QueueItem>> {
        self.ensure_schema().await?;
        let sql = format!(
            "SELECT * FROM queue_record WHERE session_id = {} ORDER BY created_at ASC;",
            serde_json::to_string(&session_id.to_string()).unwrap()
        );
        let mut items: Vec<QueueItem> =
            Self::extract_result_rows(&self.execute_query(&sql).await?)?
                .into_iter()
                .filter_map(|row| row.get("payload").cloned())
                .map(|payload| serde_json::from_value(payload).map_err(SessionError::Serialization))
                .collect::<SessionResult<Vec<_>>>()?;
        items.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        let Some(pos) = items
            .iter()
            .position(|item| item.status == super::types::QueueStatus::Pending)
        else {
            return Ok(None);
        };
        let mut item = items.swap_remove(pos);
        item.start_processing();
        let payload = serde_json::to_string(&item).map_err(SessionError::Serialization)?;
        let update_sql = format!(
            "DELETE queue_record WHERE item_id = {};\nCREATE queue_record CONTENT {{ item_id: {}, session_id: {}, payload: {}, created_at: time::now() }};",
            serde_json::to_string(&item.id.to_string()).unwrap(),
            serde_json::to_string(&item.id.to_string()).unwrap(),
            serde_json::to_string(&session_id.to_string()).unwrap(),
            payload,
        );
        self.execute_unit(&update_sql).await?;
        Ok(Some(item))
    }

    async fn cancel_queued(&self, item_id: Uuid) -> SessionResult<bool> {
        self.ensure_schema().await?;
        let sql = format!(
            "SELECT payload FROM queue_record WHERE item_id = {} LIMIT 1;",
            serde_json::to_string(&item_id.to_string()).unwrap()
        );
        let rows = Self::extract_result_rows(&self.execute_query(&sql).await?)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(false);
        };
        let payload = row
            .get("payload")
            .cloned()
            .ok_or_else(|| SessionError::Storage {
                message: "Missing queue payload".to_string(),
            })?;
        let mut item: QueueItem =
            serde_json::from_value(payload).map_err(SessionError::Serialization)?;
        item.cancel();
        let update_sql = format!(
            "DELETE queue_record WHERE item_id = {};\nCREATE queue_record CONTENT {{ item_id: {}, session_id: {}, payload: {}, created_at: time::now() }};",
            serde_json::to_string(&item.id.to_string()).unwrap(),
            serde_json::to_string(&item.id.to_string()).unwrap(),
            serde_json::to_string(&item.session_id.to_string()).unwrap(),
            serde_json::to_string(&item).unwrap(),
        );
        self.execute_unit(&update_sql).await?;
        Ok(true)
    }

    async fn pending_queue(&self, session_id: &SessionId) -> SessionResult<Vec<QueueItem>> {
        self.ensure_schema().await?;
        let sql = format!(
            "SELECT * FROM queue_record WHERE session_id = {} ORDER BY created_at ASC;",
            serde_json::to_string(&session_id.to_string()).unwrap()
        );
        let items = Self::extract_result_rows(&self.execute_query(&sql).await?)?
            .into_iter()
            .filter_map(|row| row.get("payload").cloned())
            .map(|payload| serde_json::from_value(payload).map_err(SessionError::Serialization))
            .collect::<SessionResult<Vec<QueueItem>>>()?;
        Ok(items
            .into_iter()
            .filter(|item| item.status == super::types::QueueStatus::Pending)
            .collect())
    }

    async fn cleanup_expired(&self) -> SessionResult<usize> {
        self.ensure_schema().await?;
        let sql = "DELETE session_snapshot WHERE expires_at != NONE AND expires_at < time::now();";
        self.execute_unit(sql).await?;
        Ok(0)
    }
}

pub fn surreal_prototype() -> Arc<dyn Persistence> {
    Arc::new(SurrealPersistence::new(SurrealConfig::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionConfig, SessionMessage};
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
            None,
        );

        let persistence = SurrealPersistence::new(SurrealConfig::default());
        let records = persistence.export_graph_records(&session);

        assert!(records.iter().any(|record| record.kind == "session"));
        assert!(records.iter().any(|record| record.kind == "bookmark"));
        assert!(records.iter().any(|record| record.kind == "checkpoint"));
    }
}
