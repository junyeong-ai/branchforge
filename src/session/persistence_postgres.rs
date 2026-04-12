//! PostgreSQL session persistence with explicit schema management.
//!
//! # Schema Management
//!
//! This module separates schema management from data access, allowing flexible deployment:
//!
//! ```rust,no_run
//! use branchforge::session::{PostgresPersistence, PostgresSchema, PostgresConfig};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Option 1: Auto-migrate (development/simple deployments)
//! let persistence = PostgresPersistence::connect_and_migrate("postgres://...").await?;
//!
//! // Option 2: Connect only, manage schema externally (production)
//! let persistence = PostgresPersistence::connect("postgres://...").await?;
//!
//! // Option 3: Export SQL for external migration tools (Flyway, Diesel, etc.)
//! let sql = PostgresSchema::sql(&PostgresConfig::default());
//! println!("{}", sql);
//!
//! // Option 4: Verify schema is correct
//! let issues = persistence.verify_schema().await?;
//! if !issues.is_empty() {
//!     for issue in &issues {
//!         eprintln!("Schema issue: {:?}", issue);
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::graph::{GraphEvent, GraphEventBody, GraphValidator, SessionGraph};

use super::archive::verify_restored_session_roundtrip;
use super::persistence::{Persistence, SessionMutationFn};
use super::state::{
    Session, SessionAuthorization, SessionConfig, SessionId, SessionMessage, SessionState,
    SessionType, build_graph_provenance, graph_node_id_for_message, graph_node_kind_for_message,
    graph_parent_node_id_for_message, graph_payload_for_message, graph_tags_for_message,
};
use super::types::{CompactRecord, Plan, QueueItem, QueueItemState, QueueOperation, TodoItem};
use super::{SessionError, SessionResult, StorageResultExt};

fn enum_to_db<T: serde::Serialize>(value: &T, default: &str) -> String {
    serde_json::to_string(value)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|_| default.to_string())
}

fn db_to_enum<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_str(&format!("\"{}\"", s)).ok()
}

fn db_to_session_type(s: &str) -> SessionType {
    serde_json::from_str(s)
        .ok()
        .or_else(|| db_to_enum(s))
        .unwrap_or_default()
}

/// Phase G-1: derive a stable i64 key for
/// `pg_advisory_xact_lock($1)` from a session UUID. Folds the 128-bit
/// UUID into 64 bits by XOR-ing the two halves; Postgres treats the
/// resulting i64 as an opaque application-defined lock identifier.
fn session_advisory_lock_key(id: &SessionId) -> i64 {
    let (hi, lo) = id.as_uuid().as_u64_pair();
    (hi ^ lo) as i64
}

fn validate_graph(session_id: &SessionId, graph: &SessionGraph) -> SessionResult<()> {
    let report = GraphValidator::validate(graph);
    if report.is_valid() {
        return Ok(());
    }

    let issues = report
        .issues
        .into_iter()
        .map(|issue| format!("{}: {}", issue.code, issue.message))
        .collect::<Vec<_>>()
        .join("; ");
    Err(SessionError::Storage {
        message: format!("Invalid graph for session {}: {}", session_id, issues),
    })
}

fn deserialize_session_config(row: &PgRow, session_id: &SessionId) -> SessionConfig {
    match row.try_get::<serde_json::Value, _>("config") {
        Ok(v) => serde_json::from_value(v).unwrap_or_else(|e| {
            tracing::warn!(session_id = %session_id, error = %e, "Failed to deserialize session config");
            Default::default()
        }),
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "Failed to read session config column");
            Default::default()
        }
    }
}

fn deserialize_session_authorization(row: &PgRow, session_id: &SessionId) -> SessionAuthorization {
    match row.try_get::<serde_json::Value, _>("authorization_policy") {
        Ok(v) => serde_json::from_value(v).unwrap_or_else(|e| {
            tracing::warn!(session_id = %session_id, error = %e, "Failed to deserialize session authorization");
            Default::default()
        }),
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "Failed to read session authorization_policy column");
            Default::default()
        }
    }
}

fn parse_graph_event_rows(rows: Vec<PgRow>) -> Vec<crate::graph::GraphEvent> {
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        if let Ok(value) = row.try_get::<serde_json::Value, _>("event")
            && let Ok(event) = serde_json::from_value(value)
        {
            events.push(event);
        }
    }
    events
}

fn parse_compact_rows(session_id: &SessionId, rows: Vec<PgRow>) -> Vec<CompactRecord> {
    let mut compacts = Vec::with_capacity(rows.len());

    for row in rows {
        let id: Uuid = match row.try_get("id") {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(session_id = %session_id, error = %e, "Skipping compact row: failed to get id");
                continue;
            }
        };

        let trigger = match row.try_get::<&str, _>("trigger").ok().and_then(db_to_enum) {
            Some(t) => t,
            None => {
                tracing::warn!(session_id = %session_id, compact_id = %id, "Skipping compact row: failed to parse trigger");
                continue;
            }
        };

        let summary = match row.try_get("summary") {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(session_id = %session_id, compact_id = %id, error = %e, "Skipping compact row: failed to get summary");
                continue;
            }
        };

        compacts.push(CompactRecord {
            id,
            session_id: *session_id,
            trigger,
            pre_tokens: crate::ir::TokenCount::new(
                row.try_get::<i32, _>("pre_tokens").unwrap_or(0) as u64,
            ),
            post_tokens: crate::ir::TokenCount::new(
                row.try_get::<i32, _>("post_tokens").unwrap_or(0) as u64,
            ),
            saved_tokens: crate::ir::TokenCount::new(
                row.try_get::<i32, _>("saved_tokens").unwrap_or(0) as u64,
            ),
            summary,
            original_count: row.try_get::<i32, _>("original_count").unwrap_or(0) as usize,
            new_count: row.try_get::<i32, _>("new_count").unwrap_or(0) as usize,
            logical_parent_id: row
                .try_get::<&str, _>("logical_parent_id")
                .ok()
                .and_then(|s| s.parse().ok()),
            created_at: row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
        });
    }

    compacts
}

fn parse_todo_rows(session_id: &SessionId, rows: Vec<PgRow>) -> Vec<TodoItem> {
    let mut todos = Vec::with_capacity(rows.len());

    for row in rows {
        let id: Uuid = match row.try_get("id") {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(session_id = %session_id, error = %e, "Skipping todo row: failed to get id");
                continue;
            }
        };

        let status = match row.try_get::<&str, _>("status").ok().and_then(db_to_enum) {
            Some(s) => s,
            None => {
                tracing::warn!(session_id = %session_id, todo_id = %id, "Skipping todo row: failed to parse status");
                continue;
            }
        };

        let content = match row.try_get("content") {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(session_id = %session_id, todo_id = %id, error = %e, "Skipping todo row: failed to get content");
                continue;
            }
        };

        let active_form = match row.try_get("active_form") {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(session_id = %session_id, todo_id = %id, error = %e, "Skipping todo row: failed to get active_form");
                continue;
            }
        };

        todos.push(TodoItem {
            id,
            session_id: *session_id,
            content,
            active_form,
            status,
            plan_id: row.try_get("plan_id").ok(),
            created_at: row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
            started_at: row.try_get("started_at").ok(),
            completed_at: row.try_get("completed_at").ok(),
        });
    }

    todos
}

fn parse_plan_row(session_id: &SessionId, row: Option<PgRow>) -> Option<Plan> {
    let row = row?;

    let id: Uuid = match row.try_get("id") {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "Skipping plan row: failed to get id");
            return None;
        }
    };

    let status = match row.try_get::<&str, _>("status").ok().and_then(db_to_enum) {
        Some(s) => s,
        None => {
            tracing::warn!(session_id = %session_id, plan_id = %id, "Skipping plan row: failed to parse status");
            return None;
        }
    };

    let content = match row.try_get("content") {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(session_id = %session_id, plan_id = %id, error = %e, "Skipping plan row: failed to get content");
            return None;
        }
    };

    Some(Plan {
        id,
        session_id: *session_id,
        name: row.try_get("name").ok(),
        content,
        state: status,
        error: row.try_get("error").ok(),
        created_at: row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
        approved_at: row.try_get("approved_at").ok(),
        started_at: row.try_get("started_at").ok(),
        completed_at: row.try_get("completed_at").ok(),
    })
}

fn parse_pending_queue_rows(session_id: &SessionId, rows: Vec<PgRow>) -> Vec<QueueItem> {
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let id: Uuid = match row.try_get("id") {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(session_id = %session_id, error = %e, "Skipping queue row: failed to get id");
                continue;
            }
        };

        let content = match row.try_get("content") {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(session_id = %session_id, queue_id = %id, error = %e, "Skipping queue row: failed to get content");
                continue;
            }
        };

        items.push(QueueItem {
            id,
            session_id: *session_id,
            operation: QueueOperation::Enqueue,
            content,
            priority: row.try_get("priority").unwrap_or(0),
            state: QueueItemState::Pending,
            created_at: row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
            processed_at: row.try_get("processed_at").ok(),
        });
    }

    items
}

fn build_session_graph(
    session_id: &SessionId,
    created_at: chrono::DateTime<Utc>,
    current_leaf_id: Option<super::state::MessageId>,
    primary_branch_id: crate::graph::BranchId,
    graph_events: &[crate::graph::GraphEvent],
) -> SessionResult<SessionGraph> {
    let graph = if !graph_events.is_empty() {
        let mut graph = crate::graph::GraphMaterializer::from_events_with_primary(
            graph_events,
            Some(primary_branch_id),
        );
        graph.id = crate::graph::SessionGraphId::from_uuid(session_id.as_uuid());
        graph.created_at = created_at;
        if !graph.branches.contains_key(&primary_branch_id) {
            return Err(SessionError::Storage {
                message: format!(
                    "Session {} references missing primary branch {}",
                    session_id, primary_branch_id
                ),
            });
        }
        graph
    } else {
        let mut graph = SessionGraph::new("main");
        graph.id = crate::graph::SessionGraphId::from_uuid(session_id.as_uuid());
        graph.created_at = created_at;
        let original_primary = graph.primary_branch;
        let mut branch = graph
            .branches
            .remove(&original_primary)
            .unwrap_or(crate::graph::Branch {
                id: original_primary,
                name: "main".to_string(),
                forked_from: None,
                created_at,
                head: None,
            });
        branch.id = primary_branch_id;
        graph.primary_branch = primary_branch_id;
        graph.branches.insert(primary_branch_id, branch);
        graph
    };

    validate_graph(session_id, &graph)?;
    if graph_events.is_empty() && current_leaf_id.is_some() {
        return Err(SessionError::Storage {
            message: format!(
                "Session {} has a current leaf pointer but no graph events",
                session_id
            ),
        });
    }

    Ok(graph)
}

fn reconstruct_session_from_row(
    session_id: &SessionId,
    row: PgRow,
    graph_events: Vec<crate::graph::GraphEvent>,
    compacts: Vec<CompactRecord>,
    todos: Vec<TodoItem>,
    plan: Option<Plan>,
) -> SessionResult<Session> {
    let config = deserialize_session_config(&row, session_id);
    let authorization = deserialize_session_authorization(&row, session_id);

    let session_type = row
        .try_get::<&str, _>("session_type")
        .ok()
        .map(db_to_session_type)
        .unwrap_or_default();

    let _ = row.try_get::<&str, _>("mode");

    let state = row
        .try_get::<&str, _>("state")
        .ok()
        .and_then(db_to_enum)
        .unwrap_or_default();

    let current_leaf_id = row
        .try_get::<&str, _>("current_leaf_id")
        .ok()
        .and_then(|s| s.parse().ok());
    let primary_branch_id: crate::graph::BranchId = {
        let raw: Uuid = row
            .try_get("primary_branch_id")
            .map_err(|e| SessionError::Storage {
                message: format!(
                    "Session {} is missing required primary_branch_id: {}",
                    session_id, e
                ),
            })?;
        crate::graph::BranchId::from_uuid(raw)
    };

    let created_at = row.try_get("created_at").unwrap_or_else(|_| Utc::now());
    let mut session = Session {
        id: *session_id,
        parent_id: row
            .try_get::<&str, _>("parent_id")
            .ok()
            .and_then(|s| s.parse().ok()),
        session_type,
        tenant_id: row.try_get("tenant_id").ok(),
        principal_id: row.try_get("principal_id").ok(),
        state,
        config,
        authorization,
        summary: row.try_get("summary").ok(),
        total_usage: crate::ir::Usage {
            input_tokens: row.try_get::<i64, _>("total_input_tokens").unwrap_or(0) as u64,
            output_tokens: row.try_get::<i64, _>("total_output_tokens").unwrap_or(0) as u64,
            ..Default::default()
        },
        current_input_tokens: 0,
        total_cost_usd: row
            .try_get::<rust_decimal::Decimal, _>("total_cost_usd")
            .unwrap_or_default(),
        static_context_hash: row.try_get("static_context_hash").ok(),
        created_at,
        updated_at: row.try_get("updated_at").unwrap_or_else(|_| Utc::now()),
        expires_at: row.try_get("expires_at").ok(),
        error: row.try_get("error").ok(),
        todos,
        current_plan: plan,
        compact_history: VecDeque::from(compacts),
        graph: SessionGraph::default(),
        event_bus: None,
        content_overrides: crate::session::state::ContentOverrides::default(),
    };
    session.graph = build_session_graph(
        session_id,
        session.created_at,
        current_leaf_id,
        primary_branch_id,
        &graph_events,
    )?;
    session.summary = session.graph.latest_summary();
    Ok(session)
}

// ============================================================================
// Configuration
// ============================================================================

/// Connection pool configuration for PostgreSQL.
#[derive(Clone, Debug)]
pub struct PgPoolConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
    pub acquire_timeout: Duration,
    /// Maximum retry attempts for transient failures.
    pub max_retries: u32,
    /// Initial backoff duration for retries.
    pub initial_backoff: Duration,
    /// Maximum backoff duration.
    pub max_backoff: Duration,
}

impl Default for PgPoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
            min_connections: 1,
            connect_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(600),
            max_lifetime: Duration::from_secs(1800),
            acquire_timeout: Duration::from_secs(30),
            max_retries: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
        }
    }
}

impl PgPoolConfig {
    pub fn high_throughput() -> Self {
        Self {
            max_connections: 50,
            min_connections: 5,
            connect_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(300),
            max_lifetime: Duration::from_secs(900),
            acquire_timeout: Duration::from_secs(10),
            max_retries: 3,
            initial_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_secs(2),
        }
    }

    pub(crate) fn apply(&self) -> PgPoolOptions {
        PgPoolOptions::new()
            .max_connections(self.max_connections)
            .min_connections(self.min_connections)
            .acquire_timeout(self.acquire_timeout)
            .idle_timeout(Some(self.idle_timeout))
            .max_lifetime(Some(self.max_lifetime))
    }
}

/// PostgreSQL persistence configuration.
#[derive(Clone, Debug)]
pub struct PostgresConfig {
    pub sessions_table: String,
    pub graph_events_table: String,
    pub compacts_table: String,
    pub queue_table: String,
    pub todos_table: String,
    pub plans_table: String,
    /// Phase D F-1: single-row-per-component schema-version
    /// tracking table. Defaults to `{prefix}schema_version`.
    /// Written by [`PostgresSchema::migrate`] and read back by
    /// [`PostgresSchema::verify_schema_version`] so the binary
    /// can refuse to operate on a database whose schema was
    /// written by a newer deployment.
    pub schema_version_table: String,
    pub pool: PgPoolConfig,
    /// Session retention period in days (default: 30).
    ///
    /// Sessions without explicit TTL that haven't been updated within
    /// this period are cleaned up by `cleanup_expired()`.
    pub retention_days: u32,
}

impl Default for PostgresConfig {
    fn default() -> Self {
        // Safety: "claude_" is a valid prefix (alphanumeric + underscore)
        Self::prefix("claude_").unwrap()
    }
}

impl PostgresConfig {
    pub fn prefix(prefix: &str) -> Result<Self, SessionError> {
        if !prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(SessionError::Storage {
                message: format!(
                    "Invalid table prefix '{}': only ASCII alphanumeric and underscore allowed",
                    prefix
                ),
            });
        }
        Ok(Self {
            sessions_table: format!("{prefix}sessions"),
            graph_events_table: format!("{prefix}graph_events"),
            compacts_table: format!("{prefix}compacts"),
            queue_table: format!("{prefix}queue"),
            todos_table: format!("{prefix}todos"),
            plans_table: format!("{prefix}plans"),
            schema_version_table: format!("{prefix}schema_version"),
            pool: PgPoolConfig::default(),
            retention_days: 30,
        })
    }

    pub fn pool(mut self, pool: PgPoolConfig) -> Self {
        self.pool = pool;
        self
    }

    pub fn retention_days(mut self, days: u32) -> Self {
        self.retention_days = days;
        self
    }

    /// Get all table names.
    pub fn table_names(&self) -> Vec<&str> {
        vec![
            &self.sessions_table,
            &self.graph_events_table,
            &self.compacts_table,
            &self.queue_table,
            &self.todos_table,
            &self.plans_table,
        ]
    }
}

// ============================================================================
// Schema Management
// ============================================================================

/// Schema issue found during verification.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum SchemaIssue {
    MissingTable(String),
    MissingIndex { table: String, index: String },
    MissingColumn { table: String, column: String },
}

impl std::fmt::Display for SchemaIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaIssue::MissingTable(t) => write!(f, "Missing table: {}", t),
            SchemaIssue::MissingIndex { table, index } => {
                write!(f, "Missing index '{}' on table '{}'", index, table)
            }
            SchemaIssue::MissingColumn { table, column } => {
                write!(f, "Missing column '{}' in table '{}'", column, table)
            }
        }
    }
}

/// Schema manager for PostgreSQL persistence.
///
/// Provides utilities for schema creation, migration, and verification.
pub struct PostgresSchema;

impl PostgresSchema {
    /// Generate complete SQL DDL for all tables and indexes.
    ///
    /// Use this to integrate with external migration tools (Flyway, Diesel, etc.).
    pub fn sql(config: &PostgresConfig) -> String {
        let mut sql = String::new();
        sql.push_str("-- Branchforge Session Schema\n");
        sql.push_str("-- Generated by branchforge PostgresSchema\n\n");

        for table_sql in Self::table_ddl(config) {
            sql.push_str(&table_sql);
            sql.push_str("\n\n");
        }

        sql.push_str("-- Indexes\n");
        for index_sql in Self::index_ddl(config) {
            sql.push_str(&index_sql);
            sql.push_str(";\n");
        }

        sql
    }

    /// Phase D F-1: schema-version tracking table DDL. Created
    /// alongside the session tables so every fresh database is
    /// stamped with its schema version, and so upgrades can
    /// query "what version am I reading?" without relying on
    /// out-of-band metadata.
    pub fn schema_version_table_ddl(config: &PostgresConfig) -> String {
        format!(
            r#"CREATE TABLE IF NOT EXISTS {table} (
    component VARCHAR(64) PRIMARY KEY,
    version BIGINT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);"#,
            table = config.schema_version_table
        )
    }

    /// Generate table DDL statements.
    pub fn table_ddl(config: &PostgresConfig) -> Vec<String> {
        let c = config;
        vec![
            format!(
                r#"CREATE TABLE IF NOT EXISTS {sessions} (
    id VARCHAR(255) PRIMARY KEY,
    parent_id VARCHAR(255),
    tenant_id VARCHAR(255),
    principal_id VARCHAR(255),
    session_type VARCHAR(32) NOT NULL DEFAULT 'main',
    state VARCHAR(32) NOT NULL DEFAULT 'created',
    mode VARCHAR(32) NOT NULL DEFAULT 'default',
    config JSONB NOT NULL DEFAULT '{{}}',
    authorization_policy JSONB NOT NULL DEFAULT '{{}}',
    summary TEXT,
    total_input_tokens BIGINT DEFAULT 0,
    total_output_tokens BIGINT DEFAULT 0,
    total_cost_usd DECIMAL(12, 6) DEFAULT 0,
    current_leaf_id VARCHAR(255),
    primary_branch_id UUID NOT NULL,
    static_context_hash VARCHAR(64),
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    CONSTRAINT fk_{sessions}_parent FOREIGN KEY (parent_id) REFERENCES {sessions}(id) ON DELETE CASCADE
);"#,
                sessions = c.sessions_table
            ),
            format!(
                r#"CREATE TABLE IF NOT EXISTS {graph_events} (
    ordinal BIGSERIAL NOT NULL,
    id UUID PRIMARY KEY,
    session_id VARCHAR(255) NOT NULL,
    event JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_{graph_events}_session FOREIGN KEY (session_id) REFERENCES {sessions}(id) ON DELETE CASCADE
);"#,
                graph_events = c.graph_events_table,
                sessions = c.sessions_table
            ),
            format!(
                r#"CREATE TABLE IF NOT EXISTS {compacts} (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id VARCHAR(255) NOT NULL,
    trigger VARCHAR(32) NOT NULL,
    pre_tokens INTEGER NOT NULL,
    post_tokens INTEGER NOT NULL,
    saved_tokens INTEGER NOT NULL,
    summary TEXT NOT NULL,
    original_count INTEGER NOT NULL,
    new_count INTEGER NOT NULL,
    logical_parent_id VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_{compacts}_session FOREIGN KEY (session_id) REFERENCES {sessions}(id) ON DELETE CASCADE
);"#,
                compacts = c.compacts_table,
                sessions = c.sessions_table
            ),
            format!(
                r#"CREATE TABLE IF NOT EXISTS {queue} (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id VARCHAR(255) NOT NULL,
    operation VARCHAR(32) NOT NULL,
    content TEXT NOT NULL,
    priority INTEGER DEFAULT 0,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    processed_at TIMESTAMPTZ,
    CONSTRAINT fk_{queue}_session FOREIGN KEY (session_id) REFERENCES {sessions}(id) ON DELETE CASCADE
);"#,
                queue = c.queue_table,
                sessions = c.sessions_table
            ),
            format!(
                r#"CREATE TABLE IF NOT EXISTS {todos} (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id VARCHAR(255) NOT NULL,
    plan_id UUID,
    content TEXT NOT NULL,
    active_form TEXT NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    CONSTRAINT fk_{todos}_session FOREIGN KEY (session_id) REFERENCES {sessions}(id) ON DELETE CASCADE
);"#,
                todos = c.todos_table,
                sessions = c.sessions_table
            ),
            format!(
                r#"CREATE TABLE IF NOT EXISTS {plans} (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id VARCHAR(255) NOT NULL,
    name VARCHAR(255),
    content TEXT NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'draft',
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    approved_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    CONSTRAINT fk_{plans}_session FOREIGN KEY (session_id) REFERENCES {sessions}(id) ON DELETE CASCADE
);"#,
                plans = c.plans_table,
                sessions = c.sessions_table
            ),
        ]
    }

    /// Generate index DDL statements.
    pub fn index_ddl(config: &PostgresConfig) -> Vec<String> {
        let c = config;
        vec![
            // Sessions indexes
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_tenant ON {0}(tenant_id)",
                c.sessions_table
            ),
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_parent ON {0}(parent_id)",
                c.sessions_table
            ),
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_expires ON {0}(expires_at) WHERE expires_at IS NOT NULL",
                c.sessions_table
            ),
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_state ON {0}(state)",
                c.sessions_table
            ),
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session_ordinal ON {0}(session_id, ordinal)",
                c.graph_events_table
            ),
            // Compacts index
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session ON {0}(session_id)",
                c.compacts_table
            ),
            // Queue indexes
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session_status ON {0}(session_id, status)",
                c.queue_table
            ),
            // Todos index
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session ON {0}(session_id)",
                c.todos_table
            ),
            // Plans index
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session ON {0}(session_id)",
                c.plans_table
            ),
        ]
    }

    /// Get expected indexes as (table_name, index_name) pairs.
    pub fn expected_indexes(config: &PostgresConfig) -> Vec<(String, String)> {
        let c = config;
        vec![
            (
                c.sessions_table.clone(),
                format!("idx_{}_tenant", c.sessions_table),
            ),
            (
                c.sessions_table.clone(),
                format!("idx_{}_parent", c.sessions_table),
            ),
            (
                c.sessions_table.clone(),
                format!("idx_{}_expires", c.sessions_table),
            ),
            (
                c.sessions_table.clone(),
                format!("idx_{}_state", c.sessions_table),
            ),
            (
                c.graph_events_table.clone(),
                format!("idx_{}_session_ordinal", c.graph_events_table),
            ),
            (
                c.compacts_table.clone(),
                format!("idx_{}_session", c.compacts_table),
            ),
            (
                c.queue_table.clone(),
                format!("idx_{}_session_status", c.queue_table),
            ),
            (
                c.todos_table.clone(),
                format!("idx_{}_session", c.todos_table),
            ),
            (
                c.plans_table.clone(),
                format!("idx_{}_session", c.plans_table),
            ),
        ]
    }

    /// Run migration to create tables and indexes.
    ///
    /// Phase D F-1: also creates the schema-version tracking
    /// table and stamps it with
    /// [`crate::session::SessionSchemaVersion::CURRENT`] on first run. Subsequent
    /// runs are idempotent — the row is upserted to the current
    /// version so a fresh binary can re-mark the database as
    /// owned by its layout.
    pub async fn migrate(pool: &PgPool, config: &PostgresConfig) -> Result<(), sqlx::Error> {
        // Create the version-tracking table first so a migration
        // failure later still leaves the database in a
        // self-describing state.
        sqlx::query(&Self::schema_version_table_ddl(config))
            .execute(pool)
            .await?;

        for table_ddl in Self::table_ddl(config) {
            sqlx::query(&table_ddl).execute(pool).await?;
        }

        for index_ddl in Self::index_ddl(config) {
            sqlx::query(&index_ddl).execute(pool).await?;
        }

        // Stamp the current version. Uses ON CONFLICT DO UPDATE
        // so re-running migrate on an older DB brings the stamp
        // forward; the calling side is expected to have already
        // checked `verify_schema_version` and decided the upgrade
        // is safe.
        let upsert = format!(
            r#"INSERT INTO {table} (component, version) VALUES ($1, $2)
               ON CONFLICT (component) DO UPDATE SET version = EXCLUDED.version, applied_at = NOW()"#,
            table = config.schema_version_table
        );
        sqlx::query(&upsert)
            .bind("session")
            .bind(crate::session::SessionSchemaVersion::CURRENT.value() as i64)
            .execute(pool)
            .await?;

        Ok(())
    }

    /// Pure helper: translate a raw stamped version value from
    /// the `schema_version` table into a typed result. Separated
    /// from the SQL round-trip so unit tests can cover the
    /// window-checking logic without a live database.
    pub fn interpret_stamped_version(
        raw: Option<i64>,
    ) -> Result<Option<crate::session::SessionSchemaVersion>, SessionError> {
        let Some(raw) = raw else {
            return Ok(None);
        };
        let version = crate::session::SessionSchemaVersion(raw as u32);
        if version.is_from_the_future() {
            return Err(SessionError::SchemaVersionMismatch {
                component: "postgres",
                found: version,
                expected: crate::session::SessionSchemaVersion::CURRENT,
                direction: crate::session::SchemaVersionMismatchDirection::TooNew,
            });
        }
        if !version.is_supported() {
            return Err(SessionError::SchemaVersionMismatch {
                component: "postgres",
                found: version,
                expected: crate::session::SessionSchemaVersion::CURRENT,
                direction: crate::session::SchemaVersionMismatchDirection::TooOld,
            });
        }
        Ok(Some(version))
    }

    /// Phase D F-1: read the stamped schema version and compare
    /// it against [`crate::session::SessionSchemaVersion::CURRENT`]. Returns:
    ///
    /// - `Ok(None)` when the database has not yet been
    ///   migrated — the version row is absent, which is the
    ///   expected state for a pristine deployment.
    /// - `Ok(Some(version))` when the stamped version is
    ///   inside the `[MIN_SUPPORTED, CURRENT]` window.
    /// - `Err(SessionError::SchemaVersionMismatch)` when the
    ///   stamped version is outside that window — either too
    ///   old for the built-in migration ladder or too new for
    ///   the running binary.
    pub async fn verify_schema_version(
        pool: &PgPool,
        config: &PostgresConfig,
    ) -> Result<Option<crate::session::SessionSchemaVersion>, SessionError> {
        let query = format!(
            "SELECT version FROM {table} WHERE component = $1",
            table = config.schema_version_table
        );
        let row: Option<i64> = sqlx::query_scalar(&query)
            .bind("session")
            .fetch_optional(pool)
            .await
            .map_err(|e| SessionError::Storage {
                message: format!("schema_version lookup failed: {e}"),
            })?;
        Self::interpret_stamped_version(row)
    }

    /// Verify schema integrity - check tables and indexes exist.
    pub async fn verify(
        pool: &PgPool,
        config: &PostgresConfig,
    ) -> Result<Vec<SchemaIssue>, sqlx::Error> {
        let mut issues = Vec::new();

        // Check tables
        for table in config.table_names() {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = $1)",
            )
            .bind(table)
            .fetch_one(pool)
            .await?;

            if !exists {
                issues.push(SchemaIssue::MissingTable(table.to_string()));
            }
        }

        // Check indexes
        for (table, index) in Self::expected_indexes(config) {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE tablename = $1 AND indexname = $2)",
            )
            .bind(&table)
            .bind(&index)
            .fetch_one(pool)
            .await?;

            if !exists {
                issues.push(SchemaIssue::MissingIndex { table, index });
            }
        }

        Ok(issues)
    }
}

// ============================================================================
// Persistence Implementation
// ============================================================================

/// PostgreSQL session persistence.
pub struct PostgresPersistence {
    pool: Arc<PgPool>,
    config: PostgresConfig,
}

impl PostgresPersistence {
    /// Connect to database without running migrations.
    ///
    /// Use this when managing schema externally (production deployments).
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        Self::connect_with_config(database_url, PostgresConfig::default()).await
    }

    /// Connect with custom configuration, without running migrations.
    pub async fn connect_with_config(
        database_url: &str,
        config: PostgresConfig,
    ) -> Result<Self, sqlx::Error> {
        let pool = config.pool.apply().connect(database_url).await?;
        Ok(Self {
            pool: Arc::new(pool),
            config,
        })
    }

    /// Connect and run migrations automatically.
    ///
    /// Convenient for development and simple deployments.
    pub async fn connect_and_migrate(database_url: &str) -> Result<Self, sqlx::Error> {
        Self::connect_and_migrate_with_config(database_url, PostgresConfig::default()).await
    }

    /// Connect with custom configuration and run migrations.
    pub async fn connect_and_migrate_with_config(
        database_url: &str,
        config: PostgresConfig,
    ) -> Result<Self, sqlx::Error> {
        let persistence = Self::connect_with_config(database_url, config).await?;
        persistence.migrate().await?;
        Ok(persistence)
    }

    /// Use an existing pool without running migrations.
    pub fn from_pool(pool: Arc<PgPool>) -> Self {
        Self::pool_and_config(pool, PostgresConfig::default())
    }

    /// Use an existing pool with custom configuration.
    pub fn pool_and_config(pool: Arc<PgPool>, config: PostgresConfig) -> Self {
        Self { pool, config }
    }

    /// Run schema migration.
    pub async fn migrate(&self) -> Result<(), sqlx::Error> {
        PostgresSchema::migrate(&self.pool, &self.config).await
    }

    /// Verify schema integrity.
    pub async fn verify_schema(&self) -> Result<Vec<SchemaIssue>, sqlx::Error> {
        PostgresSchema::verify(&self.pool, &self.config).await
    }

    /// Phase D F-1: verify the stamped session-schema version on
    /// this database matches the running binary. Operators running
    /// the `connect` (no-auto-migrate) path call this immediately
    /// after construction so a binary that doesn't know the on-disk
    /// schema fails loudly instead of silently mis-reading rows.
    /// The `connect_and_migrate` path is already self-consistent —
    /// `migrate` stamps the current version — so callers there do
    /// not need to call this.
    pub async fn verify_schema_version(
        &self,
    ) -> Result<Option<crate::session::SessionSchemaVersion>, SessionError> {
        PostgresSchema::verify_schema_version(&self.pool, &self.config).await
    }

    /// Get the underlying connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Get the configuration.
    pub fn config(&self) -> &PostgresConfig {
        &self.config
    }

    // ========================================================================
    // Internal helpers
    // ========================================================================

    async fn with_retry<F, Fut, T>(&self, operation: F) -> SessionResult<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = SessionResult<T>>,
    {
        super::with_retry(
            self.config.pool.max_retries,
            self.config.pool.initial_backoff,
            self.config.pool.max_backoff,
            Self::is_retryable,
            operation,
        )
        .await
    }

    fn is_retryable(error: &SessionError) -> bool {
        match error {
            SessionError::Storage { message } => {
                message.contains("connection")
                    || message.contains("timeout")
                    || message.contains("reset")
                    || message.contains("broken pipe")
                    || message.contains("serialization")
                    || message.contains("deadlock")
                    || message.contains("could not serialize")
            }
            _ => false,
        }
    }

    async fn load_session_row(&self, session_id: &SessionId) -> SessionResult<Session> {
        let c = &self.config;
        let id_str = session_id.to_string();

        let row = sqlx::query(&format!(
            r#"
            SELECT id, parent_id, tenant_id, principal_id, session_type, state, mode,
                   config, authorization_policy, summary,
                   total_input_tokens, total_output_tokens, total_cost_usd,
                   current_leaf_id, primary_branch_id, static_context_hash, error,
                   created_at, updated_at, expires_at
            FROM {sessions}
            WHERE id = $1
            "#,
            sessions = c.sessions_table
        ))
        .bind(&id_str)
        .fetch_optional(self.pool.as_ref())
        .await
        .storage_err()?
        .ok_or_else(|| SessionError::NotFound { id: id_str.clone() })?;

        let graph_events = self.load_graph_events(session_id).await?;
        let compacts = self.load_compacts(session_id).await?;
        let todos = self.load_todos_internal(session_id).await?;
        let plan = self.load_plan_internal(session_id).await?;

        reconstruct_session_from_row(session_id, row, graph_events, compacts, todos, plan)
    }

    async fn load_graph_events(
        &self,
        session_id: &SessionId,
    ) -> SessionResult<Vec<crate::graph::GraphEvent>> {
        let c = &self.config;

        let rows = sqlx::query(&format!(
            r#"
            SELECT event
            FROM {graph_events}
            WHERE session_id = $1
            ORDER BY ordinal ASC
            "#,
            graph_events = c.graph_events_table
        ))
        .bind(session_id.to_string())
        .fetch_all(self.pool.as_ref())
        .await
        .storage_err()?;

        Ok(parse_graph_event_rows(rows))
    }

    async fn load_compacts(&self, session_id: &SessionId) -> SessionResult<Vec<CompactRecord>> {
        let c = &self.config;

        let rows = sqlx::query(&format!(
            r#"
            SELECT id, session_id, trigger, pre_tokens, post_tokens, saved_tokens,
                   summary, original_count, new_count, logical_parent_id, created_at
            FROM {compacts}
            WHERE session_id = $1
            ORDER BY created_at ASC
            "#,
            compacts = c.compacts_table
        ))
        .bind(session_id.to_string())
        .fetch_all(self.pool.as_ref())
        .await
        .storage_err()?;

        Ok(parse_compact_rows(session_id, rows))
    }

    async fn load_todos_internal(&self, session_id: &SessionId) -> SessionResult<Vec<TodoItem>> {
        let c = &self.config;

        let rows = sqlx::query(&format!(
            r#"
            SELECT id, session_id, plan_id, content, active_form, status,
                   created_at, started_at, completed_at
            FROM {todos}
            WHERE session_id = $1
            ORDER BY created_at ASC
            "#,
            todos = c.todos_table
        ))
        .bind(session_id.to_string())
        .fetch_all(self.pool.as_ref())
        .await
        .storage_err()?;

        Ok(parse_todo_rows(session_id, rows))
    }

    async fn load_plan_internal(&self, session_id: &SessionId) -> SessionResult<Option<Plan>> {
        let c = &self.config;

        let row = sqlx::query(&format!(
            r#"
            SELECT id, session_id, name, content, status, error,
                   created_at, approved_at, started_at, completed_at
            FROM {plans}
            WHERE session_id = $1
            ORDER BY created_at DESC
            LIMIT 1
            "#,
            plans = c.plans_table
        ))
        .bind(session_id.to_string())
        .fetch_optional(self.pool.as_ref())
        .await
        .storage_err()?;

        Ok(parse_plan_row(session_id, row))
    }

    async fn load_graph_events_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
    ) -> SessionResult<Vec<crate::graph::GraphEvent>> {
        let c = &self.config;

        let rows = sqlx::query(&format!(
            r#"
            SELECT event
            FROM {graph_events}
            WHERE session_id = $1
            ORDER BY ordinal ASC
            "#,
            graph_events = c.graph_events_table
        ))
        .bind(session_id.to_string())
        .fetch_all(&mut **tx)
        .await
        .storage_err()?;

        Ok(parse_graph_event_rows(rows))
    }

    async fn load_compacts_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
    ) -> SessionResult<Vec<CompactRecord>> {
        let c = &self.config;

        let rows = sqlx::query(&format!(
            r#"
            SELECT id, session_id, trigger, pre_tokens, post_tokens, saved_tokens,
                   summary, original_count, new_count, logical_parent_id, created_at
            FROM {compacts}
            WHERE session_id = $1
            ORDER BY created_at ASC
            "#,
            compacts = c.compacts_table
        ))
        .bind(session_id.to_string())
        .fetch_all(&mut **tx)
        .await
        .storage_err()?;

        Ok(parse_compact_rows(session_id, rows))
    }

    async fn load_todos_internal_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
    ) -> SessionResult<Vec<TodoItem>> {
        let c = &self.config;

        let rows = sqlx::query(&format!(
            r#"
            SELECT id, session_id, plan_id, content, active_form, status,
                   created_at, started_at, completed_at
            FROM {todos}
            WHERE session_id = $1
            ORDER BY created_at ASC
            "#,
            todos = c.todos_table
        ))
        .bind(session_id.to_string())
        .fetch_all(&mut **tx)
        .await
        .storage_err()?;

        Ok(parse_todo_rows(session_id, rows))
    }

    async fn load_plan_internal_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
    ) -> SessionResult<Option<Plan>> {
        let c = &self.config;

        let row = sqlx::query(&format!(
            r#"
            SELECT id, session_id, name, content, status, error,
                   created_at, approved_at, started_at, completed_at
            FROM {plans}
            WHERE session_id = $1
            ORDER BY created_at DESC
            LIMIT 1
            "#,
            plans = c.plans_table
        ))
        .bind(session_id.to_string())
        .fetch_optional(&mut **tx)
        .await
        .storage_err()?;

        Ok(parse_plan_row(session_id, row))
    }

    async fn pending_queue_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
    ) -> SessionResult<Vec<QueueItem>> {
        let c = &self.config;

        let rows = sqlx::query(&format!(
            r#"
            SELECT id, session_id, operation, content, priority, status, created_at, processed_at
            FROM {queue}
            WHERE session_id = $1 AND status = 'pending'
            ORDER BY priority DESC, created_at ASC
            "#,
            queue = c.queue_table
        ))
        .bind(session_id.to_string())
        .fetch_all(&mut **tx)
        .await
        .storage_err()?;

        Ok(parse_pending_queue_rows(session_id, rows))
    }

    async fn load_session_row_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
    ) -> SessionResult<Session> {
        let c = &self.config;
        let id_str = session_id.to_string();

        let row = sqlx::query(&format!(
            r#"
            SELECT id, parent_id, tenant_id, principal_id, session_type, state, mode,
                   config, authorization_policy, summary,
                   total_input_tokens, total_output_tokens, total_cost_usd,
                   current_leaf_id, primary_branch_id, static_context_hash, error,
                   created_at, updated_at, expires_at
            FROM {sessions}
            WHERE id = $1
            "#,
            sessions = c.sessions_table
        ))
        .bind(&id_str)
        .fetch_optional(&mut **tx)
        .await
        .storage_err()?
        .ok_or_else(|| SessionError::NotFound { id: id_str.clone() })?;

        let graph_events = self.load_graph_events_tx(tx, session_id).await?;
        let compacts = self.load_compacts_tx(tx, session_id).await?;
        let todos = self.load_todos_internal_tx(tx, session_id).await?;
        let plan = self.load_plan_internal_tx(tx, session_id).await?;

        reconstruct_session_from_row(session_id, row, graph_events, compacts, todos, plan)
    }

    async fn save_todos_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
        todos: &[TodoItem],
    ) -> SessionResult<()> {
        let c = &self.config;
        let todo_ids: Vec<Uuid> = todos.iter().map(|todo| todo.id).collect();

        for todo in todos {
            let status = enum_to_db(&todo.status, "pending");

            sqlx::query(&format!(
                r#"
                INSERT INTO {todos} (
                    id, session_id, plan_id, content, active_form, status,
                    created_at, started_at, completed_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (id) DO UPDATE SET
                    plan_id = EXCLUDED.plan_id,
                    content = EXCLUDED.content,
                    active_form = EXCLUDED.active_form,
                    status = EXCLUDED.status,
                    started_at = EXCLUDED.started_at,
                    completed_at = EXCLUDED.completed_at
                "#,
                todos = c.todos_table
            ))
            .bind(todo.id)
            .bind(session_id.to_string())
            .bind(todo.plan_id)
            .bind(&todo.content)
            .bind(&todo.active_form)
            .bind(&status)
            .bind(todo.created_at)
            .bind(todo.started_at)
            .bind(todo.completed_at)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        if todo_ids.is_empty() {
            sqlx::query(&format!(
                "DELETE FROM {todos} WHERE session_id = $1",
                todos = c.todos_table
            ))
            .bind(session_id.to_string())
            .execute(&mut **tx)
            .await
            .storage_err()?;
        } else {
            sqlx::query(&format!(
                "DELETE FROM {todos} WHERE session_id = $1 AND NOT (id = ANY($2))",
                todos = c.todos_table
            ))
            .bind(session_id.to_string())
            .bind(&todo_ids)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        Ok(())
    }

    async fn save_plan_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        plan: &Plan,
    ) -> SessionResult<()> {
        let c = &self.config;

        let status = enum_to_db(&plan.state, "draft");

        sqlx::query(&format!(
            r#"
            INSERT INTO {plans} (
                id, session_id, name, content, status, error,
                created_at, approved_at, started_at, completed_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (id) DO UPDATE SET
                name = EXCLUDED.name,
                content = EXCLUDED.content,
                status = EXCLUDED.status,
                error = EXCLUDED.error,
                approved_at = EXCLUDED.approved_at,
                started_at = EXCLUDED.started_at,
                completed_at = EXCLUDED.completed_at
            "#,
            plans = c.plans_table
        ))
        .bind(plan.id)
        .bind(plan.session_id.to_string())
        .bind(&plan.name)
        .bind(&plan.content)
        .bind(&status)
        .bind(&plan.error)
        .bind(plan.created_at)
        .bind(plan.approved_at)
        .bind(plan.started_at)
        .bind(plan.completed_at)
        .execute(&mut **tx)
        .await
        .storage_err()?;

        sqlx::query(&format!(
            "DELETE FROM {plans} WHERE session_id = $1 AND id <> $2",
            plans = c.plans_table
        ))
        .bind(plan.session_id.to_string())
        .bind(plan.id)
        .execute(&mut **tx)
        .await
        .storage_err()?;

        Ok(())
    }

    async fn delete_plan_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
    ) -> SessionResult<()> {
        let c = &self.config;

        sqlx::query(&format!(
            "DELETE FROM {plans} WHERE session_id = $1",
            plans = c.plans_table
        ))
        .bind(session_id.to_string())
        .execute(&mut **tx)
        .await
        .storage_err()?;

        Ok(())
    }

    async fn save_compacts_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
        compacts: &VecDeque<CompactRecord>,
    ) -> SessionResult<()> {
        let c = &self.config;
        let compact_ids: Vec<Uuid> = compacts.iter().map(|compact| compact.id).collect();

        if compact_ids.is_empty() {
            sqlx::query(&format!(
                "DELETE FROM {compacts} WHERE session_id = $1",
                compacts = c.compacts_table
            ))
            .bind(session_id.to_string())
            .execute(&mut **tx)
            .await
            .storage_err()?;
        } else {
            sqlx::query(&format!(
                "DELETE FROM {compacts} WHERE session_id = $1 AND NOT (id = ANY($2))",
                compacts = c.compacts_table
            ))
            .bind(session_id.to_string())
            .bind(&compact_ids)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        for compact in compacts {
            let trigger = enum_to_db(&compact.trigger, "manual");

            sqlx::query(&format!(
                r#"
                INSERT INTO {compacts} (
                    id, session_id, trigger, pre_tokens, post_tokens, saved_tokens,
                    summary, original_count, new_count, logical_parent_id, created_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                ON CONFLICT (id) DO UPDATE SET
                    session_id = EXCLUDED.session_id,
                    trigger = EXCLUDED.trigger,
                    pre_tokens = EXCLUDED.pre_tokens,
                    post_tokens = EXCLUDED.post_tokens,
                    saved_tokens = EXCLUDED.saved_tokens,
                    summary = EXCLUDED.summary,
                    original_count = EXCLUDED.original_count,
                    new_count = EXCLUDED.new_count,
                    logical_parent_id = EXCLUDED.logical_parent_id,
                    created_at = EXCLUDED.created_at
                "#,
                compacts = c.compacts_table
            ))
            .bind(compact.id)
            .bind(session_id.to_string())
            .bind(&trigger)
            .bind(compact.pre_tokens.get() as i32)
            .bind(compact.post_tokens.get() as i32)
            .bind(compact.saved_tokens.get() as i32)
            .bind(&compact.summary)
            .bind(compact.original_count as i32)
            .bind(compact.new_count as i32)
            .bind(compact.logical_parent_id.as_ref().map(|id| id.to_string()))
            .bind(compact.created_at)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        Ok(())
    }

    async fn replace_pending_queue_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
        items: &[QueueItem],
    ) -> SessionResult<()> {
        let c = &self.config;

        sqlx::query(&format!(
            "DELETE FROM {queue} WHERE session_id = $1",
            queue = c.queue_table
        ))
        .bind(session_id.to_string())
        .execute(&mut **tx)
        .await
        .storage_err()?;

        for item in items {
            sqlx::query(&format!(
                r#"
                INSERT INTO {queue}
                    (id, session_id, operation, content, priority, status, created_at, processed_at)
                VALUES
                    ($1, $2, $3, $4, $5, $6, $7, $8)
                "#,
                queue = c.queue_table
            ))
            .bind(item.id)
            .bind(session_id.to_string())
            .bind("enqueue")
            .bind(&item.content)
            .bind(item.priority)
            .bind("pending")
            .bind(item.created_at)
            .bind(Option::<chrono::DateTime<Utc>>::None)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        Ok(())
    }

    async fn save_graph_events_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
        events: &[GraphEvent],
    ) -> SessionResult<()> {
        let c = &self.config;

        for event in events {
            sqlx::query(&format!(
                r#"
                INSERT INTO {graph_events} (id, session_id, event)
                VALUES ($1, $2, $3)
                ON CONFLICT (id) DO NOTHING
                "#,
                graph_events = c.graph_events_table
            ))
            .bind(event.metadata.id)
            .bind(session_id.to_string())
            .bind(serde_json::to_value(event).unwrap_or_else(|_| serde_json::json!({})))
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        Ok(())
    }

    /// Phase G-1: persist a full session snapshot into the supplied
    /// transaction without committing. Extracted from `save_inner` so
    /// [`with_session_lock`] can run load/f/save inside a single
    /// transaction while retaining `save_inner`'s wrap-and-commit
    /// shape for the simple save path.
    async fn save_session_into_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        session: &Session,
    ) -> SessionResult<()> {
        let c = &self.config;
        let summary_cache = session.graph.latest_summary();

        let session_type = enum_to_db(&session.session_type, "main");
        let state = enum_to_db(&session.state, "created");
        let mode = "stateless";

        sqlx::query(&format!(
            r#"
            INSERT INTO {sessions} (
                id, parent_id, tenant_id, principal_id, session_type, state, mode,
                config, authorization_policy, summary,
                total_input_tokens, total_output_tokens, total_cost_usd,
                current_leaf_id, primary_branch_id, static_context_hash, error,
                created_at, updated_at, expires_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
            )
            ON CONFLICT (id) DO UPDATE SET
                parent_id = EXCLUDED.parent_id,
                tenant_id = EXCLUDED.tenant_id,
                principal_id = EXCLUDED.principal_id,
                session_type = EXCLUDED.session_type,
                state = EXCLUDED.state,
                mode = EXCLUDED.mode,
                config = EXCLUDED.config,
                authorization_policy = EXCLUDED.authorization_policy,
                summary = EXCLUDED.summary,
                total_input_tokens = EXCLUDED.total_input_tokens,
                total_output_tokens = EXCLUDED.total_output_tokens,
                total_cost_usd = EXCLUDED.total_cost_usd,
                current_leaf_id = EXCLUDED.current_leaf_id,
                primary_branch_id = EXCLUDED.primary_branch_id,
                static_context_hash = EXCLUDED.static_context_hash,
                error = EXCLUDED.error,
                updated_at = EXCLUDED.updated_at,
                expires_at = EXCLUDED.expires_at
            "#,
            sessions = c.sessions_table
        ))
        .bind(session.id.to_string())
        .bind(session.parent_id.map(|id| id.to_string()))
        .bind(&session.tenant_id)
        .bind(&session.principal_id)
        .bind(&session_type)
        .bind(&state)
        .bind(mode)
        .bind(serde_json::to_value(&session.config).unwrap_or_else(|e| {
            tracing::warn!(session_id = %session.id, error = %e, "Failed to serialize session config");
            serde_json::Value::Object(Default::default())
        }))
        .bind(serde_json::to_value(&session.authorization).unwrap_or_else(|e| {
            tracing::warn!(session_id = %session.id, error = %e, "Failed to serialize session authorization");
            serde_json::Value::Object(Default::default())
        }))
        .bind(&summary_cache)
        .bind(session.total_usage.input_tokens as i64)
        .bind(session.total_usage.output_tokens as i64)
        .bind(session.total_cost_usd)
        .bind(session.current_leaf_id().map(|id| id.to_string()))
        .bind(session.graph.primary_branch.as_uuid())
        .bind(&session.static_context_hash)
        .bind(&session.error)
        .bind(session.created_at)
        .bind(session.updated_at)
        .bind(session.expires_at)
        .execute(&mut **tx)
        .await
        .storage_err()?;

        self.save_graph_events_tx(tx, &session.id, &session.graph.events)
            .await?;
        self.save_todos_tx(tx, &session.id, &session.todos).await?;
        self.save_compacts_tx(tx, &session.id, &session.compact_history)
            .await?;

        if let Some(ref plan) = session.current_plan {
            self.save_plan_tx(tx, plan).await?;
        } else {
            self.delete_plan_tx(tx, &session.id).await?;
        }

        Ok(())
    }

    async fn save_inner(&self, session: &Session) -> SessionResult<()> {
        let mut tx = self.pool.begin().await.storage_err()?;
        self.save_session_into_tx(&mut tx, session).await?;
        tx.commit().await.storage_err()?;
        Ok(())
    }

    async fn restore_bundle_inner(
        &self,
        session: &Session,
        pending_queue: &[QueueItem],
    ) -> SessionResult<()> {
        let c = &self.config;
        let summary_cache = session.graph.latest_summary();
        let normalized_queue: Vec<QueueItem> = pending_queue
            .iter()
            .cloned()
            .map(|mut item| {
                item.session_id = session.id;
                item.state = QueueItemState::Pending;
                item.processed_at = None;
                item
            })
            .collect();

        let mut tx = self.pool.begin().await.storage_err()?;

        let session_type = enum_to_db(&session.session_type, "main");
        let state = enum_to_db(&session.state, "created");
        let mode = "stateless";

        let insert_result = sqlx::query(&format!(
            r#"
            INSERT INTO {sessions} (
                id, parent_id, tenant_id, principal_id, session_type, state, mode,
                config, authorization_policy, summary,
                total_input_tokens, total_output_tokens, total_cost_usd,
                current_leaf_id, primary_branch_id, static_context_hash, error,
                created_at, updated_at, expires_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
            )
            ON CONFLICT (id) DO NOTHING
            "#,
            sessions = c.sessions_table
        ))
        .bind(session.id.to_string())
        .bind(session.parent_id.map(|id| id.to_string()))
        .bind(&session.tenant_id)
        .bind(&session.principal_id)
        .bind(&session_type)
        .bind(&state)
        .bind(mode)
        .bind(serde_json::to_value(&session.config).unwrap_or_else(|e| {
            tracing::warn!(session_id = %session.id, error = %e, "Failed to serialize session config");
            serde_json::Value::Object(Default::default())
        }))
        .bind(serde_json::to_value(&session.authorization).unwrap_or_else(|e| {
            tracing::warn!(session_id = %session.id, error = %e, "Failed to serialize session authorization");
            serde_json::Value::Object(Default::default())
        }))
        .bind(&summary_cache)
        .bind(session.total_usage.input_tokens as i64)
        .bind(session.total_usage.output_tokens as i64)
        .bind(session.total_cost_usd)
        .bind(session.current_leaf_id().map(|id| id.to_string()))
        .bind(session.graph.primary_branch.as_uuid())
        .bind(&session.static_context_hash)
        .bind(&session.error)
        .bind(session.created_at)
        .bind(session.updated_at)
        .bind(session.expires_at)
        .execute(&mut *tx)
        .await
        .storage_err()?;

        if insert_result.rows_affected() == 0 {
            return Err(SessionError::Storage {
                message: format!(
                    "Archive restore refuses to overwrite existing session {}",
                    session.id
                ),
            });
        }

        self.save_graph_events_tx(&mut tx, &session.id, &session.graph.events)
            .await?;
        self.save_todos_tx(&mut tx, &session.id, &session.todos)
            .await?;
        self.save_compacts_tx(&mut tx, &session.id, &session.compact_history)
            .await?;
        if let Some(plan) = &session.current_plan {
            self.save_plan_tx(&mut tx, plan).await?;
        } else {
            self.delete_plan_tx(&mut tx, &session.id).await?;
        }
        self.replace_pending_queue_tx(&mut tx, &session.id, &normalized_queue)
            .await?;

        let restored = self.load_session_row_tx(&mut tx, &session.id).await?;
        let restored_queue = self.pending_queue_tx(&mut tx, &session.id).await?;
        verify_restored_session_roundtrip(session, &normalized_queue, &restored, &restored_queue)?;

        tx.commit().await.storage_err()?;
        Ok(())
    }
}

#[async_trait]
impl Persistence for PostgresPersistence {
    fn name(&self) -> &str {
        "postgres"
    }

    async fn save(&self, session: &Session) -> SessionResult<()> {
        validate_graph(&session.id, &session.graph)?;
        self.with_retry(|| self.save_inner(session)).await
    }

    /// Phase G-1: one-shot mutation under a Postgres transaction-scoped
    /// advisory lock keyed off the session UUID. `pg_advisory_xact_lock`
    /// is released automatically on commit or rollback, so we never
    /// leak a lock even if the caller's closure panics.
    ///
    /// The lock key is derived by folding the session UUID's 128 bits
    /// into an i64. Collision probability across the lifetime of a
    /// single database is negligible for the expected session count
    /// (< 2^32 active sessions); a collision would only serialize
    /// two unrelated sessions briefly, never corrupt data.
    async fn with_session_lock(&self, id: &SessionId, f: SessionMutationFn) -> SessionResult<()> {
        let sid = *id;
        let lock_key = session_advisory_lock_key(&sid);
        let mut tx = self.pool.begin().await.storage_err()?;

        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock_key)
            .execute(&mut *tx)
            .await
            .storage_err()?;

        let mut session = self.load_session_row_tx(&mut tx, &sid).await?;
        f(&mut session)?;
        validate_graph(&session.id, &session.graph)?;
        self.save_session_into_tx(&mut tx, &session).await?;
        tx.commit().await.storage_err()?;
        Ok(())
    }

    async fn load(&self, id: &SessionId) -> SessionResult<Option<Session>> {
        self.with_retry(|| async {
            match self.load_session_row(id).await {
                Ok(session) => Ok(Some(session)),
                Err(SessionError::NotFound { .. }) => Ok(None),
                Err(e) => Err(e),
            }
        })
        .await
    }

    async fn delete(&self, id: &SessionId) -> SessionResult<bool> {
        let sid = *id;
        self.with_retry(|| async move {
            let c = &self.config;

            let result = sqlx::query(&format!(
                "DELETE FROM {sessions} WHERE id = $1",
                sessions = c.sessions_table
            ))
            .bind(sid.to_string())
            .execute(self.pool.as_ref())
            .await
            .storage_err()?;

            Ok(result.rows_affected() > 0)
        })
        .await
    }

    async fn list(&self, tenant_id: Option<&str>) -> SessionResult<Vec<SessionId>> {
        let owned_tid = tenant_id.map(|s| s.to_string());
        self.with_retry(|| {
            let tid = owned_tid.clone();
            async move {
                let c = &self.config;

                let rows = if let Some(ref tid) = tid {
                    sqlx::query(&format!(
                        "SELECT id FROM {sessions} WHERE tenant_id = $1",
                        sessions = c.sessions_table
                    ))
                    .bind(tid.as_str())
                    .fetch_all(self.pool.as_ref())
                    .await
                } else {
                    sqlx::query(&format!(
                        "SELECT id FROM {sessions}",
                        sessions = c.sessions_table
                    ))
                    .fetch_all(self.pool.as_ref())
                    .await
                }
                .storage_err()?;

                let mut ids = Vec::with_capacity(rows.len());

                for row in rows {
                    let id_str = match row.try_get::<&str, _>("id") {
                        Ok(id) => id,
                        Err(e) => {
                            tracing::warn!(error = %e, "Skipping session row: failed to get id");
                            continue;
                        }
                    };

                    match id_str.parse() {
                        Ok(id) => ids.push(id),
                        Err(e) => {
                            tracing::warn!(id = id_str, error = %e, "Skipping session row: failed to parse id");
                        }
                    }
                }

                Ok(ids)
            }
        })
        .await
    }

    async fn list_children(&self, parent_id: &SessionId) -> SessionResult<Vec<SessionId>> {
        let pid = *parent_id;
        self.with_retry(|| async move {
            let c = &self.config;
            let rows = sqlx::query(&format!(
                "SELECT id FROM {sessions} WHERE parent_id = $1",
                sessions = c.sessions_table
            ))
            .bind(pid.to_string())
            .fetch_all(self.pool.as_ref())
            .await
            .storage_err()?;

            let mut ids = Vec::with_capacity(rows.len());
            for row in rows {
                let id_str = match row.try_get::<&str, _>("id") {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!(error = %e, "Skipping child session row: failed to get id");
                        continue;
                    }
                };

                match id_str.parse() {
                    Ok(id) => ids.push(id),
                    Err(e) => {
                        tracing::warn!(id = id_str, error = %e, "Skipping child session row: failed to parse id");
                    }
                }
            }

            Ok(ids)
        })
        .await
    }

    async fn append_graph_event(
        &self,
        session_id: &SessionId,
        event: GraphEvent,
    ) -> SessionResult<()> {
        let sid = *session_id;
        let event_template = event.clone();
        self.with_retry(|| {
            let event = event_template.clone();
            async move {
            let c = &self.config;
            let mut tx = self.pool.begin().await.storage_err()?;

            let exists: Option<String> = sqlx::query_scalar(&format!(
                "SELECT id FROM {sessions} WHERE id = $1 FOR UPDATE",
                sessions = c.sessions_table
            ))
            .bind(sid.to_string())
            .fetch_optional(&mut *tx)
            .await
            .storage_err()?;

            if exists.is_none() {
                return Err(SessionError::NotFound { id: sid.to_string() });
            }

            sqlx::query(&format!(
                r#"
                INSERT INTO {graph_events} (id, session_id, event)
                VALUES ($1, $2, $3)
                ON CONFLICT (id) DO NOTHING
                "#,
                graph_events = c.graph_events_table
            ))
            .bind(event.metadata.id)
            .bind(sid.to_string())
            .bind(serde_json::to_value(&event).unwrap_or_else(|_| serde_json::json!({})))
            .execute(&mut *tx)
            .await
            .storage_err()?;

            let current_leaf_id = match &event.body {
                GraphEventBody::NodeAppended { node_id, .. } => Some(node_id.to_string()),
                GraphEventBody::CheckpointCreated { checkpoint_id, .. } => {
                    Some(checkpoint_id.to_string())
                }
                GraphEventBody::NodeMetadataPatched { .. } => None,
                _ => None,
            };
            let summary_cache = match &event.body {
                GraphEventBody::NodeAppended {
                    kind: crate::graph::NodeKind::Summary,
                    payload,
                    ..
                } => payload
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                GraphEventBody::NodeMetadataPatched { .. } => None,
                _ => None,
            };

            sqlx::query(&format!(
                "UPDATE {sessions} SET current_leaf_id = COALESCE($2, current_leaf_id), summary = COALESCE($3, summary), updated_at = $4 WHERE id = $1",
                sessions = c.sessions_table
            ))
            .bind(sid.to_string())
            .bind(current_leaf_id)
            .bind(summary_cache)
            .bind(event.metadata.occurred_at)
            .execute(&mut *tx)
            .await
            .storage_err()?;

            tx.commit().await.storage_err()?;
            Ok(())
        }
        })
        .await
    }

    async fn add_message(
        &self,
        session_id: &SessionId,
        message: SessionMessage,
    ) -> SessionResult<()> {
        let sid = *session_id;
        let message_template = message.clone();
        self.with_retry(|| {
            let mut message = message_template.clone();
            async move {
                let c = &self.config;
                let mut tx = self.pool.begin().await.storage_err()?;

                let row = sqlx::query(&format!(
                    r#"
                SELECT session_type, principal_id, current_leaf_id
                     , primary_branch_id
                FROM {sessions}
                WHERE id = $1
                FOR UPDATE
                "#,
                    sessions = c.sessions_table
                ))
                .bind(sid.to_string())
                .fetch_optional(&mut *tx)
                .await
                .storage_err()?
                .ok_or_else(|| SessionError::NotFound {
                    id: sid.to_string(),
                })?;

                let session_type = row
                    .try_get::<&str, _>("session_type")
                    .ok()
                    .map(db_to_session_type)
                    .unwrap_or_default();
                let principal_id: Option<String> = row.try_get("principal_id").ok();
                let current_leaf_id: Option<String> = row.try_get("current_leaf_id").ok();
                let primary_branch_id: crate::graph::BranchId = {
                    let raw: Uuid =
                        row.try_get("primary_branch_id")
                            .map_err(|e| SessionError::Storage {
                                message: format!(
                                    "Session {} is missing required primary_branch_id: {}",
                                    sid, e
                                ),
                            })?;
                    crate::graph::BranchId::from_uuid(raw)
                };

                if let Some(parent_id) = current_leaf_id {
                    message.parent_id = Some(crate::session::MessageId::from_string(parent_id));
                }

                let node_id = graph_node_id_for_message(&message)?;
                let parent_id = graph_parent_node_id_for_message(&message)?;
                let event = GraphEvent {
                    metadata: crate::graph::EventMetadata {
                        id: Uuid::new_v4(),
                        occurred_at: message.timestamp,
                        actor: principal_id.clone(),
                    },
                    body: GraphEventBody::NodeAppended {
                        node_id,
                        branch_id: primary_branch_id,
                        parent_id,
                        kind: graph_node_kind_for_message(&message),
                        tags: graph_tags_for_message(&message),
                        payload: graph_payload_for_message(&message),
                        provenance: build_graph_provenance(sid, &session_type),
                    },
                };

                sqlx::query(&format!(
                    r#"
                INSERT INTO {graph_events} (id, session_id, event)
                VALUES ($1, $2, $3)
                ON CONFLICT (id) DO NOTHING
                "#,
                    graph_events = c.graph_events_table
                ))
                .bind(event.metadata.id)
                .bind(sid.to_string())
                .bind(serde_json::to_value(&event).unwrap_or_else(|_| serde_json::json!({})))
                .execute(&mut *tx)
                .await
                .storage_err()?;

                let usage = message.usage.unwrap_or_default();
                let summary_cache = matches!(
                    &event.body,
                    GraphEventBody::NodeAppended {
                        kind: crate::graph::NodeKind::Summary,
                        payload,
                        ..
                    } if payload.get("summary").and_then(serde_json::Value::as_str).is_some()
                )
                .then(|| match &event.body {
                    GraphEventBody::NodeAppended { payload, .. } => payload
                        .get("summary")
                        .and_then(serde_json::Value::as_str)
                        .map(ToOwned::to_owned),
                    _ => None,
                })
                .flatten();
                sqlx::query(&format!(
                    r#"
                UPDATE {sessions}
                SET current_leaf_id = $2,
                    summary = COALESCE($3, summary),
                    primary_branch_id = $4,
                    total_input_tokens = total_input_tokens + $5,
                    total_output_tokens = total_output_tokens + $6,
                    updated_at = $7
                WHERE id = $1
                "#,
                    sessions = c.sessions_table
                ))
                .bind(sid.to_string())
                .bind(message.id.to_string())
                .bind(summary_cache)
                .bind(primary_branch_id.as_uuid())
                .bind(usage.input_tokens as i64)
                .bind(usage.output_tokens as i64)
                .bind(message.timestamp)
                .execute(&mut *tx)
                .await
                .storage_err()?;

                tx.commit().await.storage_err()?;
                Ok(())
            }
        })
        .await
    }

    async fn finalize(&self, session_id: &SessionId, terminal: SessionState) -> SessionResult<()> {
        if !terminal.is_terminal() {
            return Err(SessionError::InvalidTransition {
                message: format!("finalize requires a terminal state, got {terminal}"),
            });
        }
        let sid = *session_id;
        let state_value = enum_to_db(&terminal, "completed");
        self.with_retry(|| async {
            let c = &self.config;

            // Atomic transition: only sessions not already in a terminal
            // state are eligible. Same-state finalize is a no-op (0 rows
            // affected and not an error, matching the in-memory
            // idempotency of `Session::finalize`).
            let result = sqlx::query(&format!(
                "UPDATE {sessions} SET state = $2, updated_at = NOW() \
                 WHERE id = $1 AND state NOT IN ('completed', 'failed', 'cancelled')",
                sessions = c.sessions_table
            ))
            .bind(sid.to_string())
            .bind(&state_value)
            .execute(self.pool.as_ref())
            .await
            .storage_err()?;

            if result.rows_affected() == 0 {
                // Either not found or already terminal — probe to decide.
                let exists: Option<(String,)> = sqlx::query_as(&format!(
                    "SELECT state FROM {sessions} WHERE id = $1",
                    sessions = c.sessions_table
                ))
                .bind(sid.to_string())
                .fetch_optional(self.pool.as_ref())
                .await
                .storage_err()?;
                match exists {
                    None => {
                        return Err(SessionError::NotFound {
                            id: sid.to_string(),
                        });
                    }
                    Some((existing,)) if existing == state_value => {
                        // Already at target terminal — idempotent no-op.
                    }
                    Some((existing,)) => {
                        return Err(SessionError::InvalidTransition {
                            message: format!(
                                "session already terminal ({existing}), cannot finalize to {state_value}"
                            ),
                        });
                    }
                }
            }

            Ok(())
        })
        .await
    }

    async fn enqueue(
        &self,
        session_id: &SessionId,
        content: String,
        priority: i32,
    ) -> SessionResult<QueueItem> {
        let sid = *session_id;
        let item = QueueItem::enqueue(sid, &content).priority(priority);
        self.with_retry(|| async {
            let c = &self.config;

            sqlx::query(&format!(
                r#"
                INSERT INTO {queue} (id, session_id, operation, content, priority, status, created_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
                queue = c.queue_table
            ))
            .bind(item.id)
            .bind(sid.to_string())
            .bind("enqueue")
            .bind(&content)
            .bind(priority)
            .bind("pending")
            .bind(item.created_at)
            .execute(self.pool.as_ref())
            .await
            .storage_err()?;

            Ok(item.clone())
        })
        .await
    }

    async fn dequeue(&self, session_id: &SessionId) -> SessionResult<Option<QueueItem>> {
        let sid = *session_id;
        self.with_retry(|| async move {
            let c = &self.config;

            let row = sqlx::query(&format!(
                r#"
                UPDATE {queue}
                SET status = 'processing'
                WHERE id = (
                    SELECT id FROM {queue}
                    WHERE session_id = $1 AND status = 'pending'
                    ORDER BY priority DESC, created_at ASC
                    LIMIT 1
                    FOR UPDATE SKIP LOCKED
                )
                RETURNING id, session_id, operation, content, priority, status, created_at, processed_at
                "#,
                queue = c.queue_table
            ))
            .bind(sid.to_string())
            .fetch_optional(self.pool.as_ref())
            .await
            .storage_err()?;

            let Some(row) = row else {
                return Ok(None);
            };

            let id: Uuid = match row.try_get("id") {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!(session_id = %sid, error = %e, "Failed to get dequeued item id");
                    return Ok(None);
                }
            };

            let content = match row.try_get("content") {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(session_id = %sid, queue_id = %id, error = %e, "Failed to get dequeued item content");
                    return Ok(None);
                }
            };

            Ok(Some(QueueItem {
                id,
                session_id: sid,
                operation: super::types::QueueOperation::Enqueue,
                content,
                priority: row.try_get("priority").unwrap_or(0),
                state: QueueItemState::Processing,
                created_at: row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
                processed_at: row.try_get("processed_at").ok(),
            }))
        })
        .await
    }

    async fn cancel_queued(&self, item_id: Uuid) -> SessionResult<bool> {
        self.with_retry(|| async {
            let c = &self.config;

            let result = sqlx::query(&format!(
                "UPDATE {queue} SET status = 'cancelled', processed_at = NOW() WHERE id = $1 AND status = 'pending'",
                queue = c.queue_table
            ))
            .bind(item_id)
            .execute(self.pool.as_ref())
            .await
            .storage_err()?;

            Ok(result.rows_affected() > 0)
        })
        .await
    }

    async fn pending_queue(&self, session_id: &SessionId) -> SessionResult<Vec<QueueItem>> {
        let sid = *session_id;
        self.with_retry(|| async move {
            let c = &self.config;

            let rows = sqlx::query(&format!(
                r#"
                SELECT id, session_id, operation, content, priority, status, created_at, processed_at
                FROM {queue}
                WHERE session_id = $1 AND status = 'pending'
                ORDER BY priority DESC, created_at ASC
                "#,
                queue = c.queue_table
            ))
            .bind(sid.to_string())
            .fetch_all(self.pool.as_ref())
            .await
            .storage_err()?;

            Ok(parse_pending_queue_rows(&sid, rows))
        })
        .await
    }

    async fn replace_pending_queue(
        &self,
        session_id: &SessionId,
        items: &[QueueItem],
    ) -> SessionResult<()> {
        let sid = *session_id;
        let items = items.to_vec();
        self.with_retry(|| async {
            let c = &self.config;
            let mut tx = self.pool.begin().await.storage_err()?;

            sqlx::query(&format!(
                "DELETE FROM {queue} WHERE session_id = $1",
                queue = c.queue_table
            ))
            .bind(sid.to_string())
            .execute(&mut *tx)
            .await
            .storage_err()?;

            for item in &items {
                sqlx::query(&format!(
                    r#"
                    INSERT INTO {queue}
                        (id, session_id, operation, content, priority, status, created_at, processed_at)
                    VALUES
                        ($1, $2, $3, $4, $5, $6, $7, $8)
                    "#,
                    queue = c.queue_table
                ))
                .bind(item.id)
                .bind(sid.to_string())
                .bind("enqueue")
                .bind(&item.content)
                .bind(item.priority)
                .bind("pending")
                .bind(item.created_at)
                .bind(Option::<chrono::DateTime<Utc>>::None)
                .execute(&mut *tx)
                .await
                .storage_err()?;
            }

            tx.commit().await.storage_err()?;
            Ok(())
        })
        .await
    }

    async fn restore_bundle(
        &self,
        session: &Session,
        pending_queue: &[QueueItem],
    ) -> SessionResult<()> {
        let session_template = session.clone();
        let pending_queue_template = pending_queue.to_vec();
        self.with_retry(|| {
            let session = session_template.clone();
            let pending_queue = pending_queue_template.clone();
            async move { self.restore_bundle_inner(&session, &pending_queue).await }
        })
        .await
    }

    async fn cleanup_expired(&self) -> SessionResult<usize> {
        self.with_retry(|| async {
            let c = &self.config;

            let result = sqlx::query(&format!(
                "DELETE FROM {sessions} WHERE \
                 (expires_at IS NOT NULL AND expires_at < NOW()) OR \
                 (updated_at < NOW() - make_interval(days => $1))",
                sessions = c.sessions_table,
            ))
            .bind(c.retention_days as i32)
            .execute(self.pool.as_ref())
            .await
            .storage_err()?;

            Ok(result.rows_affected() as usize)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::ContentPart;
    use crate::session::{Session, SessionConfig};

    #[test]
    fn postgres_config_includes_graph_events_table() {
        let config = PostgresConfig::default();
        assert!(
            config
                .table_names()
                .contains(&config.graph_events_table.as_str())
        );
    }

    #[test]
    fn postgres_schema_includes_graph_events_table() {
        let config = PostgresConfig::default();
        let ddl = PostgresSchema::table_ddl(&config).join("\n");
        let indexes = PostgresSchema::index_ddl(&config).join("\n");

        assert!(ddl.contains(&config.graph_events_table));
        assert!(ddl.contains("primary_branch_id UUID NOT NULL"));
        assert!(indexes.contains(&format!("idx_{}_session", config.graph_events_table)));
    }

    /// Phase D F-1 follow-up: `interpret_stamped_version` is the
    /// pure helper that decides whether a raw row value from
    /// `schema_version` is acceptable. Exercising it here lets us
    /// cover every branch without a live Postgres connection.
    #[test]
    fn interpret_stamped_version_covers_all_windows() {
        use crate::session::{SchemaVersionMismatchDirection, SessionError, SessionSchemaVersion};

        // Missing row -> None (pristine DB, caller decides what to do).
        let result = PostgresSchema::interpret_stamped_version(None).unwrap();
        assert!(result.is_none());

        // Current version -> accepted.
        let current = SessionSchemaVersion::CURRENT.value() as i64;
        let result = PostgresSchema::interpret_stamped_version(Some(current))
            .unwrap()
            .unwrap();
        assert_eq!(result, SessionSchemaVersion::CURRENT);

        // Future version -> TooNew.
        let future = (SessionSchemaVersion::CURRENT.value() + 1) as i64;
        match PostgresSchema::interpret_stamped_version(Some(future)).unwrap_err() {
            SessionError::SchemaVersionMismatch {
                component,
                direction,
                ..
            } => {
                assert_eq!(component, "postgres");
                assert_eq!(direction, SchemaVersionMismatchDirection::TooNew);
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
    }

    /// The `schema_version` DDL string and upsert query reference
    /// the configured table name. A regression against an
    /// accidental hardcoded "schema_version" literal elsewhere
    /// in the file.
    #[test]
    fn schema_version_ddl_uses_configured_table_name() {
        let config = PostgresConfig::prefix("my_").unwrap();
        let ddl = PostgresSchema::schema_version_table_ddl(&config);
        assert!(ddl.contains("my_schema_version"));
        assert_eq!(config.schema_version_table, "my_schema_version");
    }

    #[test]
    fn graph_projection_restores_messages_when_events_exist() {
        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(crate::session::SessionMessage::user(vec![
                ContentPart::text("hi"),
            ]))
            .unwrap();
        session
            .add_message(crate::session::SessionMessage::assistant(vec![
                ContentPart::text("hello"),
            ]))
            .unwrap();

        let mut restored = Session::new(SessionConfig::default());
        restored.id = session.id;
        restored.created_at = session.created_at;
        restored.graph = crate::graph::GraphMaterializer::from_events(&session.graph.events);

        assert_eq!(restored.current_branch_messages().len(), 2);
    }
}
