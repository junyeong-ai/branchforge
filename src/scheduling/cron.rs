//! Cron-based agent scheduling.
//!
//! Provides a lightweight cron scheduler built on tokio timers.
//! Each entry runs an async callback at the specified interval or
//! according to a cron expression.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Scheduling strategy for a cron entry.
#[derive(Debug, Clone)]
pub enum CronSchedule {
    /// Fixed-interval scheduling.
    Interval(Duration),
    /// Cron expression scheduling (e.g. `"0 0/5 * * * *"` for every 5 minutes).
    Expression(Box<cron::Schedule>),
}

/// A scheduled cron entry.
#[derive(Debug, Clone)]
pub struct CronEntry {
    /// Unique identifier.
    pub id: Uuid,
    /// Human-readable name.
    pub name: String,
    /// Scheduling strategy.
    pub schedule: CronSchedule,
    /// When this entry was created.
    pub created_at: DateTime<Utc>,
    /// When this entry last ran.
    pub last_run: Option<DateTime<Utc>>,
    /// Whether this entry is enabled.
    pub enabled: bool,
    /// Whether the callback is currently executing.
    pub executing: Arc<AtomicBool>,
    /// Last error message from a failed execution.
    pub last_error: Option<String>,
    /// Total number of completed runs.
    pub run_count: u64,
}

/// Cron scheduler for periodic agent execution.
///
/// Each registered entry runs its callback at the specified interval
/// or cron schedule using tokio timers. The scheduler is stopped via
/// its cancellation token.
///
/// If a callback is still running when the next tick fires, that tick
/// is skipped and a warning is logged.
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

    /// Register a periodic task with a fixed interval.
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
        self.register_with_schedule(name, CronSchedule::Interval(interval), callback)
            .await
    }

    /// Register a task with a cron expression (6-field format: sec min hour day month weekday).
    ///
    /// Uses the `cron` crate expression format. For example:
    /// - `"0 */5 * * * *"` — every 5 minutes
    /// - `"0 0 9 * * Mon-Fri"` — weekdays at 9am
    pub async fn register_cron<F, Fut>(
        &self,
        name: impl Into<String>,
        expression: &str,
        callback: F,
    ) -> Result<Uuid, String>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let schedule: cron::Schedule = expression
            .parse()
            .map_err(|e| format!("invalid cron expression: {e}"))?;
        let id = self
            .register_with_schedule(name, CronSchedule::Expression(Box::new(schedule)), callback)
            .await;
        Ok(id)
    }

    /// Internal: register with any schedule type.
    async fn register_with_schedule<F, Fut>(
        &self,
        name: impl Into<String>,
        schedule: CronSchedule,
        callback: F,
    ) -> Uuid
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let executing = Arc::new(AtomicBool::new(false));
        let entry = CronEntry {
            id: Uuid::new_v4(),
            name: name.into(),
            schedule: schedule.clone(),
            created_at: Utc::now(),
            last_run: None,
            enabled: true,
            executing: executing.clone(),
            last_error: None,
            run_count: 0,
        };
        let id = entry.id;

        self.entries.write().await.insert(id, entry);

        let cancel = self.cancel.clone();
        let entries = self.entries.clone();

        let handle = tokio::spawn(async move {
            loop {
                let sleep_dur = match &schedule {
                    CronSchedule::Interval(dur) => *dur,
                    CronSchedule::Expression(sched) => {
                        let now = Utc::now();
                        match sched.upcoming(Utc).next() {
                            Some(next) => {
                                let delta = next - now;
                                delta.to_std().unwrap_or(Duration::from_secs(1))
                            }
                            None => Duration::from_secs(60),
                        }
                    }
                };

                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(sleep_dur) => {
                        // Check if still enabled
                        let enabled = entries.read().await
                            .get(&id)
                            .is_some_and(|e| e.enabled);

                        if !enabled {
                            continue;
                        }

                        // Concurrent execution guard
                        if executing
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_err()
                        {
                            tracing::warn!(
                                entry_id = %id,
                                "skipping tick: previous execution still running"
                            );
                            continue;
                        }

                        callback().await;

                        executing.store(false, Ordering::Release);

                        if let Some(entry) = entries.write().await.get_mut(&id) {
                            entry.last_run = Some(Utc::now());
                            entry.run_count += 1;
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

    /// Get a single entry by ID.
    pub async fn get(&self, id: &Uuid) -> Option<CronEntry> {
        self.entries.read().await.get(id).cloned()
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
            .register("test", Duration::from_secs(60), || Box::pin(async {}))
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
            .register("test", Duration::from_secs(60), || Box::pin(async {}))
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

    #[tokio::test]
    async fn cron_expression_parsing() {
        let scheduler = CronScheduler::new();

        // Valid expression
        let result = scheduler
            .register_cron("every-minute", "0 * * * * *", || Box::pin(async {}))
            .await;
        assert!(result.is_ok());

        // Invalid expression
        let result = scheduler
            .register_cron("bad", "not a cron expr", || Box::pin(async {}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid cron expression"));

        scheduler.stop().await;
    }

    #[tokio::test]
    async fn cron_expression_entry_has_correct_schedule() {
        let scheduler = CronScheduler::new();
        let id = scheduler
            .register_cron("five-min", "0 */5 * * * *", || Box::pin(async {}))
            .await
            .unwrap();

        let entry = scheduler.get(&id).await.unwrap();
        assert!(matches!(entry.schedule, CronSchedule::Expression(_)));
        assert_eq!(entry.run_count, 0);
        assert!(entry.last_error.is_none());

        scheduler.stop().await;
    }

    #[tokio::test]
    async fn concurrent_execution_guard_skips_when_busy() {
        let executing_flag = Arc::new(AtomicBool::new(false));
        let skip_count = Arc::new(AtomicU32::new(0));
        let run_count = Arc::new(AtomicU32::new(0));

        let executing_clone = executing_flag.clone();
        let run_count_clone = run_count.clone();

        let scheduler = CronScheduler::new();
        let id = scheduler
            .register("slow-task", Duration::from_millis(10), move || {
                let ef = executing_clone.clone();
                let rc = run_count_clone.clone();
                Box::pin(async move {
                    rc.fetch_add(1, Ordering::Relaxed);
                    // Simulate slow work
                    ef.store(true, Ordering::Release);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    ef.store(false, Ordering::Release);
                })
            })
            .await;

        // Wait long enough for several ticks to fire while the first is still running
        tokio::time::sleep(Duration::from_millis(100)).await;

        // The entry's executing flag should be true (first callback still running)
        let entry = scheduler.get(&id).await.unwrap();
        assert!(entry.executing.load(Ordering::Acquire));

        // Only 1 run should have started (others skipped)
        assert_eq!(run_count.load(Ordering::Relaxed), 1);
        let _ = skip_count;

        scheduler.stop().await;
    }

    #[tokio::test]
    async fn run_count_increments() {
        let scheduler = CronScheduler::new();
        let id = scheduler
            .register("fast", Duration::from_millis(10), || Box::pin(async {}))
            .await;

        // Let it run a few times
        tokio::time::sleep(Duration::from_millis(55)).await;

        let entry = scheduler.get(&id).await.unwrap();
        assert!(
            entry.run_count >= 2,
            "run_count should be >= 2, got {}",
            entry.run_count
        );
        assert!(entry.last_run.is_some());

        scheduler.stop().await;
    }
}
