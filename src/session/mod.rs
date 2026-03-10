//! Session management for stateful conversations.

pub mod compact;
mod export;
pub mod manager;
pub mod persistence;
#[cfg(feature = "jsonl")]
pub mod persistence_jsonl;
#[cfg(feature = "postgres")]
pub mod persistence_postgres;
#[cfg(feature = "redis-backend")]
pub mod persistence_redis;
#[cfg(feature = "surrealdb-backend")]
pub mod persistence_surreal;
pub mod queue;
mod replay;
pub mod session_state;
pub mod state;
pub mod types;

pub use crate::types::TokenUsage;
pub use compact::{CompactExecutor, CompactStrategy, DEFAULT_COMPACT_THRESHOLD};
pub use export::{AuditBundle, ExportPolicy, SessionExporter};
pub use manager::SessionManager;
pub use persistence::{MemoryPersistence, Persistence, PersistenceFactory};
#[cfg(feature = "jsonl")]
pub use persistence_jsonl::{
    JsonlConfig, JsonlConfigBuilder, JsonlEntry, JsonlPersistence, SyncMode,
};
#[cfg(feature = "postgres")]
pub use persistence_postgres::{
    PgPoolConfig, PostgresConfig, PostgresPersistence, PostgresSchema, SchemaIssue,
};
#[cfg(feature = "redis-backend")]
pub use persistence_redis::{RedisConfig, RedisPersistence};
#[cfg(feature = "surrealdb-backend")]
pub use persistence_surreal::{SurrealConfig, SurrealPersistence, surreal_prototype};
pub use queue::{InputQueue, MergedInput, QueueError, QueuedInput, SharedInputQueue};
pub use replay::ReplayService;
pub use session_state::{ExecutionGuard, ToolState};
pub use state::{
    MessageId, MessageMetadata, Session, SessionConfig, SessionId, SessionMessage,
    SessionPermissions, SessionState, SessionToolLimits, SessionType,
};
pub use types::{
    CompactRecord, CompactTrigger, EnvironmentContext, Plan, PlanStatus, QueueItem, QueueOperation,
    QueueStatus, SessionAccessScope, SessionStats, SessionTree, SummarySnapshot, TodoItem,
    TodoStatus, ToolExecution,
};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum SessionError {
    #[error("Session not found: {id}")]
    NotFound { id: String },

    #[error("Session expired: {id}")]
    Expired { id: String },

    #[error("Storage error: {message}")]
    Storage { message: String },

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Compact error: {message}")]
    Compact { message: String },

    #[error("Context error: {0}")]
    Context(#[from] crate::context::ContextError),
}

pub type SessionResult<T> = std::result::Result<T, SessionError>;

#[cfg(any(feature = "postgres", feature = "redis-backend"))]
pub(crate) trait StorageResultExt<T> {
    fn storage_err(self) -> SessionResult<T>;
    fn storage_err_ctx(self, context: &str) -> SessionResult<T>;
}

#[cfg(any(feature = "postgres", feature = "redis-backend"))]
pub(crate) async fn with_retry<F, Fut, T>(
    max_retries: u32,
    initial_backoff: std::time::Duration,
    max_backoff: std::time::Duration,
    is_retryable: impl Fn(&SessionError) -> bool,
    operation: F,
) -> SessionResult<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = SessionResult<T>>,
{
    let mut attempt = 0;
    let mut backoff = initial_backoff;

    loop {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) if attempt < max_retries && is_retryable(&e) => {
                attempt += 1;
                tracing::warn!(
                    attempt = attempt,
                    error = %e,
                    "Retrying operation after transient failure"
                );
                // Symmetrical 10% jitter to prevent thundering herd
                let jitter_factor = 1.0 + (rand::random::<f64>() * 0.2 - 0.1);
                tokio::time::sleep(backoff.mul_f64(jitter_factor)).await;
                backoff = (backoff * 2).min(max_backoff);
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(any(feature = "postgres", feature = "redis-backend"))]
impl<T, E: std::fmt::Display> StorageResultExt<T> for std::result::Result<T, E> {
    fn storage_err(self) -> SessionResult<T> {
        self.map_err(|e| SessionError::Storage {
            message: e.to_string(),
        })
    }

    fn storage_err_ctx(self, context: &str) -> SessionResult<T> {
        self.map_err(|e| SessionError::Storage {
            message: format!("{}: {}", context, e),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_error_display() {
        let err = SessionError::NotFound {
            id: "test-123".to_string(),
        };
        assert!(err.to_string().contains("test-123"));
    }

    #[test]
    fn test_session_error_expired() {
        let err = SessionError::Expired {
            id: "sess-456".to_string(),
        };
        assert!(err.to_string().contains("expired"));
    }
}
