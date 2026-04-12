//! Input queue for handling concurrent user inputs.

use std::collections::VecDeque;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::state::SessionId;
use super::types::EnvironmentContext;

const MAX_QUEUE_SIZE: usize = 100;
const MAX_MERGE_CHARS: usize = 100_000;

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueError {
    Full,
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "queue is full"),
        }
    }
}

impl std::error::Error for QueueError {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueuedInput {
    pub id: Uuid,
    pub session_id: SessionId,
    pub content: String,
    pub environment: Option<EnvironmentContext>,
    pub created_at: DateTime<Utc>,
}

impl QueuedInput {
    pub fn new(session_id: SessionId, content: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            content: content.into(),
            environment: None,
            created_at: Utc::now(),
        }
    }

    pub fn environment(mut self, env: EnvironmentContext) -> Self {
        self.environment = Some(env);
        self
    }
}

#[derive(Clone, Debug)]
pub struct MergedInput {
    pub ids: Vec<Uuid>,
    pub content: String,
    pub environment: Option<EnvironmentContext>,
}

#[derive(Debug)]
pub struct InputQueue {
    items: VecDeque<QueuedInput>,
}

impl Default for InputQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl InputQueue {
    pub fn new() -> Self {
        Self {
            items: VecDeque::with_capacity(16),
        }
    }

    pub fn enqueue(&mut self, input: QueuedInput) -> Result<Uuid, QueueError> {
        // A caller may read `pending_count()` (read lock) then call `enqueue()` (write lock).
        // Between those calls another enqueue could succeed, so the queue could briefly
        // hold MAX_QUEUE_SIZE + 1 items. This is bounded and harmless.
        if self.items.len() >= MAX_QUEUE_SIZE {
            return Err(QueueError::Full);
        }
        let id = input.id;
        self.items.push_back(input);
        Ok(id)
    }

    pub fn cancel(&mut self, id: Uuid) -> Option<QueuedInput> {
        self.items
            .iter()
            .position(|i| i.id == id)
            .and_then(|pos| self.items.remove(pos))
    }

    pub fn cancel_all(&mut self) -> Vec<QueuedInput> {
        self.items.drain(..).collect()
    }

    pub fn pending(&self) -> impl Iterator<Item = &QueuedInput> {
        self.items.iter()
    }

    pub fn pending_count(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn merge_all(&mut self) -> Option<MergedInput> {
        if self.items.is_empty() {
            return None;
        }

        let mut ids = Vec::with_capacity(self.items.len());
        let mut total_len = 0;
        let mut contents = Vec::with_capacity(self.items.len());
        let mut environment = None;

        while let Some(item) = self.items.pop_front() {
            let item_len = item.content.len();
            if total_len + item_len > MAX_MERGE_CHARS && !contents.is_empty() {
                self.items.push_front(item);
                break;
            }
            ids.push(item.id);
            total_len += item_len + 1;
            environment = item.environment.or(environment);
            contents.push(item.content);
        }

        let content = contents.join("\n");
        Some(MergedInput {
            ids,
            content,
            environment,
        })
    }

    pub fn dequeue(&mut self) -> Option<QueuedInput> {
        self.items.pop_front()
    }
}

#[derive(Clone)]
pub struct SharedInputQueue {
    inner: Arc<RwLock<InputQueue>>,
}

impl SharedInputQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(InputQueue::new())),
        }
    }

    pub async fn enqueue(&self, input: QueuedInput) -> Result<Uuid, QueueError> {
        self.inner.write().await.enqueue(input)
    }

    pub async fn cancel(&self, id: Uuid) -> Option<QueuedInput> {
        self.inner.write().await.cancel(id)
    }

    pub async fn cancel_all(&self) -> Vec<QueuedInput> {
        self.inner.write().await.cancel_all()
    }

    pub async fn pending_count(&self) -> usize {
        self.inner.read().await.pending_count()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    pub async fn merge_all(&self) -> Option<MergedInput> {
        self.inner.write().await.merge_all()
    }

    pub async fn dequeue(&self) -> Option<QueuedInput> {
        self.inner.write().await.dequeue()
    }

    pub async fn pending_ids(&self) -> Vec<Uuid> {
        self.inner.read().await.pending().map(|i| i.id).collect()
    }
}

impl Default for SharedInputQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SharedInputQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedInputQueue").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_enqueue_dequeue() {
        let mut queue = InputQueue::new();
        let session_id = SessionId::new();

        let input = QueuedInput::new(session_id, "Hello");
        let id = queue.enqueue(input).unwrap();

        assert_eq!(queue.pending_count(), 1);

        let dequeued = queue.dequeue().unwrap();
        assert_eq!(dequeued.id, id);
        assert_eq!(dequeued.content, "Hello");
        assert!(queue.is_empty());
    }

    #[test]
    fn test_queue_size_limit() {
        let mut queue = InputQueue::new();
        let session_id = SessionId::new();

        for i in 0..MAX_QUEUE_SIZE {
            let input = QueuedInput::new(session_id, format!("Message {}", i));
            assert!(queue.enqueue(input).is_ok());
        }

        let input = QueuedInput::new(session_id, "Overflow");
        assert_eq!(queue.enqueue(input), Err(QueueError::Full));
    }

    #[test]
    fn test_queue_cancel() {
        let mut queue = InputQueue::new();
        let session_id = SessionId::new();

        let input1 = QueuedInput::new(session_id, "First");
        let id1 = queue.enqueue(input1).unwrap();

        let input2 = QueuedInput::new(session_id, "Second");
        let _id2 = queue.enqueue(input2).unwrap();

        assert_eq!(queue.pending_count(), 2);

        let cancelled = queue.cancel(id1);
        assert!(cancelled.is_some());
        assert_eq!(cancelled.unwrap().content, "First");
        assert_eq!(queue.pending_count(), 1);
    }

    #[test]
    fn test_queue_merge_single() {
        let mut queue = InputQueue::new();
        let session_id = SessionId::new();

        let input = QueuedInput::new(session_id, "Only one");
        queue.enqueue(input).unwrap();

        let merged = queue.merge_all().unwrap();
        assert_eq!(merged.ids.len(), 1);
        assert_eq!(merged.content, "Only one");
        assert!(queue.is_empty());
    }

    #[test]
    fn test_queue_merge_multiple() {
        let mut queue = InputQueue::new();
        let session_id = SessionId::new();

        queue
            .enqueue(QueuedInput::new(session_id, "First"))
            .unwrap();
        queue
            .enqueue(QueuedInput::new(session_id, "Second"))
            .unwrap();
        queue
            .enqueue(QueuedInput::new(session_id, "Third"))
            .unwrap();

        let merged = queue.merge_all().unwrap();
        assert_eq!(merged.ids.len(), 3);
        assert_eq!(merged.content, "First\nSecond\nThird");
        assert!(queue.is_empty());
    }

    #[test]
    fn test_queue_merge_with_environment() {
        let mut queue = InputQueue::new();
        let session_id = SessionId::new();

        let env1 = EnvironmentContext {
            git_branch: Some("main".to_string()),
            ..Default::default()
        };
        let env2 = EnvironmentContext {
            git_branch: Some("feature".to_string()),
            ..Default::default()
        };

        queue
            .enqueue(QueuedInput::new(session_id, "First").environment(env1))
            .unwrap();
        queue
            .enqueue(QueuedInput::new(session_id, "Second").environment(env2))
            .unwrap();

        let merged = queue.merge_all().unwrap();
        assert_eq!(
            merged.environment.unwrap().git_branch,
            Some("feature".to_string())
        );
    }

    #[test]
    fn test_queue_merge_empty() {
        let mut queue = InputQueue::new();
        assert!(queue.merge_all().is_none());
    }

    #[test]
    fn test_queue_cancel_all() {
        let mut queue = InputQueue::new();
        let session_id = SessionId::new();

        queue
            .enqueue(QueuedInput::new(session_id, "First"))
            .unwrap();
        queue
            .enqueue(QueuedInput::new(session_id, "Second"))
            .unwrap();

        let cancelled = queue.cancel_all();
        assert_eq!(cancelled.len(), 2);
        assert!(queue.is_empty());
    }

    #[test]
    fn test_queue_merge_size_limit() {
        let mut queue = InputQueue::new();
        let session_id = SessionId::new();

        let large_content = "x".repeat(MAX_MERGE_CHARS / 2 + 1);
        queue
            .enqueue(QueuedInput::new(session_id, large_content.clone()))
            .unwrap();
        queue
            .enqueue(QueuedInput::new(session_id, large_content.clone()))
            .unwrap();
        queue
            .enqueue(QueuedInput::new(session_id, "Small"))
            .unwrap();

        let merged = queue.merge_all().unwrap();
        assert_eq!(merged.ids.len(), 1);
        assert!(!queue.is_empty());
        assert_eq!(queue.pending_count(), 2);
    }

    #[tokio::test]
    async fn test_shared_queue() {
        let queue = SharedInputQueue::new();
        let session_id = SessionId::new();

        let id = queue
            .enqueue(QueuedInput::new(session_id, "Test"))
            .await
            .unwrap();
        assert_eq!(queue.pending_count().await, 1);

        let cancelled = queue.cancel(id).await;
        assert!(cancelled.is_some());
        assert!(queue.is_empty().await);
    }
}
