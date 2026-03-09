//! PostgreSQL session persistence with explicit schema management.
//!
//! # Schema Management
//!
//! This module separates schema management from data access, allowing flexible deployment:
//!
//! ```rust,no_run
//! use claude_agent::session::{PostgresPersistence, PostgresSchema, PostgresConfig};
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
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::graph::SessionGraph;

use super::persistence::Persistence;
use super::state::{Session, SessionConfig, SessionId, SessionMessage};
use super::types::{CompactRecord, Plan, QueueItem, QueueStatus, SummarySnapshot, TodoItem};
use super::{SessionError, SessionResult, StorageResultExt};

fn enum_to_db<T: serde::Serialize>(value: &T, default: &str) -> String {
    serde_json::to_string(value)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|_| default.to_string())
}

fn db_to_enum<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_str(&format!("\"{}\"", s)).ok()
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
    pub messages_table: String,
    pub graph_events_table: String,
    pub checkpoints_table: String,
    pub bookmarks_table: String,
    pub compacts_table: String,
    pub summaries_table: String,
    pub queue_table: String,
    pub todos_table: String,
    pub plans_table: String,
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
            messages_table: format!("{prefix}messages"),
            graph_events_table: format!("{prefix}graph_events"),
            checkpoints_table: format!("{prefix}checkpoints"),
            bookmarks_table: format!("{prefix}bookmarks"),
            compacts_table: format!("{prefix}compacts"),
            summaries_table: format!("{prefix}summaries"),
            queue_table: format!("{prefix}queue"),
            todos_table: format!("{prefix}todos"),
            plans_table: format!("{prefix}plans"),
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
            &self.messages_table,
            &self.graph_events_table,
            &self.checkpoints_table,
            &self.bookmarks_table,
            &self.compacts_table,
            &self.summaries_table,
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
        sql.push_str("-- Claude Agent Session Schema\n");
        sql.push_str("-- Generated by claude-agent PostgresSchema\n\n");

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

    /// Generate table DDL statements.
    pub fn table_ddl(config: &PostgresConfig) -> Vec<String> {
        let c = config;
        vec![
            format!(
                r#"CREATE TABLE IF NOT EXISTS {sessions} (
    id VARCHAR(255) PRIMARY KEY,
    parent_id VARCHAR(255),
    tenant_id VARCHAR(255),
    session_type VARCHAR(32) NOT NULL DEFAULT 'main',
    state VARCHAR(32) NOT NULL DEFAULT 'created',
    mode VARCHAR(32) NOT NULL DEFAULT 'default',
    config JSONB NOT NULL DEFAULT '{{}}',
    permissions JSONB NOT NULL DEFAULT '{{}}',
    summary TEXT,
    total_input_tokens BIGINT DEFAULT 0,
    total_output_tokens BIGINT DEFAULT 0,
    total_cost_usd DECIMAL(12, 6) DEFAULT 0,
    current_leaf_id VARCHAR(255),
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
                r#"CREATE TABLE IF NOT EXISTS {messages} (
    id VARCHAR(255) PRIMARY KEY,
    session_id VARCHAR(255) NOT NULL,
    parent_id VARCHAR(255),
    role VARCHAR(16) NOT NULL,
    content JSONB NOT NULL,
    is_sidechain BOOLEAN DEFAULT FALSE,
    is_compact_summary BOOLEAN DEFAULT FALSE,
    model VARCHAR(64),
    request_id VARCHAR(255),
    usage JSONB,
    metadata JSONB,
    environment JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_{messages}_session FOREIGN KEY (session_id) REFERENCES {sessions}(id) ON DELETE CASCADE
);"#,
                messages = c.messages_table,
                sessions = c.sessions_table
            ),
            format!(
                r#"CREATE TABLE IF NOT EXISTS {graph_events} (
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
                r#"CREATE TABLE IF NOT EXISTS {checkpoints} (
    id UUID PRIMARY KEY,
    session_id VARCHAR(255) NOT NULL,
    branch_id UUID NOT NULL,
    label TEXT NOT NULL,
    note TEXT,
    tags JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_{checkpoints}_session FOREIGN KEY (session_id) REFERENCES {sessions}(id) ON DELETE CASCADE
);"#,
                checkpoints = c.checkpoints_table,
                sessions = c.sessions_table
            ),
            format!(
                r#"CREATE TABLE IF NOT EXISTS {bookmarks} (
    id UUID PRIMARY KEY,
    session_id VARCHAR(255) NOT NULL,
    node_id UUID NOT NULL,
    branch_id UUID NOT NULL,
    label TEXT NOT NULL,
    note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_{bookmarks}_session FOREIGN KEY (session_id) REFERENCES {sessions}(id) ON DELETE CASCADE
);"#,
                bookmarks = c.bookmarks_table,
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
                r#"CREATE TABLE IF NOT EXISTS {summaries} (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id VARCHAR(255) NOT NULL,
    summary TEXT NOT NULL,
    leaf_message_id VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_{summaries}_session FOREIGN KEY (session_id) REFERENCES {sessions}(id) ON DELETE CASCADE
);"#,
                summaries = c.summaries_table,
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
            // Messages indexes
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session ON {0}(session_id)",
                c.messages_table
            ),
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session ON {0}(session_id)",
                c.graph_events_table
            ),
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session ON {0}(session_id)",
                c.checkpoints_table
            ),
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session ON {0}(session_id)",
                c.bookmarks_table
            ),
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_created ON {0}(session_id, created_at)",
                c.messages_table
            ),
            // Compacts index
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session ON {0}(session_id)",
                c.compacts_table
            ),
            // Summaries index
            format!(
                "CREATE INDEX IF NOT EXISTS idx_{0}_session ON {0}(session_id)",
                c.summaries_table
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
                c.messages_table.clone(),
                format!("idx_{}_session", c.messages_table),
            ),
            (
                c.messages_table.clone(),
                format!("idx_{}_created", c.messages_table),
            ),
            (
                c.graph_events_table.clone(),
                format!("idx_{}_session", c.graph_events_table),
            ),
            (
                c.checkpoints_table.clone(),
                format!("idx_{}_session", c.checkpoints_table),
            ),
            (
                c.bookmarks_table.clone(),
                format!("idx_{}_session", c.bookmarks_table),
            ),
            (
                c.compacts_table.clone(),
                format!("idx_{}_session", c.compacts_table),
            ),
            (
                c.summaries_table.clone(),
                format!("idx_{}_session", c.summaries_table),
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
    pub async fn migrate(pool: &PgPool, config: &PostgresConfig) -> Result<(), sqlx::Error> {
        for table_ddl in Self::table_ddl(config) {
            sqlx::query(&table_ddl).execute(pool).await?;
        }

        for index_ddl in Self::index_ddl(config) {
            sqlx::query(&index_ddl).execute(pool).await?;
        }

        Ok(())
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
            SELECT id, parent_id, tenant_id, session_type, state, mode,
                   config, permissions, summary,
                   total_input_tokens, total_output_tokens, total_cost_usd,
                   current_leaf_id, static_context_hash, error,
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
        let checkpoints = self.load_checkpoints(session_id).await?;
        let bookmarks = self.load_bookmarks(session_id).await?;
        let messages = Vec::new();
        let compacts = self.load_compacts(session_id).await?;
        let todos = self.load_todos_internal(session_id).await?;
        let plan = self.load_plan_internal(session_id).await?;

        let config: SessionConfig = match row.try_get::<serde_json::Value, _>("config") {
            Ok(v) => serde_json::from_value(v).unwrap_or_else(|e| {
                tracing::warn!(session_id = %session_id, error = %e, "Failed to deserialize session config");
                Default::default()
            }),
            Err(e) => {
                tracing::warn!(session_id = %session_id, error = %e, "Failed to read session config column");
                Default::default()
            }
        };

        let permissions = match row.try_get::<serde_json::Value, _>("permissions") {
            Ok(v) => serde_json::from_value(v).unwrap_or_else(|e| {
                tracing::warn!(session_id = %session_id, error = %e, "Failed to deserialize session permissions");
                Default::default()
            }),
            Err(e) => {
                tracing::warn!(session_id = %session_id, error = %e, "Failed to read session permissions column");
                Default::default()
            }
        };

        let session_type = row
            .try_get::<&str, _>("session_type")
            .ok()
            .and_then(db_to_enum)
            .unwrap_or_default();

        // mode column is ignored; SessionMode was removed (always stateless)
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

        let mut session = Session {
            id: *session_id,
            parent_id: row
                .try_get::<&str, _>("parent_id")
                .ok()
                .and_then(|s| s.parse().ok()),
            session_type,
            tenant_id: row.try_get("tenant_id").ok(),
            state,
            config,
            permissions,
            messages,
            current_leaf_id,
            summary: row.try_get("summary").ok(),
            total_usage: crate::types::TokenUsage {
                input_tokens: row.try_get::<i64, _>("total_input_tokens").unwrap_or(0) as u64,
                output_tokens: row.try_get::<i64, _>("total_output_tokens").unwrap_or(0) as u64,
                ..Default::default()
            },
            current_input_tokens: 0,
            total_cost_usd: row
                .try_get::<rust_decimal::Decimal, _>("total_cost_usd")
                .unwrap_or_default(),
            static_context_hash: row.try_get("static_context_hash").ok(),
            created_at: row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
            updated_at: row.try_get("updated_at").unwrap_or_else(|_| Utc::now()),
            expires_at: row.try_get("expires_at").ok(),
            error: row.try_get("error").ok(),
            todos,
            current_plan: plan,
            compact_history: VecDeque::from(compacts),
            graph: SessionGraph::default(),
        };
        session.graph = if !graph_events.is_empty() {
            let mut graph = crate::graph::GraphMaterializer::from_events(&graph_events);
            graph.id = session.id.0;
            graph.created_at = session.created_at;
            graph
        } else {
            let mut graph = SessionGraph::new("main");
            graph.id = session.id.0;
            graph.created_at = session.created_at;
            graph
        };
        session.refresh_message_projection();
        session.graph.checkpoints = checkpoints.into_iter().map(|c| (c.id, c)).collect();
        session.graph.bookmarks = bookmarks.into_iter().map(|b| (b.id, b)).collect();
        session.refresh_message_projection();
        Ok(session)
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
            ORDER BY created_at ASC
            "#,
            graph_events = c.graph_events_table
        ))
        .bind(session_id.to_string())
        .fetch_all(self.pool.as_ref())
        .await
        .storage_err()?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            if let Ok(value) = row.try_get::<serde_json::Value, _>("event")
                && let Ok(event) = serde_json::from_value(value)
            {
                events.push(event);
            }
        }
        Ok(events)
    }

    async fn load_checkpoints(
        &self,
        session_id: &SessionId,
    ) -> SessionResult<Vec<crate::graph::Checkpoint>> {
        let c = &self.config;
        let rows = sqlx::query(&format!(
            "SELECT id, branch_id, label, note, tags, created_at FROM {} WHERE session_id = $1 ORDER BY created_at ASC",
            c.checkpoints_table
        ))
        .bind(session_id.to_string())
        .fetch_all(self.pool.as_ref())
        .await
        .storage_err()?;

        let mut checkpoints = Vec::with_capacity(rows.len());
        for row in rows {
            if let (Ok(id), Ok(branch_id), Ok(label), created_at) = (
                row.try_get("id"),
                row.try_get("branch_id"),
                row.try_get("label"),
                row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
            ) {
                checkpoints.push(crate::graph::Checkpoint {
                    id,
                    branch_id,
                    label,
                    note: row.try_get("note").ok(),
                    tags: row
                        .try_get::<serde_json::Value, _>("tags")
                        .ok()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default(),
                    created_at,
                });
            }
        }
        Ok(checkpoints)
    }

    async fn load_bookmarks(
        &self,
        session_id: &SessionId,
    ) -> SessionResult<Vec<crate::graph::Bookmark>> {
        let c = &self.config;
        let rows = sqlx::query(&format!(
            "SELECT id, node_id, branch_id, label, note, created_at FROM {} WHERE session_id = $1 ORDER BY created_at ASC",
            c.bookmarks_table
        ))
        .bind(session_id.to_string())
        .fetch_all(self.pool.as_ref())
        .await
        .storage_err()?;

        let mut bookmarks = Vec::with_capacity(rows.len());
        for row in rows {
            if let (Ok(id), Ok(node_id), Ok(branch_id), Ok(label), created_at) = (
                row.try_get("id"),
                row.try_get("node_id"),
                row.try_get("branch_id"),
                row.try_get("label"),
                row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
            ) {
                bookmarks.push(crate::graph::Bookmark {
                    id,
                    node_id,
                    branch_id,
                    label,
                    note: row.try_get("note").ok(),
                    created_at,
                });
            }
        }
        Ok(bookmarks)
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
                pre_tokens: row.try_get::<i32, _>("pre_tokens").unwrap_or(0) as usize,
                post_tokens: row.try_get::<i32, _>("post_tokens").unwrap_or(0) as usize,
                saved_tokens: row.try_get::<i32, _>("saved_tokens").unwrap_or(0) as usize,
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

        Ok(compacts)
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

        Ok(todos)
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

        let Some(row) = row else {
            return Ok(None);
        };

        let id: Uuid = match row.try_get("id") {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(session_id = %session_id, error = %e, "Skipping plan row: failed to get id");
                return Ok(None);
            }
        };

        let status = match row.try_get::<&str, _>("status").ok().and_then(db_to_enum) {
            Some(s) => s,
            None => {
                tracing::warn!(session_id = %session_id, plan_id = %id, "Skipping plan row: failed to parse status");
                return Ok(None);
            }
        };

        let content = match row.try_get("content") {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(session_id = %session_id, plan_id = %id, error = %e, "Skipping plan row: failed to get content");
                return Ok(None);
            }
        };

        Ok(Some(Plan {
            id,
            session_id: *session_id,
            name: row.try_get("name").ok(),
            content,
            status,
            error: row.try_get("error").ok(),
            created_at: row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
            approved_at: row.try_get("approved_at").ok(),
            started_at: row.try_get("started_at").ok(),
            completed_at: row.try_get("completed_at").ok(),
        }))
    }

    async fn save_todos_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
        todos: &[TodoItem],
    ) -> SessionResult<()> {
        let c = &self.config;

        sqlx::query(&format!(
            "DELETE FROM {todos} WHERE session_id = $1",
            todos = c.todos_table
        ))
        .bind(session_id.to_string())
        .execute(&mut **tx)
        .await
        .storage_err()?;

        for todo in todos {
            let status = enum_to_db(&todo.status, "pending");

            sqlx::query(&format!(
                r#"
                INSERT INTO {todos} (
                    id, session_id, plan_id, content, active_form, status,
                    created_at, started_at, completed_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
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

        Ok(())
    }

    async fn save_plan_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        plan: &Plan,
    ) -> SessionResult<()> {
        let c = &self.config;

        let status = enum_to_db(&plan.status, "draft");

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

        Ok(())
    }

    async fn save_compacts_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
        compacts: &VecDeque<CompactRecord>,
    ) -> SessionResult<()> {
        let c = &self.config;

        sqlx::query(&format!(
            "DELETE FROM {compacts} WHERE session_id = $1",
            compacts = c.compacts_table
        ))
        .bind(session_id.to_string())
        .execute(&mut **tx)
        .await
        .storage_err()?;

        for compact in compacts {
            let trigger = enum_to_db(&compact.trigger, "manual");

            sqlx::query(&format!(
                r#"
                INSERT INTO {compacts} (
                    id, session_id, trigger, pre_tokens, post_tokens, saved_tokens,
                    summary, original_count, new_count, logical_parent_id, created_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                "#,
                compacts = c.compacts_table
            ))
            .bind(compact.id)
            .bind(session_id.to_string())
            .bind(&trigger)
            .bind(compact.pre_tokens as i32)
            .bind(compact.post_tokens as i32)
            .bind(compact.saved_tokens as i32)
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

    async fn save_messages_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
        messages: &[SessionMessage],
    ) -> SessionResult<()> {
        let c = &self.config;

        // Collect current message IDs for orphan cleanup
        let current_ids: Vec<String> = messages.iter().map(|m| m.id.to_string()).collect();

        // Upsert each message (INSERT ... ON CONFLICT (id) DO UPDATE)
        for message in messages {
            let role = enum_to_db(&message.role, "user");

            sqlx::query(&format!(
                r#"
                INSERT INTO {messages} (
                    id, session_id, parent_id, role, content, is_sidechain,
                    is_compact_summary, model, request_id, usage, metadata,
                    environment, created_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                ON CONFLICT (id) DO UPDATE SET
                    parent_id = EXCLUDED.parent_id,
                    role = EXCLUDED.role,
                    content = EXCLUDED.content,
                    is_sidechain = EXCLUDED.is_sidechain,
                    is_compact_summary = EXCLUDED.is_compact_summary,
                    model = EXCLUDED.model,
                    request_id = EXCLUDED.request_id,
                    usage = EXCLUDED.usage,
                    metadata = EXCLUDED.metadata,
                    environment = EXCLUDED.environment
                "#,
                messages = c.messages_table
            ))
            .bind(message.id.to_string())
            .bind(session_id.to_string())
            .bind(message.parent_id.as_ref().map(|id| id.to_string()))
            .bind(&role)
            .bind(serde_json::to_value(&message.content).unwrap_or_else(|e| {
                tracing::warn!(message_id = %message.id, error = %e, "Failed to serialize message content");
                serde_json::Value::Array(Vec::new())
            }))
            .bind(message.is_sidechain)
            .bind(message.is_compact_summary)
            .bind(&message.metadata.model)
            .bind(&message.metadata.request_id)
            .bind(
                message
                    .usage
                    .as_ref()
                    .and_then(|u| serde_json::to_value(u).ok()),
            )
            .bind(serde_json::to_value(&message.metadata).unwrap_or_else(|e| {
                tracing::warn!(message_id = %message.id, error = %e, "Failed to serialize message metadata");
                serde_json::Value::Object(Default::default())
            }))
            .bind(
                message
                    .environment
                    .as_ref()
                    .and_then(|e| serde_json::to_value(e).ok()),
            )
            .bind(message.timestamp)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        // Delete messages no longer in the session.
        // Guard: skip when current_ids is empty to avoid deleting ALL messages
        // (PostgreSQL treats `id != ALL(ARRAY[])` as true for every row).
        if !current_ids.is_empty() {
            sqlx::query(&format!(
                "DELETE FROM {messages} WHERE session_id = $1 AND id != ALL($2)",
                messages = c.messages_table
            ))
            .bind(session_id.to_string())
            .bind(&current_ids)
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
        events: &[crate::graph::GraphEvent],
    ) -> SessionResult<()> {
        let c = &self.config;
        let current_ids: Vec<Uuid> = events.iter().map(|event| event.metadata.id).collect();

        for event in events {
            sqlx::query(&format!(
                r#"
                INSERT INTO {graph_events} (id, session_id, event, created_at)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (id) DO UPDATE SET
                    event = EXCLUDED.event,
                    created_at = EXCLUDED.created_at
                "#,
                graph_events = c.graph_events_table
            ))
            .bind(event.metadata.id)
            .bind(session_id.to_string())
            .bind(serde_json::to_value(event).unwrap_or_else(|_| serde_json::json!({})))
            .bind(event.metadata.occurred_at)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        if !current_ids.is_empty() {
            sqlx::query(&format!(
                "DELETE FROM {graph_events} WHERE session_id = $1 AND id != ALL($2)",
                graph_events = c.graph_events_table
            ))
            .bind(session_id.to_string())
            .bind(&current_ids)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        Ok(())
    }

    async fn save_checkpoints_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
        checkpoints: &[crate::graph::Checkpoint],
    ) -> SessionResult<()> {
        let c = &self.config;
        let current_ids: Vec<Uuid> = checkpoints.iter().map(|checkpoint| checkpoint.id).collect();

        for checkpoint in checkpoints {
            sqlx::query(&format!(
                "INSERT INTO {} (id, session_id, branch_id, label, note, tags, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (id) DO UPDATE SET branch_id = EXCLUDED.branch_id, label = EXCLUDED.label, note = EXCLUDED.note, tags = EXCLUDED.tags, created_at = EXCLUDED.created_at",
                c.checkpoints_table
            ))
            .bind(checkpoint.id)
            .bind(session_id.to_string())
            .bind(checkpoint.branch_id)
            .bind(&checkpoint.label)
            .bind(&checkpoint.note)
            .bind(serde_json::to_value(&checkpoint.tags).unwrap_or_else(|_| serde_json::json!([])))
            .bind(checkpoint.created_at)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        if !current_ids.is_empty() {
            sqlx::query(&format!(
                "DELETE FROM {} WHERE session_id = $1 AND id != ALL($2)",
                c.checkpoints_table
            ))
            .bind(session_id.to_string())
            .bind(&current_ids)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }
        Ok(())
    }

    async fn save_bookmarks_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        session_id: &SessionId,
        bookmarks: &[crate::graph::Bookmark],
    ) -> SessionResult<()> {
        let c = &self.config;
        let current_ids: Vec<Uuid> = bookmarks.iter().map(|bookmark| bookmark.id).collect();

        for bookmark in bookmarks {
            sqlx::query(&format!(
                "INSERT INTO {} (id, session_id, node_id, branch_id, label, note, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (id) DO UPDATE SET node_id = EXCLUDED.node_id, branch_id = EXCLUDED.branch_id, label = EXCLUDED.label, note = EXCLUDED.note, created_at = EXCLUDED.created_at",
                c.bookmarks_table
            ))
            .bind(bookmark.id)
            .bind(session_id.to_string())
            .bind(bookmark.node_id)
            .bind(bookmark.branch_id)
            .bind(&bookmark.label)
            .bind(&bookmark.note)
            .bind(bookmark.created_at)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }

        if !current_ids.is_empty() {
            sqlx::query(&format!(
                "DELETE FROM {} WHERE session_id = $1 AND id != ALL($2)",
                c.bookmarks_table
            ))
            .bind(session_id.to_string())
            .bind(&current_ids)
            .execute(&mut **tx)
            .await
            .storage_err()?;
        }
        Ok(())
    }

    async fn save_inner(&self, session: &Session) -> SessionResult<()> {
        let c = &self.config;

        let mut tx = self.pool.begin().await.storage_err()?;

        let session_type = enum_to_db(&session.session_type, "main");
        let state = enum_to_db(&session.state, "created");
        let mode = "stateless";

        sqlx::query(&format!(
            r#"
            INSERT INTO {sessions} (
                id, parent_id, tenant_id, session_type, state, mode,
                config, permissions, summary,
                total_input_tokens, total_output_tokens, total_cost_usd,
                current_leaf_id, static_context_hash, error,
                created_at, updated_at, expires_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                $10, $11, $12, $13, $14, $15, $16, $17, $18
            )
            ON CONFLICT (id) DO UPDATE SET
                parent_id = EXCLUDED.parent_id,
                tenant_id = EXCLUDED.tenant_id,
                session_type = EXCLUDED.session_type,
                state = EXCLUDED.state,
                mode = EXCLUDED.mode,
                config = EXCLUDED.config,
                permissions = EXCLUDED.permissions,
                summary = EXCLUDED.summary,
                total_input_tokens = EXCLUDED.total_input_tokens,
                total_output_tokens = EXCLUDED.total_output_tokens,
                total_cost_usd = EXCLUDED.total_cost_usd,
                current_leaf_id = EXCLUDED.current_leaf_id,
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
        .bind(&session_type)
        .bind(&state)
        .bind(mode)
        .bind(serde_json::to_value(&session.config).unwrap_or_else(|e| {
            tracing::warn!(session_id = %session.id, error = %e, "Failed to serialize session config");
            serde_json::Value::Object(Default::default())
        }))
        .bind(serde_json::to_value(&session.permissions).unwrap_or_else(|e| {
            tracing::warn!(session_id = %session.id, error = %e, "Failed to serialize session permissions");
            serde_json::Value::Object(Default::default())
        }))
        .bind(&session.summary)
        .bind(session.total_usage.input_tokens as i64)
        .bind(session.total_usage.output_tokens as i64)
        .bind(session.total_cost_usd)
        .bind(session.current_leaf_id.as_ref().map(|id| id.to_string()))
        .bind(&session.static_context_hash)
        .bind(&session.error)
        .bind(session.created_at)
        .bind(session.updated_at)
        .bind(session.expires_at)
        .execute(&mut *tx)
        .await
        .storage_err()?;

        let message_projection = session.current_branch_messages();

        self.save_messages_tx(&mut tx, &session.id, &message_projection)
            .await?;
        self.save_graph_events_tx(&mut tx, &session.id, &session.graph.events)
            .await?;
        self.save_checkpoints_tx(
            &mut tx,
            &session.id,
            &session
                .graph
                .checkpoints
                .values()
                .cloned()
                .collect::<Vec<_>>(),
        )
        .await?;
        self.save_bookmarks_tx(
            &mut tx,
            &session.id,
            &session
                .graph
                .bookmarks
                .values()
                .cloned()
                .collect::<Vec<_>>(),
        )
        .await?;
        self.save_todos_tx(&mut tx, &session.id, &session.todos)
            .await?;
        self.save_compacts_tx(&mut tx, &session.id, &session.compact_history)
            .await?;

        if let Some(ref plan) = session.current_plan {
            self.save_plan_tx(&mut tx, plan).await?;
        }

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
        self.with_retry(|| self.save_inner(session)).await
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

    async fn add_summary(&self, snapshot: SummarySnapshot) -> SessionResult<()> {
        self.with_retry(|| async {
            let c = &self.config;

            let mut tx = self.pool.begin().await.storage_err()?;

            sqlx::query(&format!(
                r#"
                INSERT INTO {summaries} (id, session_id, summary, leaf_message_id, created_at)
                VALUES ($1, $2, $3, $4, $5)
                "#,
                summaries = c.summaries_table
            ))
            .bind(snapshot.id)
            .bind(snapshot.session_id.to_string())
            .bind(&snapshot.summary)
            .bind(snapshot.leaf_message_id.as_ref().map(|id| id.to_string()))
            .bind(snapshot.created_at)
            .execute(&mut *tx)
            .await
            .storage_err()?;

            sqlx::query(&format!(
                "UPDATE {sessions} SET summary = $1, updated_at = NOW() WHERE id = $2",
                sessions = c.sessions_table
            ))
            .bind(&snapshot.summary)
            .bind(snapshot.session_id.to_string())
            .execute(&mut *tx)
            .await
            .storage_err()?;

            tx.commit().await.storage_err()?;

            Ok(())
        })
        .await
    }

    async fn get_summaries(&self, session_id: &SessionId) -> SessionResult<Vec<SummarySnapshot>> {
        let sid = *session_id;
        self.with_retry(|| async move {
            let c = &self.config;

            let rows = sqlx::query(&format!(
                r#"
                SELECT id, session_id, summary, leaf_message_id, created_at
                FROM {summaries}
                WHERE session_id = $1
                ORDER BY created_at ASC
                "#,
                summaries = c.summaries_table
            ))
            .bind(sid.to_string())
            .fetch_all(self.pool.as_ref())
            .await
            .storage_err()?;

            let mut summaries = Vec::with_capacity(rows.len());

            for row in rows {
                let id: Uuid = match row.try_get("id") {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!(session_id = %sid, error = %e, "Skipping summary row: failed to get id");
                        continue;
                    }
                };

                let summary = match row.try_get("summary") {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(session_id = %sid, summary_id = %id, error = %e, "Skipping summary row: failed to get summary");
                        continue;
                    }
                };

                summaries.push(SummarySnapshot {
                    id,
                    session_id: sid,
                    summary,
                    leaf_message_id: row
                        .try_get::<&str, _>("leaf_message_id")
                        .ok()
                        .and_then(|s| s.parse().ok()),
                    created_at: row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
                });
            }

            Ok(summaries)
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
                status: QueueStatus::Processing,
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

            let mut items = Vec::with_capacity(rows.len());

            for row in rows {
                let id: Uuid = match row.try_get("id") {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::warn!(session_id = %sid, error = %e, "Skipping queue row: failed to get id");
                        continue;
                    }
                };

                let content = match row.try_get("content") {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(session_id = %sid, queue_id = %id, error = %e, "Skipping queue row: failed to get content");
                        continue;
                    }
                };

                items.push(QueueItem {
                    id,
                    session_id: sid,
                    operation: super::types::QueueOperation::Enqueue,
                    content,
                    priority: row.try_get("priority").unwrap_or(0),
                    status: QueueStatus::Pending,
                    created_at: row.try_get("created_at").unwrap_or_else(|_| Utc::now()),
                    processed_at: row.try_get("processed_at").ok(),
                });
            }

            Ok(items)
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
    use crate::session::{Session, SessionConfig};
    use crate::types::ContentBlock;

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
        assert!(ddl.contains(&config.checkpoints_table));
        assert!(ddl.contains(&config.bookmarks_table));
        assert!(indexes.contains(&format!("idx_{}_session", config.graph_events_table)));
        assert!(indexes.contains(&format!("idx_{}_session", config.checkpoints_table)));
        assert!(indexes.contains(&format!("idx_{}_session", config.bookmarks_table)));
    }

    #[test]
    fn graph_projection_restores_messages_when_events_exist() {
        let mut session = Session::new(SessionConfig::default());
        session.add_message(crate::session::SessionMessage::user(vec![
            ContentBlock::text("hi"),
        ]));
        session.add_message(crate::session::SessionMessage::assistant(vec![
            ContentBlock::text("hello"),
        ]));

        let mut restored = Session::new(SessionConfig::default());
        restored.id = session.id;
        restored.created_at = session.created_at;
        restored.graph = crate::graph::GraphMaterializer::from_events(&session.graph.events);
        restored.refresh_message_projection();

        assert_eq!(restored.current_branch_messages().len(), 2);
    }

    #[test]
    fn postgres_schema_includes_checkpoint_and_bookmark_tables() {
        let config = PostgresConfig::default();
        let ddl = PostgresSchema::table_ddl(&config).join("\n");

        assert!(ddl.contains(&config.checkpoints_table));
        assert!(ddl.contains(&config.bookmarks_table));
    }
}
