//! JSONL-based persistence backend compatible with Claude Code CLI.
//!
//! This module provides file-based session persistence using the JSONL (JSON Lines) format,
//! matching the Claude Code CLI's storage format for full interoperability.
//!
//! # Features
//!
//! - **Claude Code CLI Compatible**: Uses the same file structure and format as the official CLI
//! - **DAG Structure**: Messages form a directed acyclic graph via parent_uuid references
//! - **Incremental Writes**: Only new entries are appended, avoiding full file rewrites
//! - **Project-Based Organization**: Sessions organized by encoded project paths
//! - **Async I/O**: Non-blocking file operations via tokio
//!
//! # File Structure
//!
//! ```text
//! ~/.claude/
//! └── projects/
//!     └── {encoded-project-path}/
//!         ├── {session-uuid}.jsonl    # Conversation history
//!         └── ...
//! ```

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::state::{MessageId, Session, SessionConfig, SessionId, SessionMessage, SessionType};
use super::types::{
    CompactRecord, Plan, QueueItem, QueueOperation, QueueStatus, SummarySnapshot, TodoItem,
};
use super::{Persistence, SessionError, SessionResult};
use crate::graph::{GraphEvent, GraphMaterializer};
use crate::types::{ContentBlock, Role, TokenUsage};

// ============================================================================
// Enum Serialization Helpers (consistent with persistence_postgres.rs)
// ============================================================================

/// Serialize an enum to its serde string representation for JSONL storage.
fn enum_to_jsonl<T: serde::Serialize>(value: &T, default: &str) -> String {
    serde_json::to_string(value)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|_| default.to_string())
}

/// Deserialize an enum from its serde string representation in JSONL storage.
fn jsonl_to_enum<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_str(&format!("\"{}\"", s)).ok()
}

// ============================================================================
// Configuration
// ============================================================================

/// Sync mode for file operations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyncMode {
    /// No explicit sync (OS buffering only).
    #[default]
    None,
    /// Sync after every write (safest, slowest).
    OnWrite,
}

/// Configuration for JSONL persistence.
#[derive(Clone, Debug)]
pub struct JsonlConfig {
    /// Base directory for storage (default: ~/.claude).
    pub base_dir: PathBuf,
    /// Log retention period in days (default: 30).
    pub retention_days: u32,
    /// File sync mode for durability.
    pub sync_mode: SyncMode,
}

impl Default for JsonlConfig {
    fn default() -> Self {
        Self {
            base_dir: crate::common::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".claude"),
            retention_days: 30,
            sync_mode: SyncMode::default(),
        }
    }
}

impl JsonlConfig {
    pub fn builder() -> JsonlConfigBuilder {
        JsonlConfigBuilder::default()
    }

    fn projects_dir(&self) -> PathBuf {
        self.base_dir.join("projects")
    }

    /// Encode a project path for use as a directory name using hex encoding.
    /// Each byte of the path is encoded as two hex characters, producing collision-free names.
    fn encode_project_path(&self, path: &Path) -> String {
        path.as_os_str()
            .as_encoded_bytes()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    }

    fn project_dir(&self, project_path: &Path) -> PathBuf {
        self.projects_dir()
            .join(self.encode_project_path(project_path))
    }
}

/// Builder for JsonlConfig.
#[derive(Default)]
pub struct JsonlConfigBuilder {
    base_dir: Option<PathBuf>,
    retention_days: Option<u32>,
    sync_mode: Option<SyncMode>,
}

impl JsonlConfigBuilder {
    pub fn base_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.base_dir = Some(path.into());
        self
    }

    pub fn retention_days(mut self, days: u32) -> Self {
        self.retention_days = Some(days);
        self
    }

    pub fn sync_mode(mut self, mode: SyncMode) -> Self {
        self.sync_mode = Some(mode);
        self
    }

    pub fn build(self) -> JsonlConfig {
        let default = JsonlConfig::default();
        JsonlConfig {
            base_dir: self.base_dir.unwrap_or(default.base_dir),
            retention_days: self.retention_days.unwrap_or(default.retention_days),
            sync_mode: self.sync_mode.unwrap_or(default.sync_mode),
        }
    }
}

// ============================================================================
// JSONL Entry Types (Claude Code CLI Compatible)
// ============================================================================

/// JSONL entry types matching Claude Code CLI format.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JsonlEntry {
    User(UserEntry),
    Assistant(AssistantEntry),
    GraphEvent(GraphEventEntry),
    Bookmark(BookmarkEntry),
    Checkpoint(CheckpointEntry),
    System(SystemEntry),
    QueueOperation(QueueOperationEntry),
    Summary(SummaryEntry),
    SessionMeta(SessionMetaEntry),
    Todo(TodoEntry),
    Plan(PlanEntry),
    Compact(CompactEntry),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphEventEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "eventId")]
    pub event_id: String,
    pub event: GraphEvent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BookmarkEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub id: String,
    #[serde(rename = "nodeId")]
    pub node_id: String,
    #[serde(rename = "branchId")]
    pub branch_id: String,
    pub label: String,
    pub note: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub id: String,
    #[serde(rename = "branchId")]
    pub branch_id: String,
    pub label: String,
    pub note: Option<String>,
    pub tags: Vec<String>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
}

/// Common fields shared by message entries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntryCommon {
    pub uuid: String,
    #[serde(rename = "parentUuid", skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    pub version: String,
    #[serde(rename = "gitBranch", default)]
    pub git_branch: String,
    #[serde(rename = "isSidechain", default)]
    pub is_sidechain: bool,
}

impl EntryCommon {
    fn from_message(session_id: &SessionId, msg: &SessionMessage) -> Self {
        Self {
            uuid: msg.id.to_string(),
            parent_uuid: msg.parent_id.as_ref().map(|id| id.to_string()),
            session_id: session_id.to_string(),
            timestamp: msg.timestamp,
            cwd: msg.environment.as_ref().and_then(|e| e.cwd.clone()),
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_branch: msg
                .environment
                .as_ref()
                .and_then(|e| e.git_branch.clone())
                .unwrap_or_default(),
            is_sidechain: msg.is_sidechain,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserMessageContent {
    pub role: String,
    pub content: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserEntry {
    #[serde(flatten)]
    pub common: EntryCommon,
    pub message: UserMessageContent,
    #[serde(rename = "isCompactSummary", default)]
    pub is_compact_summary: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantMessageContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    pub content: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageInfo>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl From<&TokenUsage> for UsageInfo {
    fn from(u: &TokenUsage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
        }
    }
}

impl From<&UsageInfo> for TokenUsage {
    fn from(u: &UsageInfo) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantEntry {
    #[serde(flatten)]
    pub common: EntryCommon,
    pub message: AssistantMessageContent,
    #[serde(rename = "requestId", skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemEntry {
    #[serde(flatten)]
    pub common: EntryCommon,
    pub subtype: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueueOperationEntry {
    pub operation: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub content: String,
    pub priority: i32,
    #[serde(rename = "itemId")]
    pub item_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummaryEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub summary: String,
    #[serde(rename = "leafUuid", skip_serializing_if = "Option::is_none")]
    pub leaf_uuid: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMetaEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "parentSessionId", skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(rename = "tenantId", skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(rename = "sessionType")]
    pub session_type: serde_json::Value,
    pub mode: String,
    pub state: String,
    pub config: serde_json::Value,
    #[serde(rename = "permissionPolicy")]
    pub permission_policy: serde_json::Value,
    #[serde(rename = "totalUsage", default)]
    pub total_usage: UsageInfo,
    #[serde(rename = "totalCostUsd", default)]
    pub total_cost_usd: Decimal,
    #[serde(rename = "staticContextHash", skip_serializing_if = "Option::is_none")]
    pub static_context_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "updatedAt")]
    pub updated_at: DateTime<Utc>,
    #[serde(rename = "expiresAt", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TodoEntry {
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub content: String,
    #[serde(rename = "activeForm")]
    pub active_form: String,
    pub status: String,
    #[serde(rename = "planId", skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(rename = "completedAt", skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanEntry {
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub content: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "approvedAt", skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<DateTime<Utc>>,
    #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(rename = "completedAt", skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactEntry {
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub trigger: String,
    #[serde(rename = "preTokens")]
    pub pre_tokens: usize,
    #[serde(rename = "postTokens")]
    pub post_tokens: usize,
    #[serde(rename = "savedTokens")]
    pub saved_tokens: usize,
    pub summary: String,
    #[serde(rename = "originalCount")]
    pub original_count: usize,
    #[serde(rename = "newCount")]
    pub new_count: usize,
    #[serde(rename = "logicalParentId", skip_serializing_if = "Option::is_none")]
    pub logical_parent_id: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
}

// ============================================================================
// Conversion: SessionMessage <-> JsonlEntry
// ============================================================================

impl JsonlEntry {
    fn from_message(session_id: &SessionId, msg: &SessionMessage) -> Self {
        let common = EntryCommon::from_message(session_id, msg);

        let serialize_content = |content: &[ContentBlock]| -> serde_json::Value {
            serde_json::to_value(content).unwrap_or_else(|e| {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "Failed to serialize message content, using empty array"
                );
                serde_json::Value::Array(Vec::new())
            })
        };

        match msg.role {
            Role::User => JsonlEntry::User(UserEntry {
                common,
                message: UserMessageContent {
                    role: "user".to_string(),
                    content: serialize_content(&msg.content),
                },
                is_compact_summary: msg.is_compact_summary,
            }),
            Role::Assistant => JsonlEntry::Assistant(AssistantEntry {
                common,
                message: AssistantMessageContent {
                    id: msg.metadata.request_id.clone(),
                    model: msg.metadata.model.clone(),
                    role: "assistant".to_string(),
                    stop_reason: None,
                    content: serialize_content(&msg.content),
                    usage: msg.usage.as_ref().map(UsageInfo::from),
                },
                request_id: msg.metadata.request_id.clone(),
            }),
        }
    }

    fn from_graph_event(session_id: &SessionId, event: &GraphEvent) -> Self {
        JsonlEntry::GraphEvent(GraphEventEntry {
            session_id: session_id.to_string(),
            event_id: event.metadata.id.to_string(),
            event: event.clone(),
        })
    }

    #[cfg(test)]
    fn message_uuid(&self) -> Option<&str> {
        match self {
            JsonlEntry::User(e) => Some(&e.common.uuid),
            JsonlEntry::Assistant(e) => Some(&e.common.uuid),
            _ => None,
        }
    }

    #[cfg(test)]
    fn graph_event_id(&self) -> Option<&str> {
        match self {
            JsonlEntry::GraphEvent(e) => Some(&e.event_id),
            _ => None,
        }
    }
}

// ============================================================================
// Session Index
// ============================================================================

#[derive(Clone, Debug)]
struct SessionMeta {
    path: PathBuf,
    project_path: Option<PathBuf>,
    tenant_id: Option<String>,
    updated_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    persisted_ids: HashSet<String>,
    todos_hash: u64,
    plan_hash: u64,
}

#[derive(Default)]
struct SessionIndex {
    sessions: HashMap<SessionId, SessionMeta>,
    by_project: HashMap<PathBuf, Vec<SessionId>>,
    by_tenant: HashMap<String, Vec<SessionId>>,
}

impl SessionIndex {
    fn insert(&mut self, session_id: SessionId, meta: SessionMeta) {
        // Remove old entries if updating
        self.remove(&session_id);

        if let Some(ref project) = meta.project_path {
            self.by_project
                .entry(project.clone())
                .or_default()
                .push(session_id);
        }
        if let Some(ref tenant) = meta.tenant_id {
            self.by_tenant
                .entry(tenant.clone())
                .or_default()
                .push(session_id);
        }
        self.sessions.insert(session_id, meta);
    }

    fn remove(&mut self, session_id: &SessionId) -> Option<SessionMeta> {
        let meta = self.sessions.remove(session_id)?;

        if let Some(ref project) = meta.project_path
            && let Some(ids) = self.by_project.get_mut(project)
        {
            ids.retain(|id| id != session_id);
        }
        if let Some(ref tenant) = meta.tenant_id
            && let Some(ids) = self.by_tenant.get_mut(tenant)
        {
            ids.retain(|id| id != session_id);
        }
        Some(meta)
    }
}

// ============================================================================
// File Operations (blocking, run via spawn_blocking)
// ============================================================================

fn read_entries_sync(path: &Path) -> SessionResult<Vec<JsonlEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = std::fs::File::open(path).map_err(|e| SessionError::Storage {
        message: format!("Failed to open {}: {}", path.display(), e),
    })?;

    let reader = BufReader::with_capacity(64 * 1024, file);
    let mut entries = Vec::with_capacity(128);

    for (line_num, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| SessionError::Storage {
            message: format!("Read error at line {}: {}", line_num + 1, e),
        })?;

        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<JsonlEntry>(&line) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    line = line_num + 1,
                    error = %e,
                    "Skipping malformed JSONL entry"
                );
            }
        }
    }

    Ok(entries)
}

fn append_entries_sync(path: &Path, entries: &[JsonlEntry], sync: bool) -> SessionResult<()> {
    if entries.is_empty() {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SessionError::Storage {
            message: format!("Failed to create directory {}: {}", parent.display(), e),
        })?;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| SessionError::Storage {
            message: format!("Failed to open {} for writing: {}", path.display(), e),
        })?;

    let mut writer = std::io::BufWriter::with_capacity(64 * 1024, file);

    for entry in entries {
        serde_json::to_writer(&mut writer, entry)?;
        writeln!(writer).map_err(|e| SessionError::Storage {
            message: format!("Write failed: {}", e),
        })?;
    }

    writer.flush().map_err(|e| SessionError::Storage {
        message: format!("Flush failed: {}", e),
    })?;

    if sync {
        writer
            .into_inner()
            .map_err(|e| SessionError::Storage {
                message: format!("Buffer error: {}", e.error()),
            })?
            .sync_all()
            .map_err(|e| SessionError::Storage {
                message: format!("Sync failed: {}", e),
            })?;
    }

    Ok(())
}

// ============================================================================
// JSONL Persistence Implementation
// ============================================================================

pub struct JsonlPersistence {
    config: JsonlConfig,
    index: Arc<RwLock<SessionIndex>>,
    summaries: Arc<RwLock<HashMap<SessionId, Vec<SummarySnapshot>>>>,
    queue: Arc<RwLock<HashMap<SessionId, Vec<QueueItem>>>>,
}

impl JsonlPersistence {
    pub async fn new(config: JsonlConfig) -> SessionResult<Self> {
        tokio::fs::create_dir_all(config.projects_dir())
            .await
            .map_err(|e| SessionError::Storage {
                message: format!("Failed to create projects directory: {}", e),
            })?;

        let persistence = Self {
            config,
            index: Arc::new(RwLock::new(SessionIndex::default())),
            summaries: Arc::new(RwLock::new(HashMap::new())),
            queue: Arc::new(RwLock::new(HashMap::new())),
        };

        persistence.rebuild_index().await?;
        Ok(persistence)
    }

    pub async fn default_config() -> SessionResult<Self> {
        Self::new(JsonlConfig::default()).await
    }

    async fn rebuild_index(&self) -> SessionResult<()> {
        let projects_dir = self.config.projects_dir();
        if !projects_dir.exists() {
            return Ok(());
        }

        let mut index = self.index.write().await;
        let mut summaries = self.summaries.write().await;
        let mut queue = self.queue.write().await;

        let mut entries =
            tokio::fs::read_dir(&projects_dir)
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Failed to read projects dir: {}", e),
                })?;

        while let Some(project_entry) =
            entries
                .next_entry()
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Failed to read entry: {}", e),
                })?
        {
            let file_type = project_entry.file_type().await.ok();
            if !file_type.map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }

            let project_path = project_entry.path();
            let mut files =
                tokio::fs::read_dir(&project_path)
                    .await
                    .map_err(|e| SessionError::Storage {
                        message: format!("Failed to read project dir: {}", e),
                    })?;

            while let Some(file_entry) =
                files
                    .next_entry()
                    .await
                    .map_err(|e| SessionError::Storage {
                        message: format!("Failed to read file entry: {}", e),
                    })?
            {
                let file_path = file_entry.path();
                if file_path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }

                let session_id = match file_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(SessionId::parse)
                {
                    Some(id) => id,
                    None => continue,
                };

                // Read file in blocking context
                let path_clone = file_path.clone();
                let parsed = tokio::task::spawn_blocking(move || read_entries_sync(&path_clone))
                    .await
                    .map_err(|e| SessionError::Storage {
                        message: format!("Task join error: {}", e),
                    })??;

                let (meta, session_summaries, session_queue) =
                    Self::parse_file_metadata(session_id, file_path, &parsed);

                index.insert(session_id, meta);
                if !session_summaries.is_empty() {
                    summaries.insert(session_id, session_summaries);
                }
                if !session_queue.is_empty() {
                    queue.insert(session_id, session_queue);
                }
            }
        }

        Ok(())
    }

    fn parse_file_metadata(
        session_id: SessionId,
        path: PathBuf,
        entries: &[JsonlEntry],
    ) -> (SessionMeta, Vec<SummarySnapshot>, Vec<QueueItem>) {
        let mut project_path: Option<PathBuf> = None;
        let mut tenant_id: Option<String> = None;
        let mut updated_at = Utc::now();
        let mut expires_at: Option<DateTime<Utc>> = None;
        let mut summaries = Vec::new();
        let mut queue_items: HashMap<String, QueueItem> = HashMap::new();
        let mut persisted_ids = HashSet::with_capacity(entries.len());

        for entry in entries {
            match entry {
                JsonlEntry::User(e) => {
                    if project_path.is_none() {
                        project_path = e.common.cwd.clone();
                    }
                    updated_at = e.common.timestamp;
                    persisted_ids.insert(e.common.uuid.clone());
                }
                JsonlEntry::Assistant(e) => {
                    if project_path.is_none() {
                        project_path = e.common.cwd.clone();
                    }
                    updated_at = e.common.timestamp;
                    persisted_ids.insert(e.common.uuid.clone());
                }
                JsonlEntry::GraphEvent(e) => {
                    persisted_ids.insert(format!("graph:{}", e.event_id));
                }
                JsonlEntry::Bookmark(e) => {
                    persisted_ids.insert(format!("bookmark:{}", e.id));
                }
                JsonlEntry::Checkpoint(e) => {
                    persisted_ids.insert(format!("checkpoint:{}", e.id));
                }
                JsonlEntry::SessionMeta(m) => {
                    tenant_id.clone_from(&m.tenant_id);
                    updated_at = m.updated_at;
                    expires_at = m.expires_at;
                }
                JsonlEntry::Summary(s) => {
                    summaries.push(SummarySnapshot {
                        id: Uuid::new_v4(),
                        session_id,
                        summary: s.summary.clone(),
                        leaf_message_id: s.leaf_uuid.as_ref().map(MessageId::from_string),
                        created_at: s.timestamp,
                    });
                }
                JsonlEntry::QueueOperation(q) => {
                    let item_id = match Uuid::parse_str(&q.item_id) {
                        Ok(id) => id,
                        Err(_) => continue,
                    };
                    match q.operation.as_str() {
                        "enqueue" => {
                            queue_items.insert(
                                q.item_id.clone(),
                                QueueItem {
                                    id: item_id,
                                    session_id,
                                    operation: QueueOperation::Enqueue,
                                    content: q.content.clone(),
                                    priority: q.priority,
                                    status: QueueStatus::Pending,
                                    created_at: q.timestamp,
                                    processed_at: None,
                                },
                            );
                        }
                        "dequeue" => {
                            if let Some(item) = queue_items.get_mut(&q.item_id) {
                                item.status = QueueStatus::Processing;
                                item.processed_at = Some(q.timestamp);
                            }
                        }
                        "cancel" => {
                            if let Some(item) = queue_items.get_mut(&q.item_id) {
                                item.status = QueueStatus::Cancelled;
                                item.processed_at = Some(q.timestamp);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        let queue: Vec<QueueItem> = queue_items.into_values().collect();

        (
            SessionMeta {
                path,
                project_path,
                tenant_id,
                updated_at,
                expires_at,
                persisted_ids,
                todos_hash: 0,
                plan_hash: 0,
            },
            summaries,
            queue,
        )
    }

    fn session_file_path(&self, session_id: &SessionId, project_path: Option<&Path>) -> PathBuf {
        let dir = match project_path {
            Some(p) => self.config.project_dir(p),
            None => self.config.projects_dir().join("_default"),
        };
        dir.join(format!("{}.jsonl", session_id))
    }

    fn get_project_path(session: &Session) -> Option<PathBuf> {
        session
            .messages
            .first()
            .and_then(|m| m.environment.as_ref())
            .and_then(|e| e.cwd.clone())
    }

    /// Compute a stable hash of todos for change detection.
    /// Uses ahash with fixed seeds for deterministic cross-version hashing.
    fn compute_todos_hash(todos: &[TodoItem]) -> u64 {
        let hasher_state = ahash::RandomState::with_seeds(1234, 5678, 9012, 3456);
        let mut hasher = hasher_state.build_hasher();
        for todo in todos {
            todo.id.hash(&mut hasher);
            enum_to_jsonl(&todo.status, "pending").hash(&mut hasher);
            todo.content.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Compute a stable hash of plan for change detection.
    /// Uses ahash with fixed seeds for deterministic cross-version hashing.
    fn compute_plan_hash(plan: Option<&Plan>) -> u64 {
        let hasher_state = ahash::RandomState::with_seeds(1234, 5678, 9012, 3456);
        let mut hasher = hasher_state.build_hasher();
        if let Some(p) = plan {
            p.id.hash(&mut hasher);
            enum_to_jsonl(&p.status, "draft").hash(&mut hasher);
            p.content.hash(&mut hasher);
        }
        hasher.finish()
    }

    fn session_to_meta_entry(session: &Session) -> JsonlEntry {
        let warn_serialize = |field: &str, e: serde_json::Error| -> serde_json::Value {
            tracing::warn!(
                session_id = %session.id,
                field,
                error = %e,
                "Failed to serialize session field"
            );
            serde_json::Value::Object(Default::default())
        };

        JsonlEntry::SessionMeta(SessionMetaEntry {
            session_id: session.id.to_string(),
            parent_session_id: session.parent_id.map(|p| p.to_string()),
            tenant_id: session.tenant_id.clone(),
            session_type: serde_json::to_value(&session.session_type)
                .unwrap_or_else(|e| warn_serialize("session_type", e)),
            mode: "stateless".to_string(),
            state: enum_to_jsonl(&session.state, "created"),
            config: serde_json::to_value(&session.config)
                .unwrap_or_else(|e| warn_serialize("config", e)),
            permission_policy: serde_json::to_value(&session.permissions)
                .unwrap_or_else(|e| warn_serialize("permissions", e)),
            total_usage: UsageInfo::from(&session.total_usage),
            total_cost_usd: session.total_cost_usd,
            static_context_hash: session.static_context_hash.clone(),
            error: session.error.clone(),
            created_at: session.created_at,
            updated_at: session.updated_at,
            expires_at: session.expires_at,
        })
    }

    fn reconstruct_session(session_id: SessionId, entries: Vec<JsonlEntry>) -> Session {
        let mut session = Session::new(SessionConfig::default());
        session.id = session_id;

        let mut todos_map: HashMap<String, TodoItem> = HashMap::new();
        let mut latest_plan: Option<Plan> = None;
        let mut compacts: Vec<CompactRecord> = Vec::new();
        let mut graph_events: Vec<GraphEvent> = Vec::new();
        let mut bookmarks: Vec<BookmarkEntry> = Vec::new();
        let mut checkpoints: Vec<CheckpointEntry> = Vec::new();

        for entry in entries {
            match entry {
                JsonlEntry::User(_) | JsonlEntry::Assistant(_) => {}
                JsonlEntry::SessionMeta(m) => {
                    session.tenant_id = m.tenant_id;
                    session.parent_id = m
                        .parent_session_id
                        .as_ref()
                        .and_then(|s| SessionId::parse(s));
                    session.session_type =
                        serde_json::from_value(m.session_type).unwrap_or(SessionType::Main);
                    // m.mode is ignored; SessionMode was removed (always stateless)
                    session.state = jsonl_to_enum(&m.state).unwrap_or_default();
                    session.config = serde_json::from_value(m.config).unwrap_or_else(|e| {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "Failed to deserialize session config, using default"
                        );
                        Default::default()
                    });
                    session.permissions = serde_json::from_value(m.permission_policy)
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                session_id = %session_id,
                                error = %e,
                                "Failed to deserialize session permissions, using default"
                            );
                            Default::default()
                        });
                    session.total_usage = TokenUsage::from(&m.total_usage);
                    session.total_cost_usd = m.total_cost_usd;
                    session.static_context_hash = m.static_context_hash;
                    session.error = m.error;
                    session.created_at = m.created_at;
                    session.updated_at = m.updated_at;
                    session.expires_at = m.expires_at;
                }
                JsonlEntry::Summary(s) => {
                    session.summary = Some(s.summary);
                }
                JsonlEntry::Todo(t) => {
                    let todo = TodoItem {
                        id: Uuid::parse_str(&t.id).unwrap_or_else(|_| Uuid::new_v4()),
                        session_id,
                        content: t.content,
                        active_form: t.active_form,
                        status: jsonl_to_enum(&t.status).unwrap_or_default(),
                        plan_id: t.plan_id.and_then(|s| Uuid::parse_str(&s).ok()),
                        created_at: t.created_at,
                        started_at: t.started_at,
                        completed_at: t.completed_at,
                    };
                    // Use map to get latest version of each todo
                    todos_map.insert(t.id, todo);
                }
                JsonlEntry::Plan(p) => {
                    let plan = Plan {
                        id: Uuid::parse_str(&p.id).unwrap_or_else(|_| Uuid::new_v4()),
                        session_id,
                        name: p.name,
                        content: p.content,
                        status: jsonl_to_enum(&p.status).unwrap_or_default(),
                        error: p.error,
                        created_at: p.created_at,
                        approved_at: p.approved_at,
                        started_at: p.started_at,
                        completed_at: p.completed_at,
                    };
                    // Keep the latest plan entry
                    latest_plan = Some(plan);
                }
                JsonlEntry::Compact(c) => {
                    compacts.push(CompactRecord {
                        id: Uuid::parse_str(&c.id).unwrap_or_else(|_| Uuid::new_v4()),
                        session_id,
                        trigger: jsonl_to_enum(&c.trigger).unwrap_or_default(),
                        pre_tokens: c.pre_tokens,
                        post_tokens: c.post_tokens,
                        saved_tokens: c.saved_tokens,
                        summary: c.summary,
                        original_count: c.original_count,
                        new_count: c.new_count,
                        logical_parent_id: c.logical_parent_id.as_ref().map(MessageId::from_string),
                        created_at: c.created_at,
                    });
                }
                JsonlEntry::GraphEvent(entry) => graph_events.push(entry.event),
                JsonlEntry::Bookmark(entry) => bookmarks.push(entry),
                JsonlEntry::Checkpoint(entry) => checkpoints.push(entry),
                _ => {}
            }
        }

        // Restore todos, plan, and compacts
        session.todos = todos_map.into_values().collect();
        session
            .todos
            .sort_by(|a, b| a.created_at.cmp(&b.created_at));
        session.current_plan = latest_plan;
        session.compact_history = VecDeque::from(compacts);
        session.graph = GraphMaterializer::from_events(&graph_events);
        session.graph.id = session.id.0;
        session.graph.created_at = session.created_at;
        for bookmark in bookmarks {
            if let (Ok(id), Ok(node_id), Ok(branch_id)) = (
                Uuid::parse_str(&bookmark.id),
                Uuid::parse_str(&bookmark.node_id),
                Uuid::parse_str(&bookmark.branch_id),
            ) {
                session.graph.bookmarks.insert(
                    id,
                    crate::graph::Bookmark {
                        id,
                        node_id,
                        branch_id,
                        label: bookmark.label,
                        note: bookmark.note,
                        created_at: bookmark.created_at,
                    },
                );
            }
        }
        for checkpoint in checkpoints {
            if let (Ok(id), Ok(branch_id)) = (
                Uuid::parse_str(&checkpoint.id),
                Uuid::parse_str(&checkpoint.branch_id),
            ) {
                session.graph.checkpoints.insert(
                    id,
                    crate::graph::Checkpoint {
                        id,
                        branch_id,
                        label: checkpoint.label,
                        note: checkpoint.note,
                        tags: checkpoint.tags,
                        created_at: checkpoint.created_at,
                    },
                );
            }
        }
        session.refresh_message_projection();

        session
    }
}

#[async_trait::async_trait]
impl Persistence for JsonlPersistence {
    fn name(&self) -> &str {
        "jsonl"
    }

    async fn save(&self, session: &Session) -> SessionResult<()> {
        let project_path = Self::get_project_path(session);
        let file_path = self.session_file_path(&session.id, project_path.as_deref());

        // Get persisted state from index (avoid re-reading file)
        let (persisted_ids, prev_todos_hash, prev_plan_hash) = {
            let index = self.index.read().await;
            match index.sessions.get(&session.id) {
                Some(m) => (m.persisted_ids.clone(), m.todos_hash, m.plan_hash),
                None => (HashSet::new(), 0, 0),
            }
        };

        let mut new_entries = Vec::new();
        let mut new_ids = HashSet::new();

        new_entries.push(Self::session_to_meta_entry(session));

        // Only new messages (incremental)
        let message_projection = session.current_branch_messages();

        for msg in &message_projection {
            let id = msg.id.to_string();
            if !persisted_ids.contains(&id) {
                new_entries.push(JsonlEntry::from_message(&session.id, msg));
                new_ids.insert(id);
            }
        }

        for event in &session.graph.events {
            let event_id = format!("graph:{}", event.metadata.id);
            if !persisted_ids.contains(&event_id) {
                new_entries.push(JsonlEntry::from_graph_event(&session.id, event));
                new_ids.insert(event_id);
            }
        }

        for bookmark in session.graph.bookmarks.values() {
            let bookmark_id = format!("bookmark:{}", bookmark.id);
            if !persisted_ids.contains(&bookmark_id) {
                new_entries.push(JsonlEntry::Bookmark(BookmarkEntry {
                    session_id: session.id.to_string(),
                    id: bookmark.id.to_string(),
                    node_id: bookmark.node_id.to_string(),
                    branch_id: bookmark.branch_id.to_string(),
                    label: bookmark.label.clone(),
                    note: bookmark.note.clone(),
                    created_at: bookmark.created_at,
                }));
                new_ids.insert(bookmark_id);
            }
        }

        for checkpoint in session.graph.checkpoints.values() {
            let checkpoint_id = format!("checkpoint:{}", checkpoint.id);
            if !persisted_ids.contains(&checkpoint_id) {
                new_entries.push(JsonlEntry::Checkpoint(CheckpointEntry {
                    session_id: session.id.to_string(),
                    id: checkpoint.id.to_string(),
                    branch_id: checkpoint.branch_id.to_string(),
                    label: checkpoint.label.clone(),
                    note: checkpoint.note.clone(),
                    tags: checkpoint.tags.clone(),
                    created_at: checkpoint.created_at,
                }));
                new_ids.insert(checkpoint_id);
            }
        }

        // Compute current hashes
        let current_todos_hash = Self::compute_todos_hash(&session.todos);
        let current_plan_hash = Self::compute_plan_hash(session.current_plan.as_ref());

        // Persist todos only if changed (including when cleared)
        if current_todos_hash != prev_todos_hash {
            for todo in &session.todos {
                new_entries.push(JsonlEntry::Todo(TodoEntry {
                    id: todo.id.to_string(),
                    session_id: session.id.to_string(),
                    content: todo.content.clone(),
                    active_form: todo.active_form.clone(),
                    status: enum_to_jsonl(&todo.status, "pending"),
                    plan_id: todo.plan_id.map(|id| id.to_string()),
                    created_at: todo.created_at,
                    started_at: todo.started_at,
                    completed_at: todo.completed_at,
                }));
            }
        }

        // Persist plan only if changed
        if current_plan_hash != prev_plan_hash
            && let Some(ref plan) = session.current_plan
        {
            new_entries.push(JsonlEntry::Plan(PlanEntry {
                id: plan.id.to_string(),
                session_id: session.id.to_string(),
                name: plan.name.clone(),
                content: plan.content.clone(),
                status: enum_to_jsonl(&plan.status, "draft"),
                error: plan.error.clone(),
                created_at: plan.created_at,
                approved_at: plan.approved_at,
                started_at: plan.started_at,
                completed_at: plan.completed_at,
            }));
        }

        // Persist compact history (incremental by ID)
        for compact in &session.compact_history {
            let compact_id = format!("compact:{}", compact.id);
            if !persisted_ids.contains(&compact_id) {
                new_entries.push(JsonlEntry::Compact(CompactEntry {
                    id: compact.id.to_string(),
                    session_id: session.id.to_string(),
                    trigger: enum_to_jsonl(&compact.trigger, "manual"),
                    pre_tokens: compact.pre_tokens,
                    post_tokens: compact.post_tokens,
                    saved_tokens: compact.saved_tokens,
                    summary: compact.summary.clone(),
                    original_count: compact.original_count,
                    new_count: compact.new_count,
                    logical_parent_id: compact.logical_parent_id.as_ref().map(|id| id.to_string()),
                    created_at: compact.created_at,
                }));
                new_ids.insert(compact_id);
            }
        }

        if new_entries.is_empty() {
            return Ok(());
        }

        // Write in blocking context
        let path_clone = file_path.clone();
        let sync = self.config.sync_mode == SyncMode::OnWrite;
        tokio::task::spawn_blocking(move || append_entries_sync(&path_clone, &new_entries, sync))
            .await
            .map_err(|e| SessionError::Storage {
                message: format!("Task join error: {}", e),
            })??;

        let mut index = self.index.write().await;
        let mut persisted = persisted_ids;
        persisted.extend(new_ids);

        index.insert(
            session.id,
            SessionMeta {
                path: file_path,
                project_path,
                tenant_id: session.tenant_id.clone(),
                updated_at: session.updated_at,
                expires_at: session.expires_at,
                persisted_ids: persisted,
                todos_hash: current_todos_hash,
                plan_hash: current_plan_hash,
            },
        );

        Ok(())
    }

    async fn load(&self, id: &SessionId) -> SessionResult<Option<Session>> {
        let path = {
            let index = self.index.read().await;
            match index.sessions.get(id) {
                Some(m) => m.path.clone(),
                None => return Ok(None),
            }
        };

        let entries = tokio::task::spawn_blocking(move || read_entries_sync(&path))
            .await
            .map_err(|e| SessionError::Storage {
                message: format!("Task join error: {}", e),
            })??;

        if entries.is_empty() {
            return Ok(None);
        }

        let session = Self::reconstruct_session(*id, entries);
        Ok(Some(session))
    }

    async fn delete(&self, id: &SessionId) -> SessionResult<bool> {
        let meta = {
            let mut index = self.index.write().await;
            index.remove(id)
        };

        let Some(meta) = meta else {
            return Ok(false);
        };

        if meta.path.exists() {
            tokio::fs::remove_file(&meta.path)
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Failed to delete {}: {}", meta.path.display(), e),
                })?;
        }

        self.summaries.write().await.remove(id);
        self.queue.write().await.remove(id);
        Ok(true)
    }

    async fn list(&self, tenant_id: Option<&str>) -> SessionResult<Vec<SessionId>> {
        let index = self.index.read().await;
        Ok(match tenant_id {
            Some(tid) => index.by_tenant.get(tid).cloned().unwrap_or_default(),
            None => index.sessions.keys().copied().collect(),
        })
    }

    async fn add_summary(&self, snapshot: SummarySnapshot) -> SessionResult<()> {
        let path = {
            let index = self.index.read().await;
            index
                .sessions
                .get(&snapshot.session_id)
                .map(|m| m.path.clone())
        };

        if let Some(path) = path {
            let entry = JsonlEntry::Summary(SummaryEntry {
                session_id: snapshot.session_id.to_string(),
                summary: snapshot.summary.clone(),
                leaf_uuid: snapshot.leaf_message_id.as_ref().map(|id| id.to_string()),
                timestamp: snapshot.created_at,
            });

            let sync = self.config.sync_mode == SyncMode::OnWrite;
            tokio::task::spawn_blocking(move || append_entries_sync(&path, &[entry], sync))
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Task join error: {}", e),
                })??;
        }

        self.summaries
            .write()
            .await
            .entry(snapshot.session_id)
            .or_default()
            .push(snapshot);

        Ok(())
    }

    async fn get_summaries(&self, session_id: &SessionId) -> SessionResult<Vec<SummarySnapshot>> {
        Ok(self
            .summaries
            .read()
            .await
            .get(session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn enqueue(
        &self,
        session_id: &SessionId,
        content: String,
        priority: i32,
    ) -> SessionResult<QueueItem> {
        let item = QueueItem::enqueue(*session_id, content.clone()).priority(priority);

        let path = {
            let index = self.index.read().await;
            index.sessions.get(session_id).map(|m| m.path.clone())
        };

        if let Some(path) = path {
            let entry = JsonlEntry::QueueOperation(QueueOperationEntry {
                operation: "enqueue".to_string(),
                session_id: session_id.to_string(),
                timestamp: Utc::now(),
                content,
                priority,
                item_id: item.id.to_string(),
            });

            let sync = self.config.sync_mode == SyncMode::OnWrite;
            tokio::task::spawn_blocking(move || append_entries_sync(&path, &[entry], sync))
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Task join error: {}", e),
                })??;
        }

        self.queue
            .write()
            .await
            .entry(*session_id)
            .or_default()
            .push(item.clone());

        Ok(item)
    }

    async fn dequeue(&self, session_id: &SessionId) -> SessionResult<Option<QueueItem>> {
        let dequeued = {
            let mut queue = self.queue.write().await;
            let items = match queue.get_mut(session_id) {
                Some(items) => items,
                None => return Ok(None),
            };

            items.sort_by(|a, b| b.priority.cmp(&a.priority));

            let mut result = None;
            for item in items.iter_mut() {
                if item.status == QueueStatus::Pending {
                    item.start_processing();
                    result = Some(item.clone());
                    break;
                }
            }
            result
        };

        if let Some(ref item) = dequeued {
            let path = {
                let index = self.index.read().await;
                index.sessions.get(session_id).map(|m| m.path.clone())
            };

            if let Some(path) = path {
                let entry = JsonlEntry::QueueOperation(QueueOperationEntry {
                    operation: "dequeue".to_string(),
                    session_id: session_id.to_string(),
                    timestamp: Utc::now(),
                    content: item.content.clone(),
                    priority: item.priority,
                    item_id: item.id.to_string(),
                });

                let sync = self.config.sync_mode == SyncMode::OnWrite;
                tokio::task::spawn_blocking(move || append_entries_sync(&path, &[entry], sync))
                    .await
                    .map_err(|e| SessionError::Storage {
                        message: format!("Task join error: {}", e),
                    })??;
            }
        }

        Ok(dequeued)
    }

    async fn cancel_queued(&self, item_id: Uuid) -> SessionResult<bool> {
        let cancelled = {
            let mut queue = self.queue.write().await;
            let mut found = None;
            for items in queue.values_mut() {
                if let Some(item) = items.iter_mut().find(|i| i.id == item_id) {
                    item.cancel();
                    found = Some(item.clone());
                    break;
                }
            }
            found
        };

        let Some(item) = cancelled else {
            return Ok(false);
        };

        let path = {
            let index = self.index.read().await;
            index.sessions.get(&item.session_id).map(|m| m.path.clone())
        };

        if let Some(path) = path {
            let entry = JsonlEntry::QueueOperation(QueueOperationEntry {
                operation: "cancel".to_string(),
                session_id: item.session_id.to_string(),
                timestamp: Utc::now(),
                content: item.content.clone(),
                priority: item.priority,
                item_id: item.id.to_string(),
            });

            let sync = self.config.sync_mode == SyncMode::OnWrite;
            tokio::task::spawn_blocking(move || append_entries_sync(&path, &[entry], sync))
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Task join error: {}", e),
                })??;
        }

        Ok(true)
    }

    async fn pending_queue(&self, session_id: &SessionId) -> SessionResult<Vec<QueueItem>> {
        Ok(self
            .queue
            .read()
            .await
            .get(session_id)
            .map(|items| {
                items
                    .iter()
                    .filter(|i| i.status == QueueStatus::Pending)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn cleanup_expired(&self) -> SessionResult<usize> {
        let now = Utc::now();
        let retention_cutoff = now - chrono::Duration::days(self.config.retention_days as i64);

        let (expired_ids, expired_paths) = {
            let mut index = self.index.write().await;
            let expired_ids: Vec<SessionId> = index
                .sessions
                .iter()
                .filter(|(_, m)| {
                    if let Some(expires_at) = m.expires_at {
                        expires_at < now
                    } else {
                        m.updated_at < retention_cutoff
                    }
                })
                .map(|(id, _)| *id)
                .collect();

            let mut paths = Vec::with_capacity(expired_ids.len());
            for id in &expired_ids {
                if let Some(meta) = index.remove(id) {
                    paths.push(meta.path);
                }
            }
            (expired_ids, paths)
        };

        let count = expired_paths.len();

        if !expired_ids.is_empty() {
            let mut summaries = self.summaries.write().await;
            let mut queue = self.queue.write().await;
            for id in &expired_ids {
                summaries.remove(id);
                queue.remove(id);
            }
        }

        for path in expired_paths {
            let _ = tokio::fs::remove_file(&path).await;
        }

        Ok(count)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ContentBlock;
    use tempfile::TempDir;

    async fn create_test_persistence() -> (JsonlPersistence, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let config = JsonlConfig::builder()
            .base_dir(temp_dir.path().to_path_buf())
            .build();
        let persistence = JsonlPersistence::new(config).await.unwrap();
        (persistence, temp_dir)
    }

    #[tokio::test]
    async fn test_save_and_load_session() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]));
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text(
            "Hi there!",
        )]));

        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.current_branch_messages().len(), 2);
    }

    #[tokio::test]
    async fn test_incremental_save() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("First")]));
        persistence.save(&session).await.unwrap();

        session.add_message(SessionMessage::assistant(vec![ContentBlock::text(
            "Second",
        )]));
        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.current_branch_messages().len(), 2);
    }

    #[tokio::test]
    async fn test_delete_session() {
        let (persistence, _temp) = create_test_persistence().await;

        let session = Session::new(SessionConfig::default());
        persistence.save(&session).await.unwrap();

        assert!(persistence.delete(&session.id).await.unwrap());
        assert!(persistence.load(&session.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_list_sessions() {
        let (persistence, _temp) = create_test_persistence().await;

        let s1 = Session::new(SessionConfig::default());
        let s2 = Session::new(SessionConfig::default());

        persistence.save(&s1).await.unwrap();
        persistence.save(&s2).await.unwrap();

        let list = persistence.list(None).await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn test_tenant_filtering() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut s1 = Session::new(SessionConfig::default());
        s1.tenant_id = Some("tenant-a".to_string());

        let mut s2 = Session::new(SessionConfig::default());
        s2.tenant_id = Some("tenant-b".to_string());

        persistence.save(&s1).await.unwrap();
        persistence.save(&s2).await.unwrap();

        let list = persistence.list(Some("tenant-a")).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], s1.id);
    }

    #[tokio::test]
    async fn test_summaries() {
        let (persistence, _temp) = create_test_persistence().await;

        let session = Session::new(SessionConfig::default());
        persistence.save(&session).await.unwrap();

        persistence
            .add_summary(SummarySnapshot::new(session.id, "Summary 1"))
            .await
            .unwrap();
        persistence
            .add_summary(SummarySnapshot::new(session.id, "Summary 2"))
            .await
            .unwrap();

        let summaries = persistence.get_summaries(&session.id).await.unwrap();
        assert_eq!(summaries.len(), 2);
    }

    #[tokio::test]
    async fn test_queue_operations() {
        let (persistence, _temp) = create_test_persistence().await;

        let session = Session::new(SessionConfig::default());
        persistence.save(&session).await.unwrap();

        persistence
            .enqueue(&session.id, "Low priority".to_string(), 1)
            .await
            .unwrap();
        persistence
            .enqueue(&session.id, "High priority".to_string(), 10)
            .await
            .unwrap();

        let next = persistence.dequeue(&session.id).await.unwrap().unwrap();
        assert_eq!(next.content, "High priority");
    }

    #[tokio::test]
    async fn test_dag_reconstruction() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("Q1")]));
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text("A1")]));
        session.add_message(SessionMessage::user(vec![ContentBlock::text("Q2")]));
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text("A2")]));

        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();

        let messages = loaded.current_branch_messages();
        assert_eq!(messages.len(), 4);
        assert!(
            messages[0]
                .content
                .iter()
                .any(|c| c.as_text() == Some("Q1"))
        );
        assert!(
            messages[1]
                .content
                .iter()
                .any(|c| c.as_text() == Some("A1"))
        );
        assert!(
            messages[2]
                .content
                .iter()
                .any(|c| c.as_text() == Some("Q2"))
        );
        assert!(
            messages[3]
                .content
                .iter()
                .any(|c| c.as_text() == Some("A2"))
        );
    }

    #[tokio::test]
    async fn test_project_path_encoding() {
        let config = JsonlConfig::default();

        assert_eq!(
            config.encode_project_path(Path::new("/home/user/project")),
            "2f686f6d652f757365722f70726f6a656374"
        );
        assert_eq!(
            config.encode_project_path(Path::new("/Users/alice/work/app")),
            "2f55736572732f616c6963652f776f726b2f617070"
        );
    }

    #[test]
    fn test_jsonl_entry_serialization() {
        let msg = SessionMessage::user(vec![ContentBlock::text("Hello")]);
        let session_id = SessionId::new();
        let entry = JsonlEntry::from_message(&session_id, &msg);

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"type\":\"user\""));

        let parsed: JsonlEntry = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, JsonlEntry::User(_)));
    }

    #[test]
    fn test_graph_event_entry_serialization() {
        let session_id = SessionId::new();
        let event = GraphEvent::new(crate::graph::GraphEventBody::NodeAppended {
            node_id: Uuid::new_v4(),
            branch_id: Uuid::new_v4(),
            parent_id: None,
            kind: crate::graph::NodeKind::User,
            tags: vec!["test".to_string()],
            payload: serde_json::json!({"content": []}),
        });
        let entry = JsonlEntry::from_graph_event(&session_id, &event);

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: JsonlEntry = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, JsonlEntry::GraphEvent(_)));
    }

    #[tokio::test]
    async fn test_no_duplicate_writes() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]));
        persistence.save(&session).await.unwrap();
        persistence.save(&session).await.unwrap(); // Save same data twice
        persistence.save(&session).await.unwrap(); // And again

        // Check file has only one message entry + session meta
        let file_path = persistence.session_file_path(&session.id, None);
        let entries = read_entries_sync(&file_path).unwrap();
        let message_count = entries
            .iter()
            .filter(|e| e.message_uuid().is_some())
            .count();
        assert_eq!(message_count, 1, "Should not duplicate message entries");

        let graph_event_count = entries
            .iter()
            .filter(|e| e.graph_event_id().is_some())
            .count();
        assert_eq!(
            graph_event_count, 1,
            "Should not duplicate graph event entries"
        );
    }

    #[tokio::test]
    async fn test_graph_events_roundtrip() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]));
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text("Hi")]));
        assert_eq!(session.graph.events.len(), 2);

        persistence.save(&session).await.unwrap();

        let file_path = persistence.session_file_path(&session.id, None);
        let raw = std::fs::read_to_string(&file_path).unwrap();
        assert!(raw.contains("\"type\":\"graph_event\""));
        let entries = read_entries_sync(&file_path).unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.graph_event_id().is_some())
                .count(),
            2
        );

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.graph.events.len(), 2);
        assert_eq!(loaded.current_branch_messages().len(), 2);
        assert_eq!(
            loaded.graph.branch_nodes(loaded.graph.primary_branch).len(),
            2
        );
    }

    #[tokio::test]
    async fn test_bookmarks_roundtrip() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]));
        session.bookmark_current_head("start", Some("saved".to_string()));

        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.graph.bookmarks.len(), 1);
    }

    #[tokio::test]
    async fn test_checkpoints_roundtrip() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]));
        session.graph.create_checkpoint(
            session.graph.primary_branch,
            "milestone",
            Some("saved".to_string()),
            vec!["tag".to_string()],
        );

        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.graph.checkpoints.len(), 1);
    }

    #[tokio::test]
    async fn test_graph_first_roundtrip_uses_projection_helper() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::user(vec![ContentBlock::text("hello")]));
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text("world")]));
        session.clear_messages();

        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        let messages = loaded.current_branch_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content[0].as_text(), Some("hello"));
        assert_eq!(messages[1].content[0].as_text(), Some("world"));
    }

    #[tokio::test]
    async fn test_direct_graph_event_append_roundtrip() {
        let (persistence, _temp) = create_test_persistence().await;
        let path = persistence.session_file_path(&SessionId::new(), None);
        let event = GraphEvent::new(crate::graph::GraphEventBody::NodeAppended {
            node_id: Uuid::new_v4(),
            branch_id: Uuid::new_v4(),
            parent_id: None,
            kind: crate::graph::NodeKind::User,
            tags: Vec::new(),
            payload: serde_json::json!({"content": []}),
        });

        append_entries_sync(
            &path,
            &[JsonlEntry::from_graph_event(&SessionId::new(), &event)],
            true,
        )
        .unwrap();

        let entries = read_entries_sync(&path).unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.graph_event_id().is_some())
                .count(),
            1
        );
    }

    #[test]
    fn test_windows_path_encoding() {
        let config = JsonlConfig::default();
        let encoded = config.encode_project_path(Path::new("C:\\Users\\alice\\project"));
        assert!(encoded.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!encoded.is_empty());
    }
}
