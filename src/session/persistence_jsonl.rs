//! JSONL-based persistence backend for inspectable local session archives.
//!
//! This module provides file-based session persistence using the JSONL (JSON Lines) format.
//! Graph events are the canonical durable state; message projections are always rebuilt from
//! graph state on load.
//!
//! # Features
//!
//! - **Graph-Canonical**: Graph events are the durable source of truth
//! - **DAG Structure**: Conversation state is reconstructed from graph events
//! - **Incremental Writes**: Only graph and metadata entries are appended, avoiding full rewrites
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
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use super::archive::verify_restored_session_roundtrip;
use super::state::{
    MessageId, Session, SessionConfig, SessionId, SessionMessage, SessionType,
    build_graph_provenance, graph_node_id_for_message, graph_node_kind_for_message,
    graph_parent_node_id_for_message, graph_payload_for_message, graph_tags_for_message,
};
use super::types::{CompactRecord, Plan, QueueItem, QueueOperation, QueueStatus, TodoItem};
use super::{Persistence, SessionError, SessionResult};
use crate::graph::{GraphEvent, GraphMaterializer, GraphValidator, SessionGraph};
use crate::types::TokenUsage;

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

fn project_path_from_graph_event(event: &GraphEvent) -> Option<PathBuf> {
    match &event.body {
        crate::graph::GraphEventBody::NodeAppended { payload, .. } => payload
            .get("environment")
            .and_then(|environment| environment.get("cwd"))
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from),
        _ => None,
    }
}

fn validate_graph(session_id: &SessionId, graph: &SessionGraph) -> SessionResult<()> {
    let report = GraphValidator::validate(graph);
    if report.is_valid() {
        return Ok(());
    }

    let issues = report
        .issues
        .into_iter()
        .map(|issue| format!("{}: {}", issue.code, issue.message))
        .collect::<Vec<_>>()
        .join("; ");
    Err(SessionError::Storage {
        message: format!("Invalid graph for session {}: {}", session_id, issues),
    })
}

fn parse_auxiliary_uuid(session_id: &SessionId, entry_type: &str, raw_id: &str) -> Option<Uuid> {
    match Uuid::parse_str(raw_id) {
        Ok(id) => Some(id),
        Err(error) => {
            tracing::warn!(
                session_id = %session_id,
                entry_type,
                raw_id,
                error = %error,
                "Skipping JSONL entry with invalid UUID"
            );
            None
        }
    }
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
// JSONL Entry Types
// ============================================================================

/// Graph-first JSONL entry types for local session persistence.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JsonlEntry {
    GraphEvent(GraphEventEntry),
    QueueReset(ResetEntry),
    QueueOperation(QueueOperationEntry),
    SessionMeta(SessionMetaEntry),
    TodoReset(ResetEntry),
    Todo(TodoEntry),
    PlanReset(ResetEntry),
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
            ..Default::default()
        }
    }
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
pub struct ResetEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionMetaEntry {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "projectPath", skip_serializing_if = "Option::is_none")]
    pub project_path: Option<PathBuf>,
    #[serde(rename = "parentSessionId", skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(rename = "tenantId", skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(rename = "principalId", skip_serializing_if = "Option::is_none")]
    pub principal_id: Option<String>,
    #[serde(rename = "sessionType")]
    pub session_type: serde_json::Value,
    pub mode: String,
    pub state: String,
    pub config: serde_json::Value,
    #[serde(rename = "permissionPolicy")]
    pub authorization_policy: serde_json::Value,
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

impl JsonlEntry {
    fn from_graph_event(session_id: &SessionId, event: &GraphEvent) -> Self {
        JsonlEntry::GraphEvent(GraphEventEntry {
            session_id: session_id.to_string(),
            event_id: event.metadata.id.to_string(),
            event: event.clone(),
        })
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
    last_session_meta: Option<SessionMetaEntry>,
    persisted_ids: HashSet<String>,
    todos_hash: u64,
    plan_hash: u64,
    current_leaf_id: Option<MessageId>,
    primary_branch_id: Option<Uuid>,
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

fn write_entries_to_temp_sync(
    path: &Path,
    entries: &[JsonlEntry],
    sync: bool,
) -> SessionResult<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SessionError::Storage {
            message: format!("Failed to create directory {}: {}", parent.display(), e),
        })?;
    }

    let temp_path = path.with_file_name(format!(
        ".{}.restore-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session.jsonl"),
        Uuid::new_v4()
    ));

    let write_result = (|| -> SessionResult<PathBuf> {
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .map_err(|e| SessionError::Storage {
                message: format!(
                    "Failed to open {} for atomic restore write: {}",
                    temp_path.display(),
                    e
                ),
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

        let file = writer.into_inner().map_err(|e| SessionError::Storage {
            message: format!("Buffer error: {}", e.error()),
        })?;

        if sync {
            file.sync_all().map_err(|e| SessionError::Storage {
                message: format!("Sync failed: {}", e),
            })?;
        }

        Ok(temp_path.clone())
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }

    write_result
}

fn publish_temp_file_no_overwrite(temp_path: &Path, path: &Path, sync: bool) -> SessionResult<()> {
    std::fs::hard_link(temp_path, path).map_err(|e| SessionError::Storage {
        message: format!(
            "Failed to publish restored JSONL session {} from {} without overwrite: {}",
            path.display(),
            temp_path.display(),
            e
        ),
    })?;

    if sync && let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .map_err(|e| SessionError::Storage {
                message: format!(
                    "Failed to open parent directory {} for sync: {}",
                    parent.display(),
                    e
                ),
            })?
            .sync_all()
            .map_err(|e| SessionError::Storage {
                message: format!(
                    "Failed to sync parent directory {}: {}",
                    parent.display(),
                    e
                ),
            })?;
    }

    if let Err(error) = std::fs::remove_file(temp_path) {
        tracing::warn!(
            path = %temp_path.display(),
            error = %error,
            "Failed to remove temporary JSONL restore file"
        );
    }

    if sync && let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .map_err(|e| SessionError::Storage {
                message: format!(
                    "Failed to open parent directory {} for sync: {}",
                    parent.display(),
                    e
                ),
            })?
            .sync_all()
            .map_err(|e| SessionError::Storage {
                message: format!(
                    "Failed to sync parent directory {}: {}",
                    parent.display(),
                    e
                ),
            })?;
    }

    Ok(())
}

#[cfg(test)]
fn write_entries_sync_atomic(path: &Path, entries: &[JsonlEntry], sync: bool) -> SessionResult<()> {
    let temp_path = write_entries_to_temp_sync(path, entries, sync)?;
    let publish_result = publish_temp_file_no_overwrite(&temp_path, path, sync);
    if publish_result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    publish_result
}

// ============================================================================
// JSONL Persistence Implementation
// ============================================================================

pub struct JsonlPersistence {
    config: JsonlConfig,
    index: Arc<RwLock<SessionIndex>>,
    queue: Arc<RwLock<HashMap<SessionId, Vec<QueueItem>>>>,
    mutation_lock: Arc<Mutex<()>>,
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
            queue: Arc::new(RwLock::new(HashMap::new())),
            mutation_lock: Arc::new(Mutex::new(())),
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

                let (meta, session_queue) =
                    Self::parse_file_metadata(session_id, file_path, &parsed);

                index.insert(session_id, meta);
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
    ) -> (SessionMeta, Vec<QueueItem>) {
        let mut project_path: Option<PathBuf> = None;
        let mut tenant_id: Option<String> = None;
        let mut updated_at = Utc::now();
        let mut expires_at: Option<DateTime<Utc>> = None;
        let mut last_session_meta: Option<SessionMetaEntry> = None;
        let mut queue_items: HashMap<String, QueueItem> = HashMap::new();
        let mut queue_order: Vec<String> = Vec::new();
        let mut persisted_ids = HashSet::with_capacity(entries.len());
        let mut todos_map: HashMap<String, TodoItem> = HashMap::new();
        let mut latest_plan: Option<Plan> = None;
        let mut current_leaf_id: Option<MessageId> = None;
        let mut primary_branch_id: Option<Uuid> = None;

        for entry in entries {
            match entry {
                JsonlEntry::GraphEvent(e) => {
                    if project_path.is_none() {
                        project_path = project_path_from_graph_event(&e.event);
                    }
                    updated_at = updated_at.max(e.event.metadata.occurred_at);
                    persisted_ids.insert(format!("graph:{}", e.event_id));
                    if primary_branch_id.is_none() {
                        primary_branch_id = Self::primary_branch_from_event(&e.event);
                    }
                    if let Some(leaf_id) = Self::current_leaf_from_event(&e.event) {
                        current_leaf_id = Some(leaf_id);
                    }
                }
                JsonlEntry::SessionMeta(m) => {
                    project_path.clone_from(&m.project_path);
                    tenant_id.clone_from(&m.tenant_id);
                    updated_at = m.updated_at;
                    expires_at = m.expires_at;
                    last_session_meta = Some(m.clone());
                }
                JsonlEntry::QueueOperation(q) => {
                    let item_id = match Uuid::parse_str(&q.item_id) {
                        Ok(id) => id,
                        Err(_) => continue,
                    };
                    match q.operation.as_str() {
                        "enqueue" => {
                            if !queue_items.contains_key(&q.item_id) {
                                queue_order.push(q.item_id.clone());
                            }
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
                JsonlEntry::QueueReset(_) => {
                    queue_items.clear();
                    queue_order.clear();
                }
                JsonlEntry::Todo(t) => {
                    let Some(todo_id) = parse_auxiliary_uuid(&session_id, "todo", &t.id) else {
                        continue;
                    };
                    todos_map.insert(
                        t.id.clone(),
                        TodoItem {
                            id: todo_id,
                            session_id,
                            content: t.content.clone(),
                            active_form: t.active_form.clone(),
                            status: jsonl_to_enum(&t.status).unwrap_or_default(),
                            plan_id: t.plan_id.as_ref().and_then(|s| Uuid::parse_str(s).ok()),
                            created_at: t.created_at,
                            started_at: t.started_at,
                            completed_at: t.completed_at,
                        },
                    );
                }
                JsonlEntry::TodoReset(_) => {
                    todos_map.clear();
                }
                JsonlEntry::Plan(p) => {
                    let Some(plan_id) = parse_auxiliary_uuid(&session_id, "plan", &p.id) else {
                        continue;
                    };
                    latest_plan = Some(Plan {
                        id: plan_id,
                        session_id,
                        name: p.name.clone(),
                        content: p.content.clone(),
                        status: jsonl_to_enum(&p.status).unwrap_or_default(),
                        error: p.error.clone(),
                        created_at: p.created_at,
                        approved_at: p.approved_at,
                        started_at: p.started_at,
                        completed_at: p.completed_at,
                    });
                }
                JsonlEntry::PlanReset(_) => {
                    latest_plan = None;
                }
                _ => {}
            }
        }

        let queue: Vec<QueueItem> = queue_order
            .into_iter()
            .filter_map(|item_id| queue_items.remove(&item_id))
            .collect();
        let todos: Vec<TodoItem> = todos_map.into_values().collect();
        let todos_hash = Self::compute_todos_hash(&todos);
        let plan_hash = Self::compute_plan_hash(latest_plan.as_ref());

        (
            SessionMeta {
                path,
                project_path,
                tenant_id,
                updated_at,
                expires_at,
                last_session_meta,
                persisted_ids,
                todos_hash,
                plan_hash,
                current_leaf_id,
                primary_branch_id,
            },
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
            .current_branch_messages()
            .first()
            .and_then(|m| m.environment.as_ref())
            .and_then(|e| e.cwd.clone())
    }

    fn project_path_from_message(message: &SessionMessage) -> Option<PathBuf> {
        message
            .environment
            .as_ref()
            .and_then(|environment| environment.cwd.clone())
    }

    fn primary_branch_from_event(event: &GraphEvent) -> Option<Uuid> {
        match &event.body {
            crate::graph::GraphEventBody::NodeAppended { branch_id, .. }
            | crate::graph::GraphEventBody::BranchForked { branch_id, .. }
            | crate::graph::GraphEventBody::CheckpointCreated { branch_id, .. }
            | crate::graph::GraphEventBody::BookmarkCreated { branch_id, .. } => Some(*branch_id),
            crate::graph::GraphEventBody::NodeMetadataPatched { .. } => None,
        }
    }

    fn current_leaf_from_event(event: &GraphEvent) -> Option<MessageId> {
        match &event.body {
            crate::graph::GraphEventBody::NodeAppended { node_id, .. } => {
                Some(MessageId::from_string(node_id.to_string()))
            }
            crate::graph::GraphEventBody::CheckpointCreated { checkpoint_id, .. } => {
                Some(MessageId::from_string(checkpoint_id.to_string()))
            }
            crate::graph::GraphEventBody::NodeMetadataPatched { .. } => None,
            _ => None,
        }
    }

    fn usage_info_with_increment(base: &UsageInfo, usage: &Option<TokenUsage>) -> UsageInfo {
        let mut next = base.clone();
        if let Some(usage) = usage {
            next.input_tokens += usage.input_tokens;
            next.output_tokens += usage.output_tokens;
            next.cache_creation_input_tokens += usage.cache_creation_input_tokens;
            next.cache_read_input_tokens += usage.cache_read_input_tokens;
        }
        next
    }

    async fn append_entries(&self, path: PathBuf, entries: Vec<JsonlEntry>) -> SessionResult<()> {
        let sync = self.config.sync_mode == SyncMode::OnWrite;
        tokio::task::spawn_blocking(move || append_entries_sync(&path, &entries, sync))
            .await
            .map_err(|e| SessionError::Storage {
                message: format!("Task join error: {}", e),
            })??;
        Ok(())
    }

    fn normalized_pending_queue(session_id: SessionId, items: &[QueueItem]) -> Vec<QueueItem> {
        items
            .iter()
            .cloned()
            .map(|mut item| {
                item.session_id = session_id;
                item.status = QueueStatus::Pending;
                item.processed_at = None;
                item
            })
            .collect()
    }

    fn snapshot_entries(
        session: &Session,
        project_path: Option<PathBuf>,
        pending_queue: &[QueueItem],
    ) -> Vec<JsonlEntry> {
        let mut entries = Vec::new();
        entries.push(Self::session_to_meta_entry(session, project_path));
        entries.extend(
            session
                .graph
                .events
                .iter()
                .map(|event| JsonlEntry::from_graph_event(&session.id, event)),
        );
        entries.extend(session.todos.iter().map(|todo| {
            JsonlEntry::Todo(TodoEntry {
                id: todo.id.to_string(),
                session_id: session.id.to_string(),
                content: todo.content.clone(),
                active_form: todo.active_form.clone(),
                status: enum_to_jsonl(&todo.status, "pending"),
                plan_id: todo.plan_id.map(|id| id.to_string()),
                created_at: todo.created_at,
                started_at: todo.started_at,
                completed_at: todo.completed_at,
            })
        }));
        if let Some(plan) = &session.current_plan {
            entries.push(JsonlEntry::Plan(PlanEntry {
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
        entries.extend(session.compact_history.iter().map(|compact| {
            JsonlEntry::Compact(CompactEntry {
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
            })
        }));
        if !pending_queue.is_empty() {
            entries.push(JsonlEntry::QueueReset(ResetEntry {
                session_id: session.id.to_string(),
                timestamp: session.updated_at,
            }));
            entries.extend(pending_queue.iter().map(|item| {
                JsonlEntry::QueueOperation(QueueOperationEntry {
                    operation: "enqueue".to_string(),
                    session_id: session.id.to_string(),
                    timestamp: item.created_at,
                    content: item.content.clone(),
                    priority: item.priority,
                    item_id: item.id.to_string(),
                })
            }));
        }
        entries
    }

    async fn fallback_append_graph_event(
        &self,
        session_id: &SessionId,
        event: GraphEvent,
    ) -> SessionResult<()> {
        let mut session = self
            .load(session_id)
            .await?
            .ok_or_else(|| SessionError::NotFound {
                id: session_id.to_string(),
            })?;
        let graph_id = session.graph.id;
        let created_at = session.graph.created_at;
        let primary_branch = session.graph.primary_branch;
        session.graph.events.push(event.clone());
        session.graph = GraphMaterializer::from_events_with_primary(
            &session.graph.events,
            Some(primary_branch),
        );
        session.graph.id = graph_id;
        session.graph.created_at = created_at;
        session.refresh_summary_cache();
        session.refresh_message_projection();
        session.updated_at = event.metadata.occurred_at;

        self.save_inner(&session).await
    }

    async fn fallback_add_message(
        &self,
        session_id: &SessionId,
        message: SessionMessage,
    ) -> SessionResult<()> {
        let mut session = self
            .load(session_id)
            .await?
            .ok_or_else(|| SessionError::NotFound {
                id: session_id.to_string(),
            })?;
        session.add_message(message)?;
        self.save_inner(&session).await
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

    fn session_to_meta_entry(session: &Session, project_path: Option<PathBuf>) -> JsonlEntry {
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
            project_path,
            parent_session_id: session.parent_id.map(|p| p.to_string()),
            tenant_id: session.tenant_id.clone(),
            principal_id: session.principal_id.clone(),
            session_type: serde_json::to_value(&session.session_type)
                .unwrap_or_else(|e| warn_serialize("session_type", e)),
            mode: "stateless".to_string(),
            state: enum_to_jsonl(&session.state, "created"),
            config: serde_json::to_value(&session.config)
                .unwrap_or_else(|e| warn_serialize("config", e)),
            authorization_policy: serde_json::to_value(&session.authorization)
                .unwrap_or_else(|e| warn_serialize("authorization", e)),
            total_usage: UsageInfo::from(&session.total_usage),
            total_cost_usd: session.total_cost_usd,
            static_context_hash: session.static_context_hash.clone(),
            error: session.error.clone(),
            created_at: session.created_at,
            updated_at: session.updated_at,
            expires_at: session.expires_at,
        })
    }

    async fn save_inner(&self, session: &Session) -> SessionResult<()> {
        validate_graph(&session.id, &session.graph)?;
        let (project_path, persisted_ids, prev_session_meta, prev_todos_hash, prev_plan_hash) = {
            let index = self.index.read().await;
            match index.sessions.get(&session.id) {
                Some(m) => (
                    m.project_path
                        .clone()
                        .or_else(|| Self::get_project_path(session)),
                    m.persisted_ids.clone(),
                    m.last_session_meta.clone(),
                    m.todos_hash,
                    m.plan_hash,
                ),
                None => (
                    Self::get_project_path(session),
                    HashSet::new(),
                    None,
                    Self::compute_todos_hash(&[]),
                    Self::compute_plan_hash(None),
                ),
            }
        };
        let file_path = self.session_file_path(&session.id, project_path.as_deref());

        let mut new_entries = Vec::new();
        let mut new_ids = HashSet::new();
        let session_meta_entry = match Self::session_to_meta_entry(session, project_path.clone()) {
            JsonlEntry::SessionMeta(entry) => entry,
            _ => unreachable!("session_to_meta_entry must return session metadata"),
        };
        if prev_session_meta.as_ref() != Some(&session_meta_entry) {
            new_entries.push(JsonlEntry::SessionMeta(session_meta_entry.clone()));
        }

        for event in &session.graph.events {
            let event_id = format!("graph:{}", event.metadata.id);
            if !persisted_ids.contains(&event_id) {
                new_entries.push(JsonlEntry::from_graph_event(&session.id, event));
                new_ids.insert(event_id);
            }
        }

        let current_todos_hash = Self::compute_todos_hash(&session.todos);
        let current_plan_hash = Self::compute_plan_hash(session.current_plan.as_ref());

        if current_todos_hash != prev_todos_hash {
            new_entries.push(JsonlEntry::TodoReset(ResetEntry {
                session_id: session.id.to_string(),
                timestamp: session.updated_at,
            }));
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

        if current_plan_hash != prev_plan_hash {
            new_entries.push(JsonlEntry::PlanReset(ResetEntry {
                session_id: session.id.to_string(),
                timestamp: session.updated_at,
            }));
            if let Some(ref plan) = session.current_plan {
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
        }

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
                last_session_meta: Some(session_meta_entry),
                persisted_ids: persisted,
                todos_hash: current_todos_hash,
                plan_hash: current_plan_hash,
                current_leaf_id: session.current_leaf_id.clone(),
                primary_branch_id: Some(session.graph.primary_branch),
            },
        );

        Ok(())
    }

    fn reconstruct_session(
        session_id: SessionId,
        entries: Vec<JsonlEntry>,
    ) -> SessionResult<Session> {
        let mut session = Session::new(SessionConfig::default());
        session.id = session_id;

        let mut todos_map: HashMap<String, TodoItem> = HashMap::new();
        let mut latest_plan: Option<Plan> = None;
        let mut compacts: Vec<CompactRecord> = Vec::new();
        let mut graph_events: Vec<GraphEvent> = Vec::new();
        let mut primary_branch_id: Option<Uuid> = None;

        for entry in entries {
            match entry {
                JsonlEntry::SessionMeta(m) => {
                    session.tenant_id = m.tenant_id;
                    session.principal_id = m.principal_id;
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
                    session.authorization = serde_json::from_value(m.authorization_policy)
                        .unwrap_or_else(|e| {
                            tracing::warn!(
                                session_id = %session_id,
                                error = %e,
                                "Failed to deserialize session authorization, using default"
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
                JsonlEntry::Todo(t) => {
                    let Some(todo_id) = parse_auxiliary_uuid(&session_id, "todo", &t.id) else {
                        continue;
                    };
                    let todo = TodoItem {
                        id: todo_id,
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
                JsonlEntry::TodoReset(_) => {
                    todos_map.clear();
                }
                JsonlEntry::Plan(p) => {
                    let Some(plan_id) = parse_auxiliary_uuid(&session_id, "plan", &p.id) else {
                        continue;
                    };
                    let plan = Plan {
                        id: plan_id,
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
                JsonlEntry::PlanReset(_) => {
                    latest_plan = None;
                }
                JsonlEntry::Compact(c) => {
                    let Some(compact_id) = parse_auxiliary_uuid(&session_id, "compact", &c.id)
                    else {
                        continue;
                    };
                    compacts.push(CompactRecord {
                        id: compact_id,
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
                JsonlEntry::GraphEvent(entry) => {
                    if primary_branch_id.is_none() {
                        primary_branch_id = Self::primary_branch_from_event(&entry.event);
                    }
                    graph_events.push(entry.event);
                }
                JsonlEntry::QueueReset(_) => {}
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
        session.graph =
            GraphMaterializer::from_events_with_primary(&graph_events, primary_branch_id);
        session.graph.id = session.id.0;
        session.graph.created_at = session.created_at;
        validate_graph(&session_id, &session.graph)?;
        session.summary = session.graph.latest_summary();
        session.refresh_message_projection();

        Ok(session)
    }
}

#[async_trait::async_trait]
impl Persistence for JsonlPersistence {
    fn name(&self) -> &str {
        "jsonl"
    }

    async fn save(&self, session: &Session) -> SessionResult<()> {
        let _guard = self.mutation_lock.lock().await;
        self.save_inner(session).await
    }

    async fn append_graph_event(
        &self,
        session_id: &SessionId,
        event: GraphEvent,
    ) -> SessionResult<()> {
        let _guard = self.mutation_lock.lock().await;
        let meta = {
            let index = self.index.read().await;
            index
                .sessions
                .get(session_id)
                .cloned()
                .ok_or_else(|| SessionError::NotFound {
                    id: session_id.to_string(),
                })?
        };

        let event_id = format!("graph:{}", event.metadata.id);
        if meta.persisted_ids.contains(&event_id) {
            return Ok(());
        }

        let target_project_path = meta
            .project_path
            .clone()
            .or_else(|| project_path_from_graph_event(&event));
        let target_path = self.session_file_path(session_id, target_project_path.as_deref());
        if target_path != meta.path || meta.last_session_meta.is_none() {
            return self.fallback_append_graph_event(session_id, event).await;
        }

        let mut session_meta_entry = match meta.last_session_meta.clone() {
            Some(entry) => entry,
            None => return self.fallback_append_graph_event(session_id, event).await,
        };
        session_meta_entry.project_path = target_project_path.clone();
        session_meta_entry.updated_at = event.metadata.occurred_at;

        self.append_entries(
            meta.path.clone(),
            vec![
                JsonlEntry::from_graph_event(session_id, &event),
                JsonlEntry::SessionMeta(session_meta_entry.clone()),
            ],
        )
        .await?;

        let mut persisted_ids = meta.persisted_ids;
        persisted_ids.insert(event_id);

        let mut index = self.index.write().await;
        index.insert(
            *session_id,
            SessionMeta {
                path: meta.path,
                project_path: target_project_path,
                tenant_id: session_meta_entry.tenant_id.clone(),
                updated_at: session_meta_entry.updated_at,
                expires_at: session_meta_entry.expires_at,
                last_session_meta: Some(session_meta_entry),
                persisted_ids,
                todos_hash: meta.todos_hash,
                plan_hash: meta.plan_hash,
                current_leaf_id: Self::current_leaf_from_event(&event).or(meta.current_leaf_id),
                primary_branch_id: meta
                    .primary_branch_id
                    .or_else(|| Self::primary_branch_from_event(&event)),
            },
        );

        Ok(())
    }

    async fn add_message(
        &self,
        session_id: &SessionId,
        mut message: SessionMessage,
    ) -> SessionResult<()> {
        let _guard = self.mutation_lock.lock().await;
        let meta = {
            let index = self.index.read().await;
            index
                .sessions
                .get(session_id)
                .cloned()
                .ok_or_else(|| SessionError::NotFound {
                    id: session_id.to_string(),
                })?
        };

        let target_project_path = meta
            .project_path
            .clone()
            .or_else(|| Self::project_path_from_message(&message));
        let target_path = self.session_file_path(session_id, target_project_path.as_deref());
        let Some(last_session_meta) = meta.last_session_meta.clone() else {
            return self.fallback_add_message(session_id, message).await;
        };
        if target_path != meta.path {
            return self.fallback_add_message(session_id, message).await;
        }

        if let Some(parent_id) = meta.current_leaf_id.clone() {
            message.parent_id = Some(parent_id);
        }

        let session_type: SessionType =
            serde_json::from_value(last_session_meta.session_type.clone()).unwrap_or_default();
        let primary_branch_id = meta.primary_branch_id.unwrap_or_else(Uuid::new_v4);
        let node_id = graph_node_id_for_message(&message)?;
        let parent_id = graph_parent_node_id_for_message(&message)?;
        let event = GraphEvent {
            metadata: crate::graph::EventMetadata {
                id: Uuid::new_v4(),
                occurred_at: message.timestamp,
                actor: last_session_meta.principal_id.clone(),
            },
            body: crate::graph::GraphEventBody::NodeAppended {
                node_id,
                branch_id: primary_branch_id,
                parent_id,
                kind: graph_node_kind_for_message(&message),
                tags: graph_tags_for_message(&message),
                payload: graph_payload_for_message(&message),
                provenance: build_graph_provenance(*session_id, &session_type),
            },
        };

        let mut session_meta_entry = last_session_meta.clone();
        session_meta_entry.project_path = target_project_path.clone();
        session_meta_entry.updated_at = message.timestamp;
        session_meta_entry.total_usage =
            Self::usage_info_with_increment(&session_meta_entry.total_usage, &message.usage);

        self.append_entries(
            meta.path.clone(),
            vec![
                JsonlEntry::from_graph_event(session_id, &event),
                JsonlEntry::SessionMeta(session_meta_entry.clone()),
            ],
        )
        .await?;

        let mut persisted_ids = meta.persisted_ids;
        persisted_ids.insert(format!("graph:{}", event.metadata.id));

        let mut index = self.index.write().await;
        index.insert(
            *session_id,
            SessionMeta {
                path: meta.path,
                project_path: target_project_path,
                tenant_id: session_meta_entry.tenant_id.clone(),
                updated_at: session_meta_entry.updated_at,
                expires_at: session_meta_entry.expires_at,
                last_session_meta: Some(session_meta_entry),
                persisted_ids,
                todos_hash: meta.todos_hash,
                plan_hash: meta.plan_hash,
                current_leaf_id: Some(message.id),
                primary_branch_id: Some(primary_branch_id),
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

        let session = Self::reconstruct_session(*id, entries)?;
        Ok(Some(session))
    }

    async fn delete(&self, id: &SessionId) -> SessionResult<bool> {
        let _guard = self.mutation_lock.lock().await;
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

    async fn list_children(&self, parent_id: &SessionId) -> SessionResult<Vec<SessionId>> {
        let parent_id = parent_id.to_string();
        let index = self.index.read().await;
        Ok(index
            .sessions
            .iter()
            .filter_map(|(session_id, meta)| {
                let is_child = meta
                    .last_session_meta
                    .as_ref()
                    .and_then(|entry| entry.parent_session_id.as_deref())
                    == Some(parent_id.as_str());
                is_child.then_some(*session_id)
            })
            .collect())
    }

    async fn enqueue(
        &self,
        session_id: &SessionId,
        content: String,
        priority: i32,
    ) -> SessionResult<QueueItem> {
        let _guard = self.mutation_lock.lock().await;
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
        let _guard = self.mutation_lock.lock().await;
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
        let _guard = self.mutation_lock.lock().await;
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

    async fn replace_pending_queue(
        &self,
        session_id: &SessionId,
        items: &[QueueItem],
    ) -> SessionResult<()> {
        let _guard = self.mutation_lock.lock().await;
        let path = {
            let index = self.index.read().await;
            index.sessions.get(session_id).map(|m| m.path.clone())
        };

        if let Some(path) = path {
            let mut entries = Vec::with_capacity(items.len() + 1);
            entries.push(JsonlEntry::QueueReset(ResetEntry {
                session_id: session_id.to_string(),
                timestamp: Utc::now(),
            }));
            entries.extend(items.iter().map(|item| {
                let mut item = item.clone();
                item.session_id = *session_id;
                item.status = QueueStatus::Pending;
                item.processed_at = None;
                JsonlEntry::QueueOperation(QueueOperationEntry {
                    operation: "enqueue".to_string(),
                    session_id: session_id.to_string(),
                    timestamp: item.created_at,
                    content: item.content,
                    priority: item.priority,
                    item_id: item.id.to_string(),
                })
            }));

            let sync = self.config.sync_mode == SyncMode::OnWrite;
            tokio::task::spawn_blocking(move || append_entries_sync(&path, &entries, sync))
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Task join error: {}", e),
                })??;
        }

        let normalized: Vec<QueueItem> = items
            .iter()
            .cloned()
            .map(|mut item| {
                item.session_id = *session_id;
                item.status = QueueStatus::Pending;
                item.processed_at = None;
                item
            })
            .collect();
        self.queue.write().await.insert(*session_id, normalized);
        Ok(())
    }

    async fn restore_bundle(
        &self,
        session: &Session,
        pending_queue: &[QueueItem],
    ) -> SessionResult<()> {
        let _guard = self.mutation_lock.lock().await;
        let project_path = Self::get_project_path(session);
        let file_path = self.session_file_path(&session.id, project_path.as_deref());
        let normalized_queue = Self::normalized_pending_queue(session.id, pending_queue);
        let entries = Self::snapshot_entries(session, project_path, &normalized_queue);

        {
            let index = self.index.read().await;
            if index.sessions.contains_key(&session.id) || file_path.exists() {
                return Err(SessionError::Storage {
                    message: format!(
                        "Archive restore refuses to overwrite existing session {}",
                        session.id
                    ),
                });
            }
        }

        let sync = self.config.sync_mode == SyncMode::OnWrite;
        let temp_path = {
            let path_clone = file_path.clone();
            let entries_for_write = entries.clone();
            tokio::task::spawn_blocking(move || {
                write_entries_to_temp_sync(&path_clone, &entries_for_write, sync)
            })
            .await
            .map_err(|e| SessionError::Storage {
                message: format!("Task join error: {}", e),
            })??
        };

        let parsed_entries = {
            let temp_path_clone = temp_path.clone();
            tokio::task::spawn_blocking(move || read_entries_sync(&temp_path_clone))
                .await
                .map_err(|e| SessionError::Storage {
                    message: format!("Task join error: {}", e),
                })??
        };

        let restored = match Self::reconstruct_session(session.id, parsed_entries.clone()) {
            Ok(session) => session,
            Err(error) => {
                let _ = std::fs::remove_file(&temp_path);
                return Err(error);
            }
        };
        let (meta, restored_queue) =
            Self::parse_file_metadata(session.id, file_path.clone(), &parsed_entries);
        if let Err(error) = verify_restored_session_roundtrip(
            session,
            &normalized_queue,
            &restored,
            &restored_queue,
        ) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }

        let mut index = self.index.write().await;
        let mut queue = self.queue.write().await;

        if index.sessions.contains_key(&session.id) || file_path.exists() {
            let _ = std::fs::remove_file(&temp_path);
            return Err(SessionError::Storage {
                message: format!(
                    "Archive restore refuses to overwrite existing session {}",
                    session.id
                ),
            });
        }

        if let Err(error) = publish_temp_file_no_overwrite(&temp_path, &file_path, sync) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        if restored_queue.is_empty() {
            queue.remove(&session.id);
        } else {
            queue.insert(session.id, restored_queue);
        }
        index.insert(session.id, meta);

        Ok(())
    }

    async fn cleanup_expired(&self) -> SessionResult<usize> {
        let _guard = self.mutation_lock.lock().await;
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
            let mut queue = self.queue.write().await;
            for id in &expired_ids {
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
    use crate::session::EnvironmentContext;
    use crate::session::{SessionMessage, TodoItem};
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
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]))
            .unwrap();
        session
            .add_message(SessionMessage::assistant(vec![ContentBlock::text(
                "Hi there!",
            )]))
            .unwrap();

        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.current_branch_messages().len(), 2);
    }

    #[tokio::test]
    async fn test_incremental_save() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("First")]))
            .unwrap();
        persistence.save(&session).await.unwrap();

        session
            .add_message(SessionMessage::assistant(vec![ContentBlock::text(
                "Second",
            )]))
            .unwrap();
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
    async fn test_replace_pending_queue_resets_previous_items() {
        let (persistence, _temp) = create_test_persistence().await;

        let session = Session::new(SessionConfig::default());
        persistence.save(&session).await.unwrap();

        persistence
            .enqueue(&session.id, "old".to_string(), 1)
            .await
            .unwrap();

        let replacement = vec![
            QueueItem::enqueue(session.id, "new-high").priority(10),
            QueueItem::enqueue(session.id, "new-low").priority(1),
        ];

        persistence
            .replace_pending_queue(&session.id, &replacement)
            .await
            .unwrap();

        let loaded = persistence.pending_queue(&session.id).await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.iter().any(|item| item.content == "new-high"));
        assert!(loaded.iter().any(|item| item.content == "new-low"));
        assert!(!loaded.iter().any(|item| item.content == "old"));
    }

    #[tokio::test]
    async fn test_dag_reconstruction() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("Q1")]))
            .unwrap();
        session
            .add_message(SessionMessage::assistant(vec![ContentBlock::text("A1")]))
            .unwrap();
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("Q2")]))
            .unwrap();
        session
            .add_message(SessionMessage::assistant(vec![ContentBlock::text("A2")]))
            .unwrap();

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
    fn test_graph_event_entry_serialization() {
        let session_id = SessionId::new();
        let event = GraphEvent::new(crate::graph::GraphEventBody::NodeAppended {
            node_id: Uuid::new_v4(),
            branch_id: Uuid::new_v4(),
            parent_id: None,
            kind: crate::graph::NodeKind::User,
            tags: vec!["test".to_string()],
            payload: serde_json::json!({"content": []}),
            provenance: None,
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
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]))
            .unwrap();
        persistence.save(&session).await.unwrap();
        persistence.save(&session).await.unwrap(); // Save same data twice
        persistence.save(&session).await.unwrap(); // And again

        // Graph-canonical JSONL should not persist message projection entries.
        let file_path = persistence.session_file_path(&session.id, None);
        let entries = read_entries_sync(&file_path).unwrap();
        assert_eq!(
            entries.len(),
            2,
            "Should persist only session metadata and graph events for a simple session"
        );

        let graph_event_count = entries
            .iter()
            .filter(|e| e.graph_event_id().is_some())
            .count();
        assert_eq!(
            graph_event_count, 1,
            "Should not duplicate graph event entries"
        );
    }

    #[test]
    fn test_atomic_restore_write_refuses_existing_destination() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("session.jsonl");
        std::fs::write(&target, "{\"type\":\"session_meta\"}\n").unwrap();

        let entry = JsonlEntry::SessionMeta(SessionMetaEntry {
            session_id: SessionId::new().to_string(),
            project_path: None,
            parent_session_id: None,
            tenant_id: None,
            principal_id: None,
            session_type: serde_json::json!("main"),
            mode: "stateless".to_string(),
            state: "created".to_string(),
            config: serde_json::json!({}),
            authorization_policy: serde_json::json!({}),
            total_usage: UsageInfo::default(),
            total_cost_usd: Decimal::default(),
            static_context_hash: None,
            error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            expires_at: None,
        });

        let error = write_entries_sync_atomic(&target, &[entry], false).unwrap_err();
        assert!(error.to_string().contains("without overwrite"));
        let contents = std::fs::read_to_string(&target).unwrap();
        assert_eq!(contents, "{\"type\":\"session_meta\"}\n");
    }

    #[tokio::test]
    async fn test_todo_reset_removes_deleted_items() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session.set_todos(vec![
            TodoItem::new(session.id, "one", "one"),
            TodoItem::new(session.id, "two", "two"),
        ]);
        persistence.save(&session).await.unwrap();

        session.set_todos(vec![session.todos[0].clone()]);
        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.todos.len(), 1);
        assert_eq!(loaded.todos[0].content, "one");
    }

    #[tokio::test]
    async fn test_plan_reset_clears_removed_plan() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session.enter_plan_mode(Some("demo".to_string()));
        session.update_plan_content("ship it".to_string());
        persistence.save(&session).await.unwrap();

        session.cancel_plan();
        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert!(loaded.current_plan.is_none());
    }

    #[tokio::test]
    async fn test_invalid_auxiliary_entry_ids_are_skipped() {
        let (persistence, _temp) = create_test_persistence().await;

        let session = Session::new(SessionConfig::default());
        persistence.save(&session).await.unwrap();

        let file_path = persistence.session_file_path(&session.id, None);
        append_entries_sync(
            &file_path,
            &[
                JsonlEntry::Todo(TodoEntry {
                    id: "invalid-todo-id".to_string(),
                    session_id: session.id.to_string(),
                    content: "broken todo".to_string(),
                    active_form: "broken todo".to_string(),
                    status: "pending".to_string(),
                    plan_id: None,
                    created_at: Utc::now(),
                    started_at: None,
                    completed_at: None,
                }),
                JsonlEntry::Plan(PlanEntry {
                    id: "invalid-plan-id".to_string(),
                    session_id: session.id.to_string(),
                    name: Some("broken plan".to_string()),
                    content: "bad".to_string(),
                    status: "draft".to_string(),
                    error: None,
                    created_at: Utc::now(),
                    approved_at: None,
                    started_at: None,
                    completed_at: None,
                }),
                JsonlEntry::Compact(CompactEntry {
                    id: "invalid-compact-id".to_string(),
                    session_id: session.id.to_string(),
                    trigger: "manual".to_string(),
                    pre_tokens: 10,
                    post_tokens: 5,
                    saved_tokens: 5,
                    summary: "broken compact".to_string(),
                    original_count: 2,
                    new_count: 1,
                    logical_parent_id: None,
                    created_at: Utc::now(),
                }),
            ],
            false,
        )
        .unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert!(loaded.todos.is_empty());
        assert!(loaded.current_plan.is_none());
        assert!(loaded.compact_history.is_empty());

        persistence.save(&loaded).await.unwrap();
        let reloaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert!(reloaded.todos.is_empty());
        assert!(reloaded.current_plan.is_none());
        assert!(reloaded.compact_history.is_empty());
    }

    #[tokio::test]
    async fn test_project_path_persists_after_graph_only_reload() {
        let (persistence, _temp) = create_test_persistence().await;
        let project_dir = tempfile::tempdir().unwrap();

        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(
                SessionMessage::user(vec![ContentBlock::text("Hello")])
                    .environment(EnvironmentContext::capture(Some(project_dir.path()))),
            )
            .unwrap();

        persistence.save(&session).await.unwrap();

        let expected_path = persistence.session_file_path(&session.id, Some(project_dir.path()));
        assert!(expected_path.exists());

        let mut loaded = persistence.load(&session.id).await.unwrap().unwrap();
        loaded.clear_messages();
        persistence.save(&loaded).await.unwrap();

        assert!(expected_path.exists());
        let default_path = persistence.session_file_path(&session.id, None);
        assert_ne!(expected_path, default_path);
        assert!(!default_path.exists());
    }

    #[tokio::test]
    async fn test_graph_events_roundtrip() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]))
            .unwrap();
        session
            .add_message(SessionMessage::assistant(vec![ContentBlock::text("Hi")]))
            .unwrap();
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
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]))
            .unwrap();
        session.bookmark_current_head("start", Some("saved".to_string()));

        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.graph.bookmarks.len(), 1);
    }

    #[tokio::test]
    async fn test_checkpoints_roundtrip() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]))
            .unwrap();
        session
            .graph
            .create_checkpoint(
                session.graph.primary_branch,
                "milestone",
                Some("saved".to_string()),
                vec!["tag".to_string()],
                None,
                None,
            )
            .unwrap();

        persistence.save(&session).await.unwrap();

        let loaded = persistence.load(&session.id).await.unwrap().unwrap();
        assert_eq!(loaded.graph.checkpoints.len(), 1);
    }

    #[tokio::test]
    async fn test_graph_first_roundtrip_uses_projection_helper() {
        let (persistence, _temp) = create_test_persistence().await;

        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("hello")]))
            .unwrap();
        session
            .add_message(SessionMessage::assistant(vec![ContentBlock::text("world")]))
            .unwrap();
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
            provenance: None,
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
