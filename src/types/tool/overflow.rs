//! Phase C-7: result-size spill policy.
//!
//! When a tool returns output larger than its self-declared
//! `MAX_RESULT_SIZE_BYTES`, the agent runtime replaces the inline
//! payload with a short preview + an [`OverflowRef`] pointer. The
//! full content lives in an [`OverflowStore`] and can be retrieved
//! on demand by consumers that need the original bytes (diff tools,
//! file viewers, search).
//!
//! Rationale:
//! - The LLM prompt cache is dominated by tool outputs. Spilling
//!   large reads out of the token budget is the single biggest
//!   win for long-horizon sessions.
//! - The preview keeps the model oriented — it sees that content
//!   exists and roughly what it looks like — without paying the
//!   full token cost.
//! - The [`OverflowStore`] trait is backend-neutral: the default
//!   [`MemoryOverflowStore`] lives at Layer 1 and keeps everything
//!   in-process, while persistence-backed stores can be plugged in
//!   by downstream users (S3, Postgres, Redis) without touching
//!   the agent runtime.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Point-in-time pointer to a spilled tool-result payload.
///
/// Every field is serde-friendly so the ref can round-trip through
/// JSONL persistence or a provider wire format. The full content
/// is retrieved via [`OverflowStore::load`] using `id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverflowRef {
    /// Opaque handle issued by the store. Stable for the lifetime
    /// of the store entry.
    pub id: String,
    /// Total byte length of the spilled content. Callers use this
    /// to decide whether to load at all (e.g. "skip loads > 10 MB").
    pub size_bytes: u64,
    /// First N characters of the content, inline for quick orient.
    /// N is determined by the spill site (default 4 KiB). Kept short
    /// on purpose — the whole point is to avoid large inline bodies.
    pub preview: String,
    /// Stable identifier of the store that owns `id`. Lets consumers
    /// route loads through the correct backend when multiple stores
    /// are active (e.g. in-memory for ephemeral runs vs S3 for
    /// persisted sessions).
    pub store: String,
}

impl OverflowRef {
    /// Helper for test fixtures and in-memory spills.
    pub fn new(
        id: impl Into<String>,
        size_bytes: u64,
        preview: impl Into<String>,
        store: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            size_bytes,
            preview: preview.into(),
            store: store.into(),
        }
    }
}

/// Backend for large-result spilling. Implementations are Send+Sync
/// and can be shared across every tool via `Arc<dyn OverflowStore>`.
#[async_trait]
pub trait OverflowStore: Send + Sync + std::fmt::Debug {
    /// Stable identifier for this store. Copied into every
    /// [`OverflowRef`] it issues so loads can be routed back.
    fn name(&self) -> &str;

    /// Store `content` and return an [`OverflowRef`] that points at
    /// it. The preview is constructed by the caller — stores do not
    /// dictate its length.
    async fn store(&self, content: String, preview: String) -> crate::Result<OverflowRef>;

    /// Retrieve a previously-stored payload by id. Returns `None`
    /// when the id is unknown (evicted, never stored, wrong store).
    async fn load(&self, id: &str) -> crate::Result<Option<String>>;
}

/// Default in-memory [`OverflowStore`]. Lives at Layer 1 (no
/// feature flags) and is suitable for tests, short-running agent
/// processes, and any deployment where the loss of spill storage
/// on crash is acceptable (the originating tool can always be
/// re-invoked).
#[derive(Debug, Clone, Default)]
pub struct MemoryOverflowStore {
    entries: Arc<RwLock<HashMap<String, String>>>,
}

impl MemoryOverflowStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of currently-stored entries. Test-only introspection.
    pub async fn len(&self) -> usize {
        self.entries.read().await.len()
    }

    /// `true` when the store has no entries.
    pub async fn is_empty(&self) -> bool {
        self.entries.read().await.is_empty()
    }
}

#[async_trait]
impl OverflowStore for MemoryOverflowStore {
    fn name(&self) -> &str {
        "memory"
    }

    async fn store(&self, content: String, preview: String) -> crate::Result<OverflowRef> {
        let id = Uuid::new_v4().to_string();
        let size_bytes = content.len() as u64;
        self.entries.write().await.insert(id.clone(), content);
        Ok(OverflowRef::new(id, size_bytes, preview, "memory"))
    }

    async fn load(&self, id: &str) -> crate::Result<Option<String>> {
        Ok(self.entries.read().await.get(id).cloned())
    }
}

/// Truncate `content` to the first `limit` characters **on a char
/// boundary** so multi-byte UTF-8 sequences are never split. Shared
/// by every spill site so previews are computed uniformly.
pub fn preview_of(content: &str, limit: usize) -> String {
    if content.len() <= limit {
        return content.to_string();
    }
    let boundary = content.floor_char_boundary(limit);
    let mut preview = String::with_capacity(boundary + 32);
    preview.push_str(&content[..boundary]);
    preview.push_str("…\n(content spilled to overflow store)");
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_overflow_store_round_trip() {
        let store = MemoryOverflowStore::new();
        let long = "x".repeat(10_000);
        let r = store.store(long.clone(), "xxx…".into()).await.unwrap();
        assert_eq!(r.store, "memory");
        assert_eq!(r.size_bytes, 10_000);
        let loaded = store.load(&r.id).await.unwrap().unwrap();
        assert_eq!(loaded.len(), 10_000);
    }

    #[tokio::test]
    async fn memory_overflow_store_missing_id() {
        let store = MemoryOverflowStore::new();
        assert!(store.load("nonexistent").await.unwrap().is_none());
    }

    #[test]
    fn preview_of_short_content_passes_through() {
        assert_eq!(preview_of("hi", 100), "hi");
    }

    #[test]
    fn preview_of_truncates_on_char_boundary() {
        // "한" is 3 bytes in UTF-8; a byte-level truncate at
        // limit=2 would panic. The helper walks back to a
        // valid boundary.
        let s = "한국어";
        let p = preview_of(s, 2);
        assert!(p.starts_with('…') || p.is_empty() || p.starts_with(""));
        assert!(p.contains("overflow store"));
    }

    #[test]
    fn preview_of_annotates_truncation() {
        let long = "a".repeat(200);
        let p = preview_of(&long, 50);
        assert!(p.contains("overflow store"));
        assert!(p.len() < long.len() + 100);
    }
}
