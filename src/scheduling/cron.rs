//! Cron-based agent scheduling.
//!
//! Provides a lightweight cron scheduler built on tokio timers.
//! Each entry runs an async callback at the specified interval.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// A scheduled cron entry.
#[derive(Debug, Clone)]
pub struct CronEntry {
    /// Unique identifier.
    pub id: Uuid,
    /// Human-readable name.
    pub name: String,
    /// Execution interval.
    pub interval: Duration,
    /// When this entry was created.
    pub created_at: DateTime<Utc>,
    /// When this entry last ran.
    pub last_run: Option<DateTime<Utc>>,
    /// Whether this entry is enabled.
    pub enabled: bool,
}

/// Cron scheduler for periodic agent execution.
///
/// Each registered entry runs its callback at the specified interval
/// using tokio timers. The scheduler is stopped via its cancellation token.
///
/// # Example
///
/// ```rust,no_run
/// use branchforge::scheduling::CronScheduler;
/// use std::time::Duration;
///
/// # async fn example() {
/// let scheduler = CronScheduler::new();
/// scheduler.register("health-check", Duration::from_secs(300), || {
///     Box::pin(async { println!("Health check ran"); })
/// }).await;
/// // Tasks start running automatically after registration
/// # }
/// ```
pub struct CronScheduler {
    entries: Arc<RwLock<HashMap<Uuid, CronEntry>>>,
    cancel: CancellationToken,
    handles: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
}

impl CronScheduler {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            cancel: CancellationToken::new(),
            handles: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Register a periodic task.
    pub async fn register<F, Fut>(
        &self,
        name: impl Into<String>,
        interval: Duration,
        callback: F,
    ) -> Uuid
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let entry = CronEntry {
            id: Uuid::new_v4(),
            name: name.into(),
            interval,
            created_at: Utc::now(),
            last_run: None,
            enabled: true,
        };
        let id = entry.id;

        self.entries.write().await.insert(id, entry);

        let cancel = self.cancel.clone();
        let entries = self.entries.clone();

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(interval) => {
                        // Check if still enabled
                        let enabled = entries.read().await
                            .get(&id)
                            .is_some_and(|e| e.enabled);

                        if enabled {
                            callback().await;
                            if let Some(entry) = entries.write().await.get_mut(&id) {
                                entry.last_run = Some(Utc::now());
                            }
                        }
                    }
                }
            }
        });

        self.handles.write().await.push(handle);
        id
    }

    /// Unregister a scheduled task.
    pub async fn unregister(&self, id: &Uuid) -> bool {
        self.entries.write().await.remove(id).is_some()
    }

    /// Enable or disable a scheduled task without removing it.
    pub async fn set_enabled(&self, id: &Uuid, enabled: bool) -> bool {
        if let Some(entry) = self.entries.write().await.get_mut(id) {
            entry.enabled = enabled;
            true
        } else {
            false
        }
    }

    /// List all registered entries.
    pub async fn list(&self) -> Vec<CronEntry> {
        self.entries.read().await.values().cloned().collect()
    }

    /// Stop the scheduler and cancel all tasks.
    pub async fn stop(&self) {
        self.cancel.cancel();
        let handles: Vec<_> = self.handles.write().await.drain(..).collect();
        for handle in handles {
            let _ = handle.await;
        }
    }

    /// Number of registered entries.
    pub async fn len(&self) -> usize {
        self.entries.read().await.len()
    }

    /// Whether the scheduler has no entries.
    pub async fn is_empty(&self) -> bool {
        self.entries.read().await.is_empty()
    }
}

impl Default for CronScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CronScheduler {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn register_and_list() {
        let scheduler = CronScheduler::new();
        let id = scheduler
            .register("test", Duration::from_secs(60), || {
                Box::pin(async {})
            })
            .await;

        let entries = scheduler.list().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].name, "test");

        scheduler.stop().await;
    }

    #[tokio::test]
    async fn unregister() {
        let scheduler = CronScheduler::new();
        let id = scheduler
            .register("test", Duration::from_secs(60), || {
                Box::pin(async {})
            })
            .await;

        assert!(scheduler.unregister(&id).await);
        assert!(scheduler.is_empty().await);

        scheduler.stop().await;
    }

    #[tokio::test]
    async fn disable_prevents_execution() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let scheduler = CronScheduler::new();
        let id = scheduler
            .register("test", Duration::from_millis(10), move || {
                let c = counter_clone.clone();
                Box::pin(async move {
                    c.fetch_add(1, Ordering::Relaxed);
                })
            })
            .await;

        // Disable immediately
        scheduler.set_enabled(&id, false).await;

        // Wait a bit
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Should not have run
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        scheduler.stop().await;
    }
}
