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

        if keywords.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored: Vec<(f64, &MemoryEntry)> = entries
            .iter()
            .filter_map(|entry| {
                let summary_lower = entry.summary.to_lowercase();
                let summary_matched = keywords
                    .iter()
                    .filter(|kw| summary_lower.contains(*kw))
                    .count();

                let tags_lower: Vec<String> = entry.tags.iter().map(|t| t.to_lowercase()).collect();

                // Count keywords that match any tag (exact tag match or tag contains keyword)
                let tag_matched = keywords
                    .iter()
                    .filter(|kw| {
                        tags_lower
                            .iter()
                            .any(|tag| tag == **kw || tag.contains(*kw))
                    })
                    .count();

                // Count exact tag matches (query keyword == tag exactly) for boosting
                let exact_tag_matches = keywords
                    .iter()
                    .filter(|kw| tags_lower.iter().any(|tag| tag == **kw))
                    .count();

                let total_matched = summary_matched.max(tag_matched);
                if total_matched > 0 {
                    let base_score = total_matched as f64 / keywords.len() as f64;
                    // Boost by 0.25 per exact tag match, capped at 1.0
                    let boost = exact_tag_matches as f64 * 0.25;
                    let score = (base_score + boost).min(2.0);
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

    // -----------------------------------------------------------------------
    // Original tests
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Tag-based search tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_tag_based_search() {
        let store = InMemoryStore::new();
        // Summary does NOT mention "error", but it has an "error" tag.
        store
            .add(
                MemoryEntry::new("s1", "Refactored the logging pipeline")
                    .with_tags(vec!["error".into(), "observability".into()]),
            )
            .await
            .unwrap();
        // Summary also doesn't mention "error", no tags either.
        store
            .add(MemoryEntry::new("s2", "Updated the CSS grid layout"))
            .await
            .unwrap();

        let results = store.search("error", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "s1");
    }

    #[tokio::test]
    async fn test_tag_boost_scoring() {
        let store = InMemoryStore::new();
        // Entry with "architecture" only in summary text.
        store
            .add(MemoryEntry::new(
                "summary-only",
                "Discussed architecture patterns for the service layer",
            ))
            .await
            .unwrap();
        // Entry with "architecture" as a tag AND in summary.
        store
            .add(
                MemoryEntry::new(
                    "tag-and-summary",
                    "Discussed architecture of the event system",
                )
                .with_tags(vec!["architecture".into()]),
            )
            .await
            .unwrap();

        let results = store.search("architecture", 10).await.unwrap();
        assert_eq!(results.len(), 2);
        // The entry with the tag match should be boosted above the summary-only match.
        assert_eq!(results[0].session_id, "tag-and-summary");
        assert!(results[0].score.unwrap() > results[1].score.unwrap());
    }

    // -----------------------------------------------------------------------
    // Session isolation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_cross_session_isolation() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new("session-a", "Kubernetes deployment"))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new("session-b", "Kubernetes networking"))
            .await
            .unwrap();

        // Both sessions should appear in search results since search is global.
        let results = store.search("kubernetes", 10).await.unwrap();
        assert_eq!(results.len(), 2);

        // Remove session A.
        store.remove_session("session-a").await.unwrap();

        let results = store.search("kubernetes", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "session-b");
    }

    // -----------------------------------------------------------------------
    // Concurrency
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_concurrent_add_and_search() {
        let store = Arc::new(InMemoryStore::new());

        // Spawn writers.
        let mut handles = Vec::new();
        for i in 0..20 {
            let s = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                s.add(MemoryEntry::new(
                    format!("s{}", i),
                    format!("concurrent entry number {}", i),
                ))
                .await
                .unwrap();
            }));
        }

        // Spawn readers interleaved.
        for _ in 0..10 {
            let s = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                let _ = s.search("concurrent", 50).await.unwrap();
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // All 20 entries were added despite concurrent reads.
        assert_eq!(store.count().await.unwrap(), 20);

        // Search still works correctly after concurrent operations.
        let results = store.search("concurrent", 50).await.unwrap();
        assert_eq!(results.len(), 20);

        // Each entry has a unique session ID.
        let mut session_ids: Vec<String> = results.iter().map(|r| r.session_id.clone()).collect();
        session_ids.sort();
        session_ids.dedup();
        assert_eq!(session_ids.len(), 20);

        // Scores are populated for all results.
        assert!(results.iter().all(|r| r.score.is_some()));
    }

    // -----------------------------------------------------------------------
    // Unicode
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_unicode_search() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new("s1", "Discussed the Straße routing bug"))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new("s2", "Unicode emoji test 🦀 Rust"))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new("s3", "日本語テスト"))
            .await
            .unwrap();

        let results = store.search("straße", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "s1");

        let results = store.search("🦀", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "s2");

        let results = store.search("日本語", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "s3");
    }

    // -----------------------------------------------------------------------
    // Empty query
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_empty_query() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new("s1", "some content"))
            .await
            .unwrap();

        // Empty string has no keywords, should return nothing.
        let results = store.search("", 10).await.unwrap();
        assert!(results.is_empty());

        // Whitespace-only should also return nothing.
        let results = store.search("   ", 10).await.unwrap();
        assert!(results.is_empty());
    }

    // -----------------------------------------------------------------------
    // Special characters
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_special_characters() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new("s1", "null-pointer exception in module_a"))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new("s2", "fixed: off-by-one error"))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new("s3", "user@example.com sent request"))
            .await
            .unwrap();

        // Hyphens
        let results = store.search("null-pointer", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "s1");

        // Underscores
        let results = store.search("module_a", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "s1");

        // Punctuation embedded in text
        let results = store.search("user@example.com", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "s3");
    }

    // -----------------------------------------------------------------------
    // Case insensitivity
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_case_insensitive_search() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new("s1", "OAuth2 Token Refresh"))
            .await
            .unwrap();

        for query in &["oauth2", "OAUTH2", "OAuth2", "oAuth2 TOKEN"] {
            let results = store.search(query, 10).await.unwrap();
            assert!(
                !results.is_empty(),
                "Expected results for query '{query}' but got none"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Large store performance
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_large_store_performance() {
        let store = InMemoryStore::new();
        for i in 0..1500 {
            store
                .add(MemoryEntry::new(
                    format!("s{}", i),
                    format!("entry number {} about topic {}", i, i % 10),
                ))
                .await
                .unwrap();
        }

        let start = std::time::Instant::now();
        let results = store.search("topic", 10).await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(results.len(), 10);
        // Should complete well under 1 second for keyword matching.
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "Search took too long: {:?}",
            elapsed
        );
    }

    // -----------------------------------------------------------------------
    // Limit edge cases
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_limit_zero_returns_empty() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new("s1", "important data"))
            .await
            .unwrap();

        let results = store.search("important", 0).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_limit_larger_than_results() {
        let store = InMemoryStore::new();
        store.add(MemoryEntry::new("s1", "alpha")).await.unwrap();
        store
            .add(MemoryEntry::new("s2", "alpha beta"))
            .await
            .unwrap();

        let results = store.search("alpha", 100).await.unwrap();
        assert_eq!(results.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Duplicate session entries
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_duplicate_session_entries() {
        let store = InMemoryStore::new();
        store
            .add(MemoryEntry::new("s1", "first compaction summary"))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new("s1", "second compaction summary"))
            .await
            .unwrap();
        store
            .add(MemoryEntry::new("s1", "third compaction summary"))
            .await
            .unwrap();

        assert_eq!(store.count().await.unwrap(), 3);

        let results = store.search("compaction", 10).await.unwrap();
        assert_eq!(results.len(), 3);
        // All from the same session.
        assert!(results.iter().all(|r| r.session_id == "s1"));
    }

    // -----------------------------------------------------------------------
    // Remove nonexistent session
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_remove_nonexistent_session() {
        let store = InMemoryStore::new();
        store.add(MemoryEntry::new("s1", "data")).await.unwrap();

        let removed = store.remove_session("nonexistent").await.unwrap();
        assert_eq!(removed, 0);
        // Original data intact.
        assert_eq!(store.count().await.unwrap(), 1);
    }

    // -----------------------------------------------------------------------
    // MemoryEntry builder methods
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_entry_with_score() {
        let entry = MemoryEntry::new("s1", "test").with_score(0.95);
        assert_eq!(entry.score, Some(0.95));
        assert_eq!(entry.session_id, "s1");
        assert_eq!(entry.summary, "test");
    }

    #[tokio::test]
    async fn test_entry_with_tags() {
        let tags = vec!["error".to_string(), "production".to_string()];
        let entry = MemoryEntry::new("s1", "incident").with_tags(tags.clone());
        assert_eq!(entry.tags, tags);
        assert_eq!(entry.session_id, "s1");
        assert_eq!(entry.summary, "incident");
    }
}
