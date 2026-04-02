//! Remote trigger for event-driven agent execution.
//!
//! Provides webhook-style triggers that can be invoked via HTTP
//! or programmatically to start agent execution on demand.

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

/// Remote trigger manager.
///
/// Stores trigger configurations and provides invocation.
/// The actual agent execution is delegated to the caller.
pub struct RemoteTrigger {
    configs: std::collections::HashMap<Uuid, TriggerConfig>,
}

impl RemoteTrigger {
    pub fn new() -> Self {
        Self {
            configs: std::collections::HashMap::new(),
        }
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

    /// Create a payload for invoking a trigger.
    pub fn invoke(&self, id: &Uuid) -> Option<TriggerPayload> {
        self.configs.get(id).map(|_| TriggerPayload::new(*id))
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
}
