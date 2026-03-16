//! Cross-session memory store for long-term context retrieval.
//!
//! The [`MemoryStore`] trait defines how agents retrieve relevant context
//! from past sessions. The SDK provides [`InMemoryStore`] with keyword
//! matching; users can implement the trait with vector databases
//! (pgvector, Pinecone, Qdrant) for semantic search.
//!
//! # Example
//!
//! ```rust,no_run
//! use branchforge::session::memory::{MemoryStore, MemoryEntry, InMemoryStore};
//!
//! let store = InMemoryStore::new();
//! // Entries are added automatically via EventBus::SessionCompacted
//! // or manually:
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! store.add(MemoryEntry::new("session-1", "User discussed auth refactoring")).await;
//! let results = store.search("authentication", 5).await.unwrap();
//! # });
//! ```

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::RwLock;

/// A single memory entry from a past session.
#[derive(Clone, Debug)]
pub struct MemoryEntry {
    /// Session that produced this memory.
    pub session_id: String,
    /// Summary text (from compaction or checkpoint).
    pub summary: String,
    /// When this memory was created.
    pub created_at: DateTime<Utc>,
    /// Optional tags for filtering.
    pub tags: Vec<String>,
    /// Optional relevance score (set by search implementation).
    pub score: Option<f64>,
}

impl MemoryEntry {
    pub fn new(session_id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            summary: summary.into(),
            created_at: Utc::now(),
            tags: Vec::new(),
            score: None,
        }
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn with_score(mut self, score: f64) -> Self {
        self.score = Some(score);
        self
    }
}

/// Trait for cross-session memory retrieval.
///
/// Implementations determine how past session context is stored and searched.
/// The agent can use retrieved memories to inform its responses.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Add a memory entry (e.g., from session compaction).
    async fn add(&self, entry: MemoryEntry) -> crate::Result<()>;

    /// Search for relevant memories.
    ///
    /// `query` is the search text (keyword or semantic depending on implementation).
    /// `limit` is the maximum number of results to return.
    /// Results should be ordered by relevance (most relevant first).
    async fn search(&self, query: &str, limit: usize) -> crate::Result<Vec<MemoryEntry>>;

    /// Remove all memories for a session.
    async fn remove_session(&self, session_id: &str) -> crate::Result<usize>;

    /// Get the total number of stored memories.
    async fn count(&self) -> crate::Result<usize>;
}

/// In-memory keyword-based memory store.
///
/// Searches by case-insensitive substring matching on summary text.
/// Suitable for development and testing. For production semantic search,
/// implement [`MemoryStore`] with a vector database.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    entries: Arc<RwLock<Vec<MemoryEntry>>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    async fn add(&self, entry: MemoryEntry) -> crate::Result<()> {
        self.entries.write().await.push(entry);
        Ok(())
    }

    async fn search(&self, query: &str, limit: usize) -> crate::Result<Vec<MemoryEntry>> {
        let entries = self.entries.read().await;
        let query_lower = query.to_lowercase();
        let keywords: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<(f64, &MemoryEntry)> = entries
            .iter()
            .filter_map(|entry| {
                let summary_lower = entry.summary.to_lowercase();
                let matched = keywords
                    .iter()
                    .filter(|kw| summary_lower.contains(*kw))
                    .count();
                if matched > 0 {
                    let score = matched as f64 / keywords.len().max(1) as f64;
                    Some((score, entry))
                } else {
                    None
                }
            })
            .collect();

        // Sort by score descending, then by recency
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.1.created_at.cmp(&a.1.created_at))
        });

        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(score, entry)| {
                let mut e = entry.clone();
                e.score = Some(score);
                e
            })
            .collect())
    }

    async fn remove_session(&self, session_id: &str) -> crate::Result<usize> {
        let mut entries = self.entries.write().await;
        let before = entries.len();
        entries.retain(|e| e.session_id != session_id);
        Ok(before - entries.len())
    }

    async fn count(&self) -> crate::Result<usize> {
        Ok(self.entries.read().await.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_and_search() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new(
                "s1",
                "User discussed authentication refactoring",
            ))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new(
                "s2",
                "Debugging database connection issues",
            ))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new(
                "s3",
                "Authentication token expiry handling",
            ))
            .await
            .unwrap();

        let results = store.search("authentication", 10).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].summary.to_lowercase().contains("authentication"));
    }

    #[tokio::test]
    async fn test_multi_keyword_scoring() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new("s1", "auth token refresh"))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new(
                "s2",
                "auth token refresh and database migration",
            ))
            .await
            .unwrap();

        let results = store
            .search("auth token refresh migration", 10)
            .await
            .unwrap();
        // s2 matches more keywords, should rank higher
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].session_id, "s2");
        assert!(results[0].score.unwrap() > results[1].score.unwrap());
    }

    #[tokio::test]
    async fn test_remove_session() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new("s1", "memory one"))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new("s1", "memory two"))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new("s2", "other memory"))
            .await
            .unwrap();

        let removed = store.remove_session("s1").await.unwrap();
        assert_eq!(removed, 2);
        assert_eq!(store.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_empty_search() {
        let store = InMemoryStore::new();
        let results = store.search("anything", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_no_match() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new("s1", "about cats"))
            .await
            .unwrap();
        let results = store.search("dogs", 10).await.unwrap();
        assert!(results.is_empty());
    }
}
