//! Redis persistence backend for sessions.

use async_trait::async_trait;
use redis::AsyncCommands;
use redis::Script;
use std::sync::Arc;
use std::time::Duration;

use super::archive::verify_restored_session_roundtrip;
use super::lock::{DEFAULT_LOCK_TTL_SECS, DistributedLock, RedisLock};
use super::persistence::{Persistence, validate_session_graph};
use super::state::{Session, SessionId, SessionMessage, SessionState};
use super::types::QueueItem;
use super::{SessionError, SessionResult, StorageResultExt};
use crate::graph::{GraphEvent, GraphMaterializer, GraphValidator};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct RedisConfig {
    pub key_prefix: String,
    pub default_ttl: Option<Duration>,
    pub connection_timeout: Duration,
    pub response_timeout: Duration,
    /// Maximum retry attempts for transient failures.
    pub max_retries: u32,
    /// Initial backoff duration for retries.
    pub initial_backoff: Duration,
    /// Maximum backoff duration.
    pub max_backoff: Duration,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            key_prefix: "claude:session:".to_string(),
            default_ttl: Some(Duration::from_secs(86400 * 7)),
            connection_timeout: Duration::from_secs(10),
            response_timeout: Duration::from_secs(30),
            max_retries: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
        }
    }
}

impl RedisConfig {
    pub fn prefix(mut self, prefix: impl Into<String>) -> SessionResult<Self> {
        let prefix = prefix.into();
        if !prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        {
            return Err(SessionError::Storage {
                message: format!(
                    "Invalid key prefix '{}': only ASCII alphanumeric, underscore, and colon allowed",
                    prefix
                ),
            });
        }
        self.key_prefix = prefix;
        Ok(self)
    }

    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    pub fn without_ttl(mut self) -> Self {
        self.default_ttl = None;
        self
    }
}

pub struct RedisPersistence {
    client: Arc<redis::Client>,
    config: RedisConfig,
    lock: RedisLock,
}

impl RedisPersistence {
    pub fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        Self::from_config(redis_url, RedisConfig::default())
    }

    pub fn from_config(redis_url: &str, config: RedisConfig) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        let lock = RedisLock::new(redis_url)?;
        Ok(Self {
            client: Arc::new(client),
            config,
            lock,
        })
    }

    pub fn prefix(mut self, prefix: impl Into<String>) -> SessionResult<Self> {
        self.config = self.config.prefix(prefix)?;
        Ok(self)
    }

    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.config = self.config.ttl(ttl);
        self
    }

    pub fn without_ttl(mut self) -> Self {
        self.config = self.config.without_ttl();
        self
    }

    fn session_key(&self, id: &SessionId) -> String {
        format!("{}{}", self.config.key_prefix, id)
    }

    fn tenant_key(&self, tenant_id: &str) -> String {
        format!("{}tenant:{}", self.config.key_prefix, tenant_id)
    }

    fn children_key(&self, parent_id: &SessionId) -> String {
        format!("{}children:{}", self.config.key_prefix, parent_id)
    }

    fn queue_key(&self, session_id: &SessionId) -> String {
        format!("{}queue:{}", self.config.key_prefix, session_id)
    }

    fn parse_session_id_strict(value: &str, context: &str) -> SessionResult<SessionId> {
        SessionId::parse(value).ok_or_else(|| SessionError::Storage {
            message: format!("Redis {context} contains invalid session UUID '{value}'"),
        })
    }

    /// Key for queue item index: maps item_id → serialized JSON for O(1) cancel.
    fn queue_index_key(&self) -> String {
        format!("{}queue_index", self.config.key_prefix)
    }

    fn restore_staging_session_key(&self, session_id: &SessionId, nonce: Uuid) -> String {
        format!(
            "{}restore:{}:{}:session",
            self.config.key_prefix, nonce, session_id
        )
    }

    fn restore_staging_queue_key(&self, session_id: &SessionId, nonce: Uuid) -> String {
        format!(
            "{}restore:{}:{}:queue",
            self.config.key_prefix, nonce, session_id
        )
    }

    async fn load_session_from_key(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        key: &str,
    ) -> SessionResult<Option<(String, Session)>> {
        let data: Option<String> = conn.get(key).await.storage_err()?;
        match data {
            Some(json) => {
                let mut session: Session =
                    serde_json::from_str(&json).map_err(SessionError::Serialization)?;
                Self::validate_loaded_session(&mut session, key)?;
                Ok(Some((json, session)))
            }
            None => Ok(None),
        }
    }

    async fn load_session_snapshot_from_key(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        key: &str,
    ) -> SessionResult<Option<Session>> {
        Ok(self
            .load_session_from_key(conn, key)
            .await?
            .map(|(_, session)| session))
    }

    fn validate_loaded_session(session: &mut Session, source: &str) -> SessionResult<()> {
        let report = GraphValidator::validate(&session.graph);
        if !report.is_valid() {
            let issues = report
                .issues
                .into_iter()
                .map(|issue| format!("{}: {}", issue.code, issue.message))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(SessionError::Storage {
                message: format!(
                    "Invalid graph in Redis snapshot '{}' for session {}: {}",
                    source, session.id, issues
                ),
            });
        }

        session.refresh_summary_cache();
        session.refresh_message_projection();
        Ok(())
    }

    async fn pending_queue_from_key(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        key: &str,
    ) -> SessionResult<Vec<QueueItem>> {
        let items: Vec<String> = conn.zrange(key, 0, -1).await.storage_err()?;
        items
            .into_iter()
            .map(|json| serde_json::from_str(&json).map_err(SessionError::Serialization))
            .collect()
    }

    async fn get_connection(&self) -> SessionResult<redis::aio::MultiplexedConnection> {
        super::with_retry(
            self.config.max_retries,
            self.config.initial_backoff,
            self.config.max_backoff,
            Self::is_retryable,
            || async {
                tokio::time::timeout(
                    self.config.connection_timeout,
                    self.client.get_multiplexed_async_connection(),
                )
                .await
                .storage_err_ctx("connection timeout")?
                .storage_err()
            },
        )
        .await
    }

    fn is_retryable(error: &SessionError) -> bool {
        match error {
            SessionError::Storage { message } => {
                message.contains("timeout")
                    || message.contains("connection")
                    || message.contains("BUSY")
                    || message.contains("LOADING")
                    || message.contains("CLUSTERDOWN")
            }
            _ => false,
        }
    }

    async fn scan_keys(
        conn: &mut redis::aio::MultiplexedConnection,
        pattern: &str,
    ) -> SessionResult<Vec<String>> {
        let mut cursor: u64 = 0;
        let mut all_keys = Vec::new();

        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(conn)
                .await
                .storage_err()?;

            all_keys.extend(keys);
            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(all_keys)
    }

    fn is_conflict(error: &SessionError) -> bool {
        matches!(
            error,
            SessionError::Storage { message } if message.contains("REDIS_SESSION_CONFLICT")
        )
    }

    async fn compare_and_set_session(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        key: &str,
        expected_json: &str,
        next_json: &str,
    ) -> SessionResult<bool> {
        let compare_and_set = Script::new(
            r#"
            local current = redis.call('GET', KEYS[1])
            if not current then
                return redis.error_reply('REDIS_SESSION_MISSING')
            end
            if current ~= ARGV[1] then
                return 0
            end

            local ttl = redis.call('PTTL', KEYS[1])
            redis.call('SET', KEYS[1], ARGV[2])
            if ttl > 0 then
                redis.call('PEXPIRE', KEYS[1], ttl)
            elseif ttl == -1 then
                redis.call('PERSIST', KEYS[1])
            end

            return 1
            "#,
        );

        match compare_and_set
            .key(key)
            .arg(expected_json)
            .arg(next_json)
            .invoke_async::<i32>(conn)
            .await
        {
            Ok(1) => Ok(true),
            Ok(0) => Ok(false),
            Ok(_) => Err(SessionError::Storage {
                message: "Redis compare-and-set returned an unknown status".to_string(),
            }),
            Err(error) if error.to_string().contains("REDIS_SESSION_MISSING") => {
                Err(SessionError::NotFound {
                    id: key.to_string(),
                })
            }
            Err(error) => Err(SessionError::Storage {
                message: format!("Redis compare-and-set failed: {}", error),
            }),
        }
    }

    async fn mutate_session_atomic<F>(
        &self,
        session_id: &SessionId,
        operation: &str,
        mutate: F,
    ) -> SessionResult<()>
    where
        F: Fn(&mut Session) -> SessionResult<()>,
    {
        let key = self.session_key(session_id);
        let lock_resource = format!("session:{}", session_id);
        let lock_ttl = Duration::from_secs(DEFAULT_LOCK_TTL_SECS);
        let mut attempt = 0;
        let mut backoff = self.config.initial_backoff;

        loop {
            let result = async {
                // Acquire distributed lock before reading + mutating
                let mut guard = self.lock.acquire(&lock_resource, lock_ttl).await?;

                let inner_result = async {
                    let mut conn = self.get_connection().await?;
                    let Some((expected_json, mut session)) =
                        self.load_session_from_key(&mut conn, &key).await?
                    else {
                        return Err(SessionError::NotFound {
                            id: session_id.to_string(),
                        });
                    };

                    mutate(&mut session)?;
                    validate_session_graph(&session, "redis")?;
                    session.refresh_summary_cache();
                    session.refresh_message_projection();
                    let next_json =
                        serde_json::to_string(&session).map_err(SessionError::Serialization)?;

                    if self
                        .compare_and_set_session(&mut conn, &key, &expected_json, &next_json)
                        .await?
                    {
                        Ok(())
                    } else {
                        Err(SessionError::Storage {
                            message: format!(
                                "REDIS_SESSION_CONFLICT during {operation} for session {}",
                                session_id
                            ),
                        })
                    }
                }
                .await;

                // Always release the lock, even on error
                if let Err(release_err) = self.lock.release(&mut guard).await {
                    tracing::warn!(
                        session_id = %session_id,
                        operation,
                        error = %release_err,
                        "Failed to release distributed lock"
                    );
                }

                inner_result
            }
            .await;

            match result {
                Ok(()) => return Ok(()),
                Err(error)
                    if attempt < self.config.max_retries
                        && (Self::is_retryable(&error) || Self::is_conflict(&error)) =>
                {
                    attempt += 1;
                    tracing::warn!(
                        attempt = attempt,
                        session_id = %session_id,
                        operation,
                        error = %error,
                        "Retrying Redis session mutation"
                    );
                    let jitter_factor = 1.0 + (rand::random::<f64>() * 0.2 - 0.1);
                    tokio::time::sleep(backoff.mul_f64(jitter_factor)).await;
                    backoff = (backoff * 2).min(self.config.max_backoff);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[async_trait]
impl Persistence for RedisPersistence {
    fn name(&self) -> &str {
        "redis"
    }

    async fn save(&self, session: &Session) -> SessionResult<()> {
        validate_session_graph(session, "redis")?;
        let mut conn = self.get_connection().await?;
        let key = self.session_key(&session.id);
        let mut persisted = session.clone();
        persisted.refresh_message_projection();
        let data = serde_json::to_string(&persisted).map_err(SessionError::Serialization)?;

        let ttl_secs = persisted
            .config
            .ttl_secs
            .or_else(|| self.config.default_ttl.map(|d| d.as_secs()));

        let mut pipe = redis::pipe();
        pipe.atomic();

        match ttl_secs {
            Some(ttl) => {
                pipe.cmd("SET").arg(&key).arg(&data).arg("EX").arg(ttl);
            }
            None => {
                pipe.cmd("SET").arg(&key).arg(&data);
            }
        }

        if let Some(ref tenant_id) = persisted.tenant_id {
            pipe.cmd("SADD")
                .arg(self.tenant_key(tenant_id))
                .arg(persisted.id.to_string());
        }

        if let Some(parent_id) = persisted.parent_id {
            pipe.cmd("SADD")
                .arg(self.children_key(&parent_id))
                .arg(persisted.id.to_string());
        }

        pipe.query_async::<()>(&mut conn).await.storage_err()?;

        Ok(())
    }

    async fn append_graph_event(
        &self,
        session_id: &SessionId,
        event: GraphEvent,
    ) -> SessionResult<()> {
        self.mutate_session_atomic(session_id, "append_graph_event", |session| {
            let graph_id = session.graph.id;
            let created_at = session.graph.created_at;
            let primary_branch = session.graph.primary_branch;
            session.graph.events.push(event.clone());
            session.graph = GraphMaterializer::from_events_with_primary(
                &session.graph.events,
                Some(primary_branch),
            );
            session.graph.id = graph_id;
            session.graph.created_at = created_at;
            Ok(())
        })
        .await
    }

    async fn add_message(
        &self,
        session_id: &SessionId,
        message: SessionMessage,
    ) -> SessionResult<()> {
        self.mutate_session_atomic(session_id, "add_message", |session| {
            session.add_message(message.clone())
        })
        .await
    }

    async fn set_state(&self, session_id: &SessionId, state: SessionState) -> SessionResult<()> {
        self.mutate_session_atomic(session_id, "set_state", |session| {
            session.set_state(state);
            Ok(())
        })
        .await
    }

    async fn load(&self, id: &SessionId) -> SessionResult<Option<Session>> {
        let mut conn = self.get_connection().await?;
        let key = self.session_key(id);
        self.load_session_snapshot_from_key(&mut conn, &key).await
    }

    async fn delete(&self, id: &SessionId) -> SessionResult<bool> {
        let mut conn = self.get_connection().await?;
        let key = self.session_key(id);

        // Load session to get relationships before deletion
        if let Some(session) = self.load(id).await? {
            // Remove from tenant set
            if let Some(ref tenant_id) = session.tenant_id {
                conn.srem::<_, _, ()>(&self.tenant_key(tenant_id), id.to_string())
                    .await
                    .storage_err()?;
            }

            // Remove from parent's children set
            if let Some(parent_id) = session.parent_id {
                conn.srem::<_, _, ()>(&self.children_key(&parent_id), id.to_string())
                    .await
                    .storage_err()?;
            }
        }

        // Clean up queue items and remove from queue_index
        let queue_key = self.queue_key(id);
        let items: Vec<String> = conn.zrange(&queue_key, 0, -1).await.storage_err()?;
        let index_key = self.queue_index_key();
        for json in items {
            if let Ok(item) = serde_json::from_str::<QueueItem>(&json) {
                conn.hdel::<_, _, ()>(&index_key, item.id.to_string())
                    .await
                    .storage_err()?;
            }
        }

        // Delete related keys
        conn.del::<_, ()>(&queue_key).await.storage_err()?;
        conn.del::<_, ()>(&self.children_key(id))
            .await
            .storage_err()?;

        // Delete session
        let deleted: i32 = conn.del(&key).await.storage_err()?;

        Ok(deleted > 0)
    }

    async fn list(&self, tenant_id: Option<&str>) -> SessionResult<Vec<SessionId>> {
        let mut conn = self.get_connection().await?;

        match tenant_id {
            Some(tid) => {
                let ids: Vec<String> = conn.smembers(self.tenant_key(tid)).await.storage_err()?;
                ids.into_iter()
                    .map(|id| {
                        Self::parse_session_id_strict(&id, &format!("tenant index '{}'", tid))
                    })
                    .collect()
            }
            None => {
                let pattern = format!("{}*", self.config.key_prefix);
                let keys = Self::scan_keys(&mut conn, &pattern).await?;
                let mut all_ids = Vec::new();

                for key in keys {
                    if let Some(id) = key
                        .strip_prefix(&self.config.key_prefix)
                        .filter(|id| !id.contains(':'))
                    {
                        all_ids.push(Self::parse_session_id_strict(
                            id,
                            &format!("session key '{key}'"),
                        )?);
                    }
                }

                Ok(all_ids)
            }
        }
    }

    async fn list_children(&self, parent_id: &SessionId) -> SessionResult<Vec<SessionId>> {
        let mut conn = self.get_connection().await?;
        let ids: Vec<String> = conn
            .smembers(self.children_key(parent_id))
            .await
            .storage_err()?;
        ids.into_iter()
            .map(|id| {
                Self::parse_session_id_strict(
                    &id,
                    &format!("children index for parent {parent_id}"),
                )
            })
            .collect()
    }

    async fn enqueue(
        &self,
        session_id: &SessionId,
        content: String,
        priority: i32,
    ) -> SessionResult<QueueItem> {
        let mut conn = self.get_connection().await?;
        let key = self.queue_key(session_id);
        let item = QueueItem::enqueue(*session_id, &content).priority(priority);
        let data = serde_json::to_string(&item).map_err(SessionError::Serialization)?;
        let index_key = self.queue_index_key();

        // Atomic: add to sorted set + index in MULTI/EXEC
        let mut pipe = redis::pipe();
        pipe.atomic();
        pipe.cmd("ZADD")
            .arg(&key)
            .arg(-(priority as f64))
            .arg(&data);
        pipe.cmd("HSET")
            .arg(&index_key)
            .arg(item.id.to_string())
            .arg(&data);
        pipe.query_async::<()>(&mut conn).await.storage_err()?;

        Ok(item)
    }

    async fn dequeue(&self, session_id: &SessionId) -> SessionResult<Option<QueueItem>> {
        let mut conn = self.get_connection().await?;
        let key = self.queue_key(session_id);

        let items: Vec<String> = conn.zpopmin(&key, 1).await.storage_err()?;

        if items.is_empty() {
            return Ok(None);
        }

        let json = &items[0];
        let mut item: QueueItem =
            serde_json::from_str(json).map_err(SessionError::Serialization)?;
        item.start_processing();

        let index_key = self.queue_index_key();
        conn.hdel::<_, _, ()>(&index_key, item.id.to_string())
            .await
            .storage_err()?;

        Ok(Some(item))
    }

    async fn cancel_queued(&self, item_id: Uuid) -> SessionResult<bool> {
        let mut conn = self.get_connection().await?;
        let index_key = self.queue_index_key();

        // O(1): Get serialized item data from index
        let data: Option<String> = conn
            .hget(&index_key, item_id.to_string())
            .await
            .storage_err()?;

        let Some(data) = data else {
            return Ok(false);
        };

        // Extract session_id to construct queue key
        let item: QueueItem = serde_json::from_str(&data).map_err(SessionError::Serialization)?;
        let queue_key = self.queue_key(&item.session_id);

        // O(1): Remove from sorted set using exact member + remove from index
        let removed: i32 = conn.zrem(&queue_key, &data).await.storage_err()?;
        conn.hdel::<_, _, ()>(&index_key, item_id.to_string())
            .await
            .storage_err()?;

        Ok(removed > 0)
    }

    async fn pending_queue(&self, session_id: &SessionId) -> SessionResult<Vec<QueueItem>> {
        let mut conn = self.get_connection().await?;
        let key = self.queue_key(session_id);

        let items: Vec<String> = conn.zrange(&key, 0, -1).await.storage_err()?;

        items
            .into_iter()
            .map(|json| serde_json::from_str(&json).map_err(SessionError::Serialization))
            .collect()
    }

    async fn replace_pending_queue(
        &self,
        session_id: &SessionId,
        items: &[QueueItem],
    ) -> SessionResult<()> {
        let mut conn = self.get_connection().await?;
        let key = self.queue_key(session_id);
        let index_key = self.queue_index_key();

        let existing: Vec<String> = conn.zrange(&key, 0, -1).await.storage_err()?;
        let mut pipe = redis::pipe();
        pipe.atomic();

        for json in existing {
            if let Ok(item) = serde_json::from_str::<QueueItem>(&json) {
                pipe.cmd("HDEL").arg(&index_key).arg(item.id.to_string());
            }
        }

        pipe.cmd("DEL").arg(&key);

        for item in items {
            let mut item = item.clone();
            item.session_id = *session_id;
            item.status = super::types::QueueStatus::Pending;
            item.processed_at = None;
            let data = serde_json::to_string(&item).map_err(SessionError::Serialization)?;
            pipe.cmd("ZADD")
                .arg(&key)
                .arg(-(item.priority as f64))
                .arg(&data);
            pipe.cmd("HSET")
                .arg(&index_key)
                .arg(item.id.to_string())
                .arg(&data);
        }

        pipe.query_async::<()>(&mut conn).await.storage_err()?;
        Ok(())
    }

    async fn restore_bundle(
        &self,
        session: &Session,
        pending_queue: &[QueueItem],
    ) -> SessionResult<()> {
        let mut conn = self.get_connection().await?;
        let key = self.session_key(&session.id);
        let queue_key = self.queue_key(&session.id);
        let queue_index_key = self.queue_index_key();
        let restore_nonce = Uuid::new_v4();
        let staging_key = self.restore_staging_session_key(&session.id, restore_nonce);
        let staging_queue_key = self.restore_staging_queue_key(&session.id, restore_nonce);
        let mut persisted = session.clone();
        persisted.refresh_message_projection();
        let data = serde_json::to_string(&persisted).map_err(SessionError::Serialization)?;
        let ttl_secs = persisted
            .config
            .ttl_secs
            .or_else(|| self.config.default_ttl.map(|d| d.as_secs()));
        let tenant_key = persisted
            .tenant_id
            .as_deref()
            .map(|tenant_id| self.tenant_key(tenant_id))
            .unwrap_or_default();
        let children_key = persisted
            .parent_id
            .map(|parent_id| self.children_key(&parent_id))
            .unwrap_or_default();
        let normalized_queue: Vec<QueueItem> = pending_queue
            .iter()
            .cloned()
            .map(|mut item| {
                item.session_id = persisted.id;
                item.status = super::types::QueueStatus::Pending;
                item.processed_at = None;
                item
            })
            .collect();
        let staging_ttl_secs = ttl_secs.unwrap_or(300);
        let stale_index_ids: Vec<String> = conn
            .hgetall::<_, Vec<(String, String)>>(&queue_index_key)
            .await
            .storage_err()?
            .into_iter()
            .filter_map(|(item_id, json_data)| {
                serde_json::from_str::<QueueItem>(&json_data)
                    .ok()
                    .filter(|item| item.session_id == persisted.id)
                    .map(|_| item_id)
            })
            .collect();

        let mut stage_pipe = redis::pipe();
        stage_pipe.atomic();
        stage_pipe.cmd("DEL").arg(&staging_key);
        stage_pipe.cmd("DEL").arg(&staging_queue_key);
        stage_pipe
            .cmd("SET")
            .arg(&staging_key)
            .arg(&data)
            .arg("EX")
            .arg(staging_ttl_secs);
        if normalized_queue.is_empty() {
            stage_pipe.cmd("DEL").arg(&staging_queue_key);
        } else {
            for item in &normalized_queue {
                let payload = serde_json::to_string(item).map_err(SessionError::Serialization)?;
                stage_pipe
                    .cmd("ZADD")
                    .arg(&staging_queue_key)
                    .arg(-(item.priority as f64))
                    .arg(payload);
            }
            stage_pipe
                .cmd("EXPIRE")
                .arg(&staging_queue_key)
                .arg(staging_ttl_secs);
        }
        stage_pipe
            .query_async::<()>(&mut conn)
            .await
            .storage_err()?;

        let staged_session = match self
            .load_session_snapshot_from_key(&mut conn, &staging_key)
            .await?
        {
            Some(session) => session,
            None => {
                conn.del::<_, ()>(&staging_key).await.storage_err()?;
                conn.del::<_, ()>(&staging_queue_key).await.storage_err()?;
                return Err(SessionError::Storage {
                    message: format!(
                        "Redis archive restore staging lost session {} before verification",
                        session.id
                    ),
                });
            }
        };
        let staged_queue = self
            .pending_queue_from_key(&mut conn, &staging_queue_key)
            .await?;
        if let Err(error) = verify_restored_session_roundtrip(
            session,
            &normalized_queue,
            &staged_session,
            &staged_queue,
        ) {
            let _ = conn.del::<_, ()>(&staging_key).await;
            let _ = conn.del::<_, ()>(&staging_queue_key).await;
            return Err(error);
        }

        let publish_script = Script::new(
            r#"
            if redis.call('EXISTS', KEYS[1]) == 1 then
                return redis.error_reply('ARCHIVE_RESTORE_OVERWRITE')
            end
            if redis.call('EXISTS', KEYS[2]) == 0 then
                return redis.error_reply('ARCHIVE_RESTORE_MISSING_STAGING')
            end

            redis.call('RENAME', KEYS[2], KEYS[1])
            if ARGV[2] == '1' then
                redis.call('PERSIST', KEYS[1])
            end

            if KEYS[4] ~= '' then
                redis.call('SADD', KEYS[4], ARGV[1])
            end
            if KEYS[5] ~= '' then
                redis.call('SADD', KEYS[5], ARGV[1])
            end

            local stale_count = tonumber(ARGV[3])
            local index = 4
            for _ = 1, stale_count do
                redis.call('HDEL', KEYS[6], ARGV[index])
                index = index + 1
            end

            redis.call('DEL', KEYS[3])
            if redis.call('EXISTS', KEYS[7]) == 1 then
                redis.call('RENAME', KEYS[7], KEYS[3])
                if ARGV[2] == '1' then
                    redis.call('PERSIST', KEYS[3])
                end
            end

            local item_count = tonumber(ARGV[index])
            index = index + 1
            for _ = 1, item_count do
                local item_id = ARGV[index]
                local payload = ARGV[index + 1]
                redis.call('HSET', KEYS[6], item_id, payload)
                index = index + 2
            end

            return 1
            "#,
        );

        let mut invocation = publish_script.prepare_invoke();
        invocation
            .key(&key)
            .key(&staging_key)
            .key(&queue_key)
            .key(&tenant_key)
            .key(&children_key)
            .key(&queue_index_key)
            .key(&staging_queue_key)
            .arg(persisted.id.to_string())
            .arg(if ttl_secs.is_some() { "0" } else { "1" })
            .arg(stale_index_ids.len());

        for stale_id in &stale_index_ids {
            invocation.arg(stale_id);
        }

        invocation.arg(normalized_queue.len());
        for item in &normalized_queue {
            let payload = serde_json::to_string(item).map_err(SessionError::Serialization)?;
            invocation.arg(item.id.to_string()).arg(payload);
        }

        match invocation.invoke_async::<i32>(&mut conn).await {
            Ok(_) => Ok(()),
            Err(error) if error.to_string().contains("ARCHIVE_RESTORE_OVERWRITE") => {
                let _ = conn.del::<_, ()>(&staging_key).await;
                let _ = conn.del::<_, ()>(&staging_queue_key).await;
                Err(SessionError::Storage {
                    message: format!(
                        "Archive restore refuses to overwrite existing session {}",
                        session.id
                    ),
                })
            }
            Err(error)
                if error
                    .to_string()
                    .contains("ARCHIVE_RESTORE_MISSING_STAGING") =>
            {
                Err(SessionError::Storage {
                    message: format!(
                        "Redis archive restore lost staging data for session {}",
                        session.id
                    ),
                })
            }
            Err(error) => {
                let _ = conn.del::<_, ()>(&staging_key).await;
                let _ = conn.del::<_, ()>(&staging_queue_key).await;
                Err(SessionError::Storage {
                    message: format!("Redis archive restore failed: {}", error),
                })
            }
        }
    }

    async fn cleanup_expired(&self) -> SessionResult<usize> {
        let mut conn = self.get_connection().await?;
        let mut cleaned = 0;

        // Redis auto-expires session keys via TTL, but related data becomes orphaned.
        // Clean up orphaned queues, queue_index, children sets, and tenant refs.

        // 1. Clean orphaned queues and their index entries
        let pattern = format!("{}queue:*", self.config.key_prefix);
        cleaned += self.cleanup_orphaned_queues(&mut conn, &pattern).await?;

        // 2. Clean orphaned children sets
        let pattern = format!("{}children:*", self.config.key_prefix);
        cleaned += self.cleanup_orphaned_keys(&mut conn, &pattern).await?;

        // 3. Clean stale references from tenant sets
        let pattern = format!("{}tenant:*", self.config.key_prefix);
        cleaned += self.cleanup_tenant_refs(&mut conn, &pattern).await?;

        // 4. Clean stale queue_index entries
        cleaned += self.cleanup_queue_index(&mut conn).await?;

        Ok(cleaned)
    }
}

impl RedisPersistence {
    async fn cleanup_orphaned_keys(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        pattern: &str,
    ) -> SessionResult<usize> {
        let keys = Self::scan_keys(conn, pattern).await?;
        let mut cleaned = 0;

        for key in keys {
            if let Some(session_id) = key
                .strip_prefix(&self.config.key_prefix)
                .and_then(|s| s.split(':').nth(1))
            {
                let parent_id = match Self::parse_session_id_strict(
                    session_id,
                    &format!("children key '{key}'"),
                ) {
                    Ok(session_id) => session_id,
                    Err(_) => {
                        conn.del::<_, ()>(&key).await.storage_err()?;
                        cleaned += 1;
                        continue;
                    }
                };
                let session_key = self.session_key(&parent_id);
                let exists: bool = conn.exists(&session_key).await.storage_err()?;

                if !exists {
                    conn.del::<_, ()>(&key).await.storage_err()?;
                    cleaned += 1;
                }
            } else {
                conn.del::<_, ()>(&key).await.storage_err()?;
                cleaned += 1;
            }
        }

        Ok(cleaned)
    }

    async fn cleanup_tenant_refs(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        pattern: &str,
    ) -> SessionResult<usize> {
        let keys = Self::scan_keys(conn, pattern).await?;
        let mut cleaned = 0;

        for key in keys {
            let members: Vec<String> = conn.smembers(&key).await.storage_err()?;

            for member in members {
                let session_id = match Self::parse_session_id_strict(
                    member.as_str(),
                    &format!("tenant reference '{key}'"),
                ) {
                    Ok(session_id) => session_id,
                    Err(_) => {
                        conn.srem::<_, _, ()>(&key, &member).await.storage_err()?;
                        cleaned += 1;
                        continue;
                    }
                };
                let session_key = self.session_key(&session_id);
                let exists: bool = conn.exists(&session_key).await.storage_err()?;

                if !exists {
                    conn.srem::<_, _, ()>(&key, &member).await.storage_err()?;
                    cleaned += 1;
                }
            }
        }

        Ok(cleaned)
    }

    async fn cleanup_orphaned_queues(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        pattern: &str,
    ) -> SessionResult<usize> {
        let keys = Self::scan_keys(conn, pattern).await?;
        let mut cleaned = 0;
        let index_key = self.queue_index_key();

        for key in keys {
            if let Some(session_id) = key
                .strip_prefix(&self.config.key_prefix)
                .and_then(|s| s.strip_prefix("queue:"))
            {
                let parsed_session_id = match Self::parse_session_id_strict(
                    session_id,
                    &format!("queue key '{key}'"),
                ) {
                    Ok(session_id) => session_id,
                    Err(_) => {
                        let items: Vec<String> = conn.zrange(&key, 0, -1).await.storage_err()?;
                        for json in items {
                            if let Ok(item) = serde_json::from_str::<QueueItem>(&json) {
                                conn.hdel::<_, _, ()>(&index_key, item.id.to_string())
                                    .await
                                    .storage_err()?;
                            }
                        }
                        conn.del::<_, ()>(&key).await.storage_err()?;
                        cleaned += 1;
                        continue;
                    }
                };
                let session_key = self.session_key(&parsed_session_id);
                let exists: bool = conn.exists(&session_key).await.storage_err()?;

                if !exists {
                    let items: Vec<String> = conn.zrange(&key, 0, -1).await.storage_err()?;
                    for json in items {
                        if let Ok(item) = serde_json::from_str::<QueueItem>(&json) {
                            conn.hdel::<_, _, ()>(&index_key, item.id.to_string())
                                .await
                                .storage_err()?;
                        }
                    }
                    conn.del::<_, ()>(&key).await.storage_err()?;
                    cleaned += 1;
                }
            } else {
                conn.del::<_, ()>(&key).await.storage_err()?;
                cleaned += 1;
            }
        }

        Ok(cleaned)
    }

    /// Clean stale entries from queue_index where session no longer exists.
    async fn cleanup_queue_index(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
    ) -> SessionResult<usize> {
        let index_key = self.queue_index_key();
        let mut cleaned = 0;

        let entries: Vec<(String, String)> = conn.hgetall(&index_key).await.storage_err()?;

        for (item_id, json_data) in entries {
            let item = match serde_json::from_str::<QueueItem>(&json_data) {
                Ok(item) => item,
                Err(_) => {
                    // Corrupt entry, remove it
                    conn.hdel::<_, _, ()>(&index_key, &item_id)
                        .await
                        .storage_err()?;
                    cleaned += 1;
                    continue;
                }
            };

            let session_key = self.session_key(&item.session_id);
            let exists: bool = conn.exists(&session_key).await.storage_err()?;

            if !exists {
                conn.hdel::<_, _, ()>(&index_key, &item_id)
                    .await
                    .storage_err()?;
                cleaned += 1;
                continue;
            }

            let queue_key = self.queue_key(&item.session_id);
            let queue_contains_item: Option<f64> =
                conn.zscore(&queue_key, &json_data).await.storage_err()?;
            if queue_contains_item.is_none() {
                conn.hdel::<_, _, ()>(&index_key, &item_id)
                    .await
                    .storage_err()?;
                cleaned += 1;
            }
        }

        Ok(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionConfig, SessionMessage};
    use crate::types::ContentBlock;

    #[test]
    fn validate_loaded_session_rejects_invalid_graph() {
        let mut session = Session::new(SessionConfig::default());
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("hello")]))
            .unwrap();

        let node = session
            .graph
            .nodes
            .values_mut()
            .next()
            .expect("graph has node");
        node.provenance = None;
        node.created_by_principal_id = Some("user-1".to_string());

        let error = RedisPersistence::validate_loaded_session(&mut session, "unit-test")
            .expect_err("invalid graph should fail validation");
        assert!(
            error
                .to_string()
                .contains("Invalid graph in Redis snapshot")
        );
    }
}
