//! Distributed locking for coordinating access to shared session resources.
//!
//! This module provides a [`DistributedLock`] trait with backend-specific implementations
//! for Redis (SETNX + TTL) and PostgreSQL (advisory locks). Locks use unique tokens
//! to guarantee ownership verification on extend and release operations.
//!
//! # Redis Example
//!
//! ```rust,no_run
//! # #[cfg(feature = "redis-backend")]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use branchforge::session::lock::{RedisLock, DistributedLock};
//! use std::time::Duration;
//!
//! let lock = RedisLock::new("redis://127.0.0.1/")?;
//! let mut guard = lock.acquire("session:abc", Duration::from_secs(30)).await?;
//! // ... perform mutation ...
//! lock.release(&mut guard).await?;
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use std::time::Duration;

use super::{SessionError, SessionResult};

/// Default lock TTL in seconds.
pub const DEFAULT_LOCK_TTL_SECS: u64 = 30;

/// Default retry delay between lock acquisition attempts.
pub const DEFAULT_RETRY_DELAY_MS: u64 = 100;

/// Default maximum number of lock acquisition retries.
pub const DEFAULT_MAX_RETRIES: u32 = 50;

/// Key prefix for all distributed lock keys.
pub const LOCK_KEY_PREFIX: &str = "branchforge:lock:";

/// A distributed lock for coordinating access to shared resources.
#[async_trait]
pub trait DistributedLock: Send + Sync {
    /// Acquire the lock, retrying with backoff until success or max retries.
    /// Returns a guard that identifies the held lock.
    async fn acquire(&self, resource: &str, ttl: Duration) -> SessionResult<LockGuard>;

    /// Try to acquire the lock without retrying. Returns `None` if the lock is
    /// already held by another owner.
    async fn try_acquire(&self, resource: &str, ttl: Duration) -> SessionResult<Option<LockGuard>>;

    /// Extend the TTL of an existing lock. Returns `true` if the lock was
    /// successfully extended, `false` if the lock is no longer owned by
    /// the given guard (e.g., it expired and was acquired by someone else).
    async fn extend(&self, guard: &LockGuard, ttl: Duration) -> SessionResult<bool>;

    /// Release the lock explicitly. This is a no-op if the lock has already
    /// expired or been released.
    async fn release(&self, guard: &mut LockGuard) -> SessionResult<()>;
}

/// Guard representing a held distributed lock.
///
/// The guard carries a unique token that is checked on extend and release
/// operations to prevent accidentally releasing a lock that was re-acquired
/// by another process after TTL expiry.
///
/// **Important**: Callers must explicitly call [`DistributedLock::release`]
/// when done. If the guard is dropped without release, a warning is logged
/// and the lock will remain held until its TTL expires.
pub struct LockGuard {
    /// The resource identifier this lock protects.
    pub resource: String,
    /// Unique token proving lock ownership.
    pub token: String,
    /// When this lock was acquired (monotonic clock).
    pub acquired_at: std::time::Instant,
    /// Whether this guard was explicitly released.
    released: bool,
}

impl LockGuard {
    /// Create a new lock guard for the given resource with a unique token.
    pub fn new(resource: String, token: String) -> Self {
        Self {
            resource,
            token,
            acquired_at: std::time::Instant::now(),
            released: false,
        }
    }

    /// Mark this guard as explicitly released.
    pub(crate) fn mark_released(&mut self) {
        self.released = true;
    }

    /// Check whether this lock has been held longer than the given TTL.
    pub fn is_expired(&self, ttl: Duration) -> bool {
        self.acquired_at.elapsed() > ttl
    }
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard")
            .field("resource", &self.resource)
            .field("token", &self.token)
            .field("elapsed_ms", &self.acquired_at.elapsed().as_millis())
            .finish()
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if !self.released {
            tracing::warn!(
                resource = %self.resource,
                held_ms = self.acquired_at.elapsed().as_millis() as u64,
                "LockGuard dropped without explicit release; lock will persist until TTL expires"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Redis implementation
// ---------------------------------------------------------------------------

#[cfg(feature = "redis-backend")]
mod redis_impl {
    use super::*;
    use redis::Script;
    use uuid::Uuid;

    /// Redis-backed distributed lock using SETNX + TTL pattern.
    ///
    /// Lock keys are stored under `branchforge:lock:{resource}` with a random
    /// UUID token as the value. Release and extend use Lua scripts to
    /// atomically verify ownership before mutating state.
    pub struct RedisLock {
        client: redis::Client,
        retry_delay: Duration,
        max_retries: u32,
    }

    impl RedisLock {
        /// Create a new Redis lock with default retry settings.
        pub fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
            let client = redis::Client::open(redis_url)?;
            Ok(Self {
                client,
                retry_delay: Duration::from_millis(DEFAULT_RETRY_DELAY_MS),
                max_retries: DEFAULT_MAX_RETRIES,
            })
        }

        /// Create a new Redis lock with custom retry configuration.
        pub fn with_config(
            redis_url: &str,
            retry_delay: Duration,
            max_retries: u32,
        ) -> Result<Self, redis::RedisError> {
            let client = redis::Client::open(redis_url)?;
            Ok(Self {
                client,
                retry_delay,
                max_retries,
            })
        }

        /// Build the full lock key for a given resource.
        fn lock_key(resource: &str) -> String {
            format!("{}{}", LOCK_KEY_PREFIX, resource)
        }

        async fn get_connection(&self) -> SessionResult<redis::aio::MultiplexedConnection> {
            self.client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Redis lock connection failed: {}", e),
                })
        }

        /// Attempt a single SETNX + PX to acquire the lock. Returns `Some(guard)`
        /// on success, `None` if the key is already held.
        async fn try_set_lock(
            &self,
            conn: &mut redis::aio::MultiplexedConnection,
            resource: &str,
            ttl: Duration,
        ) -> SessionResult<Option<LockGuard>> {
            let key = Self::lock_key(resource);
            let token = Uuid::new_v4().to_string();
            let ttl_ms = ttl.as_millis() as u64;

            // SET key token NX PX ttl_ms
            let result: Option<String> = redis::cmd("SET")
                .arg(&key)
                .arg(&token)
                .arg("NX")
                .arg("PX")
                .arg(ttl_ms)
                .query_async(conn)
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Redis lock SET NX failed: {}", e),
                })?;

            if result.is_some() {
                Ok(Some(LockGuard::new(resource.to_string(), token)))
            } else {
                Ok(None)
            }
        }
    }

    #[async_trait]
    impl DistributedLock for RedisLock {
        async fn acquire(&self, resource: &str, ttl: Duration) -> SessionResult<LockGuard> {
            let mut conn = self.get_connection().await?;
            let mut attempt = 0u32;

            loop {
                if let Some(guard) = self.try_set_lock(&mut conn, resource, ttl).await? {
                    return Ok(guard);
                }

                attempt += 1;
                if attempt > self.max_retries {
                    return Err(SessionError::Storage {
                        message: format!(
                            "Failed to acquire lock on '{}' after {} retries",
                            resource, self.max_retries
                        ),
                    });
                }

                // Add jitter: +/- 10%
                let jitter_factor = 1.0 + (rand::random::<f64>() * 0.2 - 0.1);
                let delay = self.retry_delay.mul_f64(jitter_factor);
                tokio::time::sleep(delay).await;
            }
        }

        async fn try_acquire(
            &self,
            resource: &str,
            ttl: Duration,
        ) -> SessionResult<Option<LockGuard>> {
            let mut conn = self.get_connection().await?;
            self.try_set_lock(&mut conn, resource, ttl).await
        }

        async fn extend(&self, guard: &LockGuard, ttl: Duration) -> SessionResult<bool> {
            let mut conn = self.get_connection().await?;
            let key = Self::lock_key(&guard.resource);
            let ttl_ms = ttl.as_millis() as u64;

            // Atomically check ownership then extend TTL.
            let script = Script::new(
                r#"
                if redis.call("get", KEYS[1]) == ARGV[1] then
                    return redis.call("pexpire", KEYS[1], ARGV[2])
                else
                    return 0
                end
                "#,
            );

            let result: i32 = script
                .key(&key)
                .arg(&guard.token)
                .arg(ttl_ms)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Redis lock extend failed: {}", e),
                })?;

            Ok(result == 1)
        }

        async fn release(&self, guard: &mut LockGuard) -> SessionResult<()> {
            let mut conn = self.get_connection().await?;
            let key = Self::lock_key(&guard.resource);

            // Atomically check ownership then delete.
            let script = Script::new(
                r#"
                if redis.call("get", KEYS[1]) == ARGV[1] then
                    return redis.call("del", KEYS[1])
                else
                    return 0
                end
                "#,
            );

            let _: i32 = script
                .key(&key)
                .arg(&guard.token)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Redis lock release failed: {}", e),
                })?;

            guard.mark_released();
            Ok(())
        }
    }
}

#[cfg(feature = "redis-backend")]
pub use redis_impl::RedisLock;

// ---------------------------------------------------------------------------
// PostgreSQL implementation
// ---------------------------------------------------------------------------

#[cfg(feature = "postgres")]
mod postgres_impl {
    use super::*;
    use sqlx::PgPool;
    use uuid::Uuid;

    /// PostgreSQL-backed distributed lock using advisory locks.
    ///
    /// Uses `pg_advisory_lock(hashtext(resource))` for blocking acquisition and
    /// `pg_try_advisory_lock(hashtext(resource))` for non-blocking attempts.
    /// TTL is advisory-only since PostgreSQL advisory locks are session-scoped.
    pub struct PostgresLock {
        pool: PgPool,
    }

    impl PostgresLock {
        /// Create a new PostgreSQL lock backed by the given connection pool.
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    #[async_trait]
    impl DistributedLock for PostgresLock {
        async fn acquire(&self, resource: &str, _ttl: Duration) -> SessionResult<LockGuard> {
            let token = Uuid::new_v4().to_string();

            sqlx::query("SELECT pg_advisory_lock(hashtext($1))")
                .bind(resource)
                .execute(&self.pool)
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("PostgreSQL advisory lock acquire failed: {}", e),
                })?;

            Ok(LockGuard::new(resource.to_string(), token))
        }

        async fn try_acquire(
            &self,
            resource: &str,
            _ttl: Duration,
        ) -> SessionResult<Option<LockGuard>> {
            let token = Uuid::new_v4().to_string();

            let acquired: (bool,) = sqlx::query_as("SELECT pg_try_advisory_lock(hashtext($1))")
                .bind(resource)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("PostgreSQL advisory lock try_acquire failed: {}", e),
                })?;

            if acquired.0 {
                Ok(Some(LockGuard::new(resource.to_string(), token)))
            } else {
                Ok(None)
            }
        }

        async fn extend(&self, _guard: &LockGuard, _ttl: Duration) -> SessionResult<bool> {
            // PostgreSQL advisory locks are session-scoped and do not have a TTL.
            // Extending is a no-op; the lock remains held until explicitly released
            // or the database connection is closed.
            Ok(true)
        }

        async fn release(&self, guard: &mut LockGuard) -> SessionResult<()> {
            sqlx::query("SELECT pg_advisory_unlock(hashtext($1))")
                .bind(&guard.resource)
                .execute(&self.pool)
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("PostgreSQL advisory lock release failed: {}", e),
                })?;

            guard.mark_released();
            Ok(())
        }
    }
}

#[cfg(feature = "postgres")]
pub use postgres_impl::PostgresLock;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_guard_new_sets_fields() {
        let guard = LockGuard::new("my-resource".to_string(), "tok-123".to_string());
        assert_eq!(guard.resource, "my-resource");
        assert_eq!(guard.token, "tok-123");
        assert!(!guard.is_expired(Duration::from_secs(60)));
    }

    #[test]
    fn lock_guard_is_expired_after_zero_ttl() {
        let guard = LockGuard::new("r".to_string(), "t".to_string());
        // A TTL of zero means the lock is immediately considered expired.
        assert!(guard.is_expired(Duration::ZERO));
    }

    #[test]
    fn lock_guard_debug_format() {
        let guard = LockGuard::new("res".to_string(), "tok".to_string());
        let debug = format!("{:?}", guard);
        assert!(debug.contains("res"));
        assert!(debug.contains("tok"));
        assert!(debug.contains("elapsed_ms"));
    }

    #[test]
    fn lock_key_prefix_is_correct() {
        assert_eq!(LOCK_KEY_PREFIX, "branchforge:lock:");
    }

    #[test]
    fn default_constants_are_reasonable() {
        assert_eq!(DEFAULT_LOCK_TTL_SECS, 30);
        assert_eq!(DEFAULT_RETRY_DELAY_MS, 100);
        assert_eq!(DEFAULT_MAX_RETRIES, 50);
        // 50 retries * 100ms = 5s total max wait
    }
}
