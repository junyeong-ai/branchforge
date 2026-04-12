//! Agent directory — registry of active agents.
//!
//! Manages agent lifecycle and provides inter-agent messaging via
//! named lookup. Agents register on spawn and are cleaned up on completion.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use dashmap::DashMap;

use super::messaging::{AgentMessage, MessageChannel};

crate::uuid_id!(AgentId);

/// Agent execution status.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DirectoryEntryStatus {
    Running = 0,
    Completed = 1,
    Failed = 2,
}

impl From<u8> for DirectoryEntryStatus {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Running,
            1 => Self::Completed,
            _ => Self::Failed,
        }
    }
}

/// Handle to a registered agent.
///
/// Provides messaging and status tracking. Handles are stored in
/// [`AgentDirectory`] and can be looked up by name or ID.
pub struct AgentHandle {
    /// Unique agent identifier.
    pub id: AgentId,
    /// Human-readable agent name (used for lookup).
    pub name: String,
    /// Communication channel for sending messages to this agent.
    pub(crate) channel: Arc<MessageChannel>,
    /// Current execution status (atomic for lock-free reads).
    status: AtomicU8,
    /// Last result text (populated on completion).
    last_result: tokio::sync::RwLock<Option<String>>,
}

impl AgentHandle {
    pub fn new(name: impl Into<String>, channel: Arc<MessageChannel>) -> Self {
        Self {
            id: AgentId::new(),
            name: name.into(),
            channel,
            status: AtomicU8::new(DirectoryEntryStatus::Running as u8),
            last_result: tokio::sync::RwLock::new(None),
        }
    }

    pub fn status(&self) -> DirectoryEntryStatus {
        DirectoryEntryStatus::from(self.status.load(Ordering::Acquire))
    }

    pub fn is_running(&self) -> bool {
        self.status() == DirectoryEntryStatus::Running
    }

    pub fn mark_completed(&self, result: Option<String>) {
        self.status
            .store(DirectoryEntryStatus::Completed as u8, Ordering::Release);
        if let Ok(mut guard) = self.last_result.try_write() {
            *guard = result;
        }
    }

    pub fn mark_failed(&self) {
        self.status
            .store(DirectoryEntryStatus::Failed as u8, Ordering::Release);
    }

    pub async fn last_result(&self) -> Option<String> {
        self.last_result.read().await.clone()
    }

    /// Send a message to this agent.
    pub async fn send(&self, from: AgentId, content: impl Into<String>) -> crate::Result<()> {
        let msg = AgentMessage::new(from, self.id, content);
        self.channel.send(msg).await.map_err(|_| {
            crate::Error::Session(crate::session::SessionError::ChannelClosed {
                message: format!("Agent '{}' channel closed", self.name),
            })
        })
    }
}

/// Registry of active agents with name-based lookup.
///
/// Thread-safe via [`DashMap`]. Agents register on spawn and can be
/// looked up by name or ID for messaging.
pub struct AgentDirectory {
    by_id: DashMap<AgentId, Arc<AgentHandle>>,
    by_name: DashMap<String, AgentId>,
}

impl AgentDirectory {
    pub fn new() -> Self {
        Self {
            by_id: DashMap::new(),
            by_name: DashMap::new(),
        }
    }

    /// Register an agent handle in the directory.
    pub fn register(&self, handle: Arc<AgentHandle>) {
        self.by_name.insert(handle.name.clone(), handle.id);
        self.by_id.insert(handle.id, handle);
    }

    /// Look up an agent by name.
    pub fn get_by_name(&self, name: &str) -> Option<Arc<AgentHandle>> {
        let id = self.by_name.get(name)?;
        self.by_id.get(&id).map(|h| h.value().clone())
    }

    /// Look up an agent by ID.
    pub fn get(&self, id: &AgentId) -> Option<Arc<AgentHandle>> {
        self.by_id.get(id).map(|h| h.value().clone())
    }

    /// Send a message to a named agent.
    ///
    /// Returns an error if the agent is not found or has completed.
    /// If completed, the error includes the agent's last result for context.
    pub async fn send(
        &self,
        from: AgentId,
        to_name: &str,
        content: impl Into<String>,
    ) -> crate::Result<()> {
        let handle = self.get_by_name(to_name).ok_or_else(|| {
            crate::Error::Config(format!("Agent '{}' not found in directory", to_name))
        })?;

        if !handle.is_running() {
            let last = handle.last_result().await.unwrap_or_default();
            return Err(crate::Error::Session(
                crate::session::SessionError::ChannelClosed {
                    message: format!(
                        "Agent '{}' has already completed. Last result: {}",
                        to_name,
                        if last.len() > 500 {
                            format!("{}...", &last[..500])
                        } else {
                            last
                        }
                    ),
                },
            ));
        }

        handle.send(from, content).await
    }

    /// Remove an agent from the directory.
    pub fn remove(&self, id: &AgentId) -> Option<Arc<AgentHandle>> {
        let handle = self.by_id.remove(id).map(|(_, h)| h)?;
        self.by_name.remove(&handle.name);
        Some(handle)
    }

    /// Remove all completed/failed agents.
    pub fn cleanup_finished(&self) {
        let finished: Vec<AgentId> = self
            .by_id
            .iter()
            .filter(|entry| !entry.value().is_running())
            .map(|entry| *entry.key())
            .collect();

        for id in finished {
            self.remove(&id);
        }
    }

    /// Number of registered agents.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the directory is empty.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// List all registered agent names.
    pub fn agent_names(&self) -> Vec<String> {
        self.by_name.iter().map(|e| e.key().clone()).collect()
    }
}

impl Default for AgentDirectory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_lookup() {
        let dir = AgentDirectory::new();
        let ch = Arc::new(MessageChannel::new(8));
        let handle = Arc::new(AgentHandle::new("worker-1", ch));
        let id = handle.id;

        dir.register(handle);

        assert_eq!(dir.len(), 1);
        assert!(dir.get_by_name("worker-1").is_some());
        assert!(dir.get(&id).is_some());
        assert!(dir.get_by_name("nonexistent").is_none());
    }

    #[tokio::test]
    async fn send_to_running_agent() {
        let dir = AgentDirectory::new();
        let ch = Arc::new(MessageChannel::new(8));
        let handle = Arc::new(AgentHandle::new("worker-1", ch.clone()));
        let sender_id = AgentId::new();

        dir.register(handle);
        dir.send(sender_id, "worker-1", "do something")
            .await
            .unwrap();

        let received = ch.recv().await.unwrap();
        assert_eq!(received.content, "do something");
    }

    #[tokio::test]
    async fn send_to_completed_agent_fails() {
        let dir = AgentDirectory::new();
        let ch = Arc::new(MessageChannel::new(8));
        let handle = Arc::new(AgentHandle::new("worker-1", ch));
        handle.mark_completed(Some("done".into()));

        dir.register(handle);

        let result = dir.send(AgentId::new(), "worker-1", "more work").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cleanup_finished() {
        let dir = AgentDirectory::new();

        let ch1 = Arc::new(MessageChannel::new(8));
        let h1 = Arc::new(AgentHandle::new("running", ch1));
        dir.register(h1);

        let ch2 = Arc::new(MessageChannel::new(8));
        let h2 = Arc::new(AgentHandle::new("done", ch2));
        h2.mark_completed(None);
        dir.register(h2);

        assert_eq!(dir.len(), 2);
        dir.cleanup_finished();
        assert_eq!(dir.len(), 1);
        assert!(dir.get_by_name("running").is_some());
    }
}
