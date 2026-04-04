//! Remote trigger for event-driven agent execution.
//!
//! Provides webhook-style triggers that can be invoked via HTTP
//! or programmatically to start agent execution on demand.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Configuration for a remote trigger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerConfig {
    /// Unique trigger identifier.
    pub id: Uuid,
    /// Human-readable name.
    pub name: String,
    /// Agent prompt to execute when triggered.
    pub prompt: String,
    /// Model to use for execution.
    pub model: Option<String>,
    /// Maximum execution time in seconds.
    pub timeout_secs: Option<u64>,
    /// Whether this trigger is enabled.
    pub enabled: bool,
    /// When this trigger was created.
    pub created_at: DateTime<Utc>,
    /// Last error message from a failed execution.
    pub last_error: Option<String>,
    /// Total number of completed invocations.
    pub run_count: u64,
}

impl TriggerConfig {
    pub fn new(name: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            prompt: prompt.into(),
            model: None,
            timeout_secs: None,
            enabled: true,
            created_at: Utc::now(),
            last_error: None,
            run_count: 0,
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }
}

/// Payload sent with a trigger invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerPayload {
    /// The trigger being invoked.
    pub trigger_id: Uuid,
    /// Optional additional context/instructions.
    pub context: Option<String>,
    /// Optional key-value metadata.
    pub metadata: Option<serde_json::Value>,
    /// When the trigger was invoked.
    pub invoked_at: DateTime<Utc>,
}

impl TriggerPayload {
    pub fn new(trigger_id: Uuid) -> Self {
        Self {
            trigger_id,
            context: None,
            metadata: None,
            invoked_at: Utc::now(),
        }
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }
}

/// Result of a trigger execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerResult {
    /// The trigger that was executed.
    pub trigger_id: Uuid,
    /// Whether execution succeeded.
    pub success: bool,
    /// Output from the execution callback.
    pub output: Option<String>,
    /// Error message if execution failed.
    pub error: Option<String>,
    /// When execution completed.
    pub completed_at: DateTime<Utc>,
}

/// Type alias for the async executor callback.
type ExecutorFn = dyn Fn(TriggerPayload) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
    + Send
    + Sync;

/// Remote trigger manager.
///
/// Stores trigger configurations and provides invocation and execution.
/// An optional executor callback handles the actual agent execution.
pub struct RemoteTrigger {
    configs: HashMap<Uuid, TriggerConfig>,
    executor: Option<Arc<ExecutorFn>>,
}

impl RemoteTrigger {
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            executor: None,
        }
    }

    /// Set the execution callback used by `execute` and `execute_with_context`.
    pub fn with_executor<F>(mut self, f: F) -> Self
    where
        F: Fn(TriggerPayload) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync
            + 'static,
    {
        self.executor = Some(Arc::new(f));
        self
    }

    /// Register a new trigger configuration.
    pub fn register(&mut self, config: TriggerConfig) -> Uuid {
        let id = config.id;
        self.configs.insert(id, config);
        id
    }

    /// Get a trigger configuration by ID.
    pub fn get(&self, id: &Uuid) -> Option<&TriggerConfig> {
        self.configs.get(id)
    }

    /// Get a trigger configuration by name.
    pub fn get_by_name(&self, name: &str) -> Option<&TriggerConfig> {
        self.configs.values().find(|c| c.name == name)
    }

    /// List all registered triggers.
    pub fn list(&self) -> Vec<&TriggerConfig> {
        self.configs.values().collect()
    }

    /// Remove a trigger.
    pub fn remove(&mut self, id: &Uuid) -> Option<TriggerConfig> {
        self.configs.remove(id)
    }

    /// Create a payload for invoking a trigger (without executing).
    pub fn invoke(&self, id: &Uuid) -> Option<TriggerPayload> {
        self.configs.get(id).map(|_| TriggerPayload::new(*id))
    }

    /// Invoke and execute a trigger using the registered executor.
    pub async fn execute(&mut self, id: &Uuid) -> Result<TriggerResult, String> {
        self.execute_inner(id, None).await
    }

    /// Invoke and execute a trigger with additional context.
    pub async fn execute_with_context(
        &mut self,
        id: &Uuid,
        context: String,
    ) -> Result<TriggerResult, String> {
        self.execute_inner(id, Some(context)).await
    }

    async fn execute_inner(
        &mut self,
        id: &Uuid,
        context: Option<String>,
    ) -> Result<TriggerResult, String> {
        let executor = self
            .executor
            .clone()
            .ok_or_else(|| "no executor configured".to_string())?;

        let config = self
            .configs
            .get(id)
            .ok_or_else(|| format!("trigger not found: {id}"))?;

        if !config.enabled {
            return Err(format!("trigger is disabled: {}", config.name));
        }

        let mut payload = TriggerPayload::new(*id);
        if let Some(ctx) = context {
            payload = payload.with_context(ctx);
        }

        let result = match executor(payload).await {
            Ok(output) => {
                let result = TriggerResult {
                    trigger_id: *id,
                    success: true,
                    output: Some(output),
                    error: None,
                    completed_at: Utc::now(),
                };
                // Update config on success
                if let Some(config) = self.configs.get_mut(id) {
                    config.run_count += 1;
                    config.last_error = None;
                }
                result
            }
            Err(err) => {
                let result = TriggerResult {
                    trigger_id: *id,
                    success: false,
                    output: None,
                    error: Some(err.clone()),
                    completed_at: Utc::now(),
                };
                // Update config on failure
                if let Some(config) = self.configs.get_mut(id) {
                    config.run_count += 1;
                    config.last_error = Some(err);
                }
                result
            }
        };

        Ok(result)
    }
}

impl Default for RemoteTrigger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_lookup() {
        let mut triggers = RemoteTrigger::new();
        let config = TriggerConfig::new("deploy-check", "Check deployment status");
        let id = triggers.register(config);

        assert!(triggers.get(&id).is_some());
        assert_eq!(
            triggers.get_by_name("deploy-check").unwrap().prompt,
            "Check deployment status"
        );
    }

    #[test]
    fn invoke_creates_payload() {
        let mut triggers = RemoteTrigger::new();
        let config = TriggerConfig::new("test", "Run tests");
        let id = triggers.register(config);

        let payload = triggers.invoke(&id).unwrap();
        assert_eq!(payload.trigger_id, id);
    }

    #[tokio::test]
    async fn execute_with_executor_success() {
        let mut triggers = RemoteTrigger::new().with_executor(|payload| {
            Box::pin(async move { Ok(format!("executed trigger {}", payload.trigger_id)) })
        });

        let config = TriggerConfig::new("test", "Run tests");
        let id = triggers.register(config);

        let result = triggers.execute(&id).await.unwrap();
        assert!(result.success);
        assert!(result.output.unwrap().contains("executed trigger"));
        assert!(result.error.is_none());

        // Check run_count incremented
        assert_eq!(triggers.get(&id).unwrap().run_count, 1);
        assert!(triggers.get(&id).unwrap().last_error.is_none());
    }

    #[tokio::test]
    async fn execute_with_executor_failure() {
        let mut triggers = RemoteTrigger::new().with_executor(|_payload| {
            Box::pin(async move { Err("something went wrong".to_string()) })
        });

        let config = TriggerConfig::new("failing", "This will fail");
        let id = triggers.register(config);

        let result = triggers.execute(&id).await.unwrap();
        assert!(!result.success);
        assert!(result.output.is_none());
        assert_eq!(result.error.as_deref(), Some("something went wrong"));

        // Check failure tracking
        assert_eq!(triggers.get(&id).unwrap().run_count, 1);
        assert_eq!(
            triggers.get(&id).unwrap().last_error.as_deref(),
            Some("something went wrong")
        );
    }

    #[tokio::test]
    async fn execute_without_executor_returns_error() {
        let mut triggers = RemoteTrigger::new();
        let config = TriggerConfig::new("test", "Run tests");
        let id = triggers.register(config);

        let result = triggers.execute(&id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no executor configured"));
    }

    #[tokio::test]
    async fn execute_disabled_trigger_returns_error() {
        let mut triggers =
            RemoteTrigger::new().with_executor(|_| Box::pin(async { Ok("ok".to_string()) }));

        let mut config = TriggerConfig::new("disabled", "Disabled trigger");
        config.enabled = false;
        let id = triggers.register(config);

        let result = triggers.execute(&id).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("disabled"));
    }

    #[tokio::test]
    async fn execute_with_context() {
        let mut triggers = RemoteTrigger::new().with_executor(|payload| {
            Box::pin(async move { Ok(format!("context: {}", payload.context.unwrap_or_default())) })
        });

        let config = TriggerConfig::new("ctx-test", "Context test");
        let id = triggers.register(config);

        let result = triggers
            .execute_with_context(&id, "extra info".to_string())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output.as_deref(), Some("context: extra info"));
    }

    #[test]
    fn trigger_config_has_failure_tracking_fields() {
        let config = TriggerConfig::new("test", "prompt");
        assert_eq!(config.run_count, 0);
        assert!(config.last_error.is_none());
    }
}
