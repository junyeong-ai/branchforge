//! Redis persistence backend for sessions.

use async_trait::async_trait;
use redis::AsyncCommands;
use std::sync::Arc;
use std::time::Duration;

use super::persistence::Persistence;
use super::state::{Session, SessionId};
use super::types::{QueueItem, SummarySnapshot};
use super::{SessionError, SessionResult, StorageResultExt};
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
}

impl RedisPersistence {
    pub fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        Self::from_config(redis_url, RedisConfig::default())
    }

    pub fn from_config(redis_url: &str, config: RedisConfig) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        Ok(Self {
            client: Arc::new(client),
            config,
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

    fn summaries_key(&self, session_id: &SessionId) -> String {
        format!("{}summaries:{}", self.config.key_prefix, session_id)
    }

    fn queue_key(&self, session_id: &SessionId) -> String {
        format!("{}queue:{}", self.config.key_prefix, session_id)
    }

    /// Key for queue item index: maps item_id → serialized JSON for O(1) cancel.
    fn queue_index_key(&self) -> String {
        format!("{}queue_index", self.config.key_prefix)
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
}

#[async_trait]
impl Persistence for RedisPersistence {
    fn name(&self) -> &str {
        "redis"
    }

    async fn save(&self, session: &Session) -> SessionResult<()> {
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

    async fn load(&self, id: &SessionId) -> SessionResult<Option<Session>> {
        let mut conn = self.get_connection().await?;
        let key = self.session_key(id);

        let data: Option<String> = conn.get(&key).await.storage_err()?;

        match data {
            Some(json) => {
                let mut session: Session =
                    serde_json::from_str(&json).map_err(SessionError::Serialization)?;
                session.refresh_message_projection();
                Ok(Some(session))
            }
            None => Ok(None),
        }
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
        conn.del::<_, ()>(&self.summaries_key(id))
            .await
            .storage_err()?;
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
                Ok(ids.into_iter().map(SessionId::from).collect())
            }
            None => {
                let pattern = format!("{}*", self.config.key_prefix);
                let keys = Self::scan_keys(&mut conn, &pattern).await?;

                let all_ids = keys
                    .into_iter()
                    .filter_map(|key| {
                        key.strip_prefix(&self.config.key_prefix)
                            .filter(|id| !id.contains(':'))
                            .map(SessionId::from)
                    })
                    .collect();

                Ok(all_ids)
            }
        }
    }

    async fn add_summary(&self, snapshot: SummarySnapshot) -> SessionResult<()> {
        let mut conn = self.get_connection().await?;
        let key = self.summaries_key(&snapshot.session_id);
        let data = serde_json::to_string(&snapshot).map_err(SessionError::Serialization)?;

        conn.rpush::<_, _, ()>(&key, &data).await.storage_err()?;

        Ok(())
    }

    async fn get_summaries(&self, session_id: &SessionId) -> SessionResult<Vec<SummarySnapshot>> {
        let mut conn = self.get_connection().await?;
        let key = self.summaries_key(session_id);

        let items: Vec<String> = conn.lrange(&key, 0, -1).await.storage_err()?;

        items
            .into_iter()
            .map(|json| serde_json::from_str(&json).map_err(SessionError::Serialization))
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

    async fn cleanup_expired(&self) -> SessionResult<usize> {
        let mut conn = self.get_connection().await?;
        let mut cleaned = 0;

        // Redis auto-expires session keys via TTL, but related data becomes orphaned.
        // Clean up orphaned summaries, queues, queue_index, children sets, and tenant refs.

        // 1. Clean orphaned summaries
        let pattern = format!("{}summaries:*", self.config.key_prefix);
        cleaned += self.cleanup_orphaned_keys(&mut conn, &pattern).await?;

        // 2. Clean orphaned queues and their index entries
        let pattern = format!("{}queue:*", self.config.key_prefix);
        cleaned += self.cleanup_orphaned_queues(&mut conn, &pattern).await?;

        // 3. Clean orphaned children sets
        let pattern = format!("{}children:*", self.config.key_prefix);
        cleaned += self.cleanup_orphaned_keys(&mut conn, &pattern).await?;

        // 4. Clean stale references from tenant sets
        let pattern = format!("{}tenant:*", self.config.key_prefix);
        cleaned += self.cleanup_tenant_refs(&mut conn, &pattern).await?;

        // 5. Clean stale queue_index entries
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
                let session_key = self.session_key(&SessionId::from(session_id));
                let exists: bool = conn.exists(&session_key).await.storage_err()?;

                if !exists {
                    conn.del::<_, ()>(&key).await.storage_err()?;
                    cleaned += 1;
                }
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
                let session_key = self.session_key(&SessionId::from(member.as_str()));
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
                let session_key = self.session_key(&SessionId::from(session_id));
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
            let session_id = match serde_json::from_str::<QueueItem>(&json_data) {
                Ok(item) => item.session_id,
                Err(_) => {
                    // Corrupt entry, remove it
                    conn.hdel::<_, _, ()>(&index_key, &item_id)
                        .await
                        .storage_err()?;
                    cleaned += 1;
                    continue;
                }
            };

            let session_key = self.session_key(&session_id);
            let exists: bool = conn.exists(&session_key).await.storage_err()?;

            if !exists {
                conn.hdel::<_, _, ()>(&index_key, &item_id)
                    .await
                    .storage_err()?;
                cleaned += 1;
            }
        }

        Ok(cleaned)
    }
}
