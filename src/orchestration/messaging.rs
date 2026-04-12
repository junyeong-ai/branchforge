//! Inter-agent messaging types.

#![allow(missing_docs)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::directory::AgentId;

/// Message sent between agents via [`AgentDirectory`](super::AgentDirectory).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    /// Sender agent identifier.
    pub from: AgentId,
    /// Recipient agent identifier.
    pub to: AgentId,
    /// Message content (instruction or follow-up).
    pub content: String,
    /// When the message was created.
    pub timestamp: DateTime<Utc>,
}

impl AgentMessage {
    pub fn new(from: AgentId, to: AgentId, content: impl Into<String>) -> Self {
        Self {
            from,
            to,
            content: content.into(),
            timestamp: Utc::now(),
        }
    }
}

/// Abstraction over the communication channel between agents.
///
/// Currently uses `tokio::sync::mpsc`. Future implementations could
/// support cross-process channels for distributed scenarios.
pub struct MessageChannel {
    pub(crate) sender: tokio::sync::mpsc::Sender<AgentMessage>,
    pub(crate) receiver: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<AgentMessage>>,
}

impl MessageChannel {
    pub fn new(buffer: usize) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(buffer);
        Self {
            sender,
            receiver: tokio::sync::Mutex::new(receiver),
        }
    }

    pub async fn send(&self, message: AgentMessage) -> Result<(), AgentMessage> {
        self.sender.send(message).await.map_err(|e| e.0)
    }

    pub async fn recv(&self) -> Option<AgentMessage> {
        self.receiver.lock().await.recv().await
    }

    pub fn try_recv(&self) -> Option<AgentMessage> {
        self.receiver
            .try_lock()
            .ok()
            .and_then(|mut rx| rx.try_recv().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn message_roundtrip() {
        let ch = MessageChannel::new(8);
        let msg = AgentMessage::new(AgentId::new(), AgentId::new(), "hello");
        let to = msg.to;
        ch.send(msg).await.unwrap();
        let received = ch.recv().await.unwrap();
        assert_eq!(received.to, to);
        assert_eq!(received.content, "hello");
    }
}
