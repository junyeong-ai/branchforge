//! Session state management.

mod config;
mod enums;
mod ids;
mod message;
mod policy;

pub use config::SessionConfig;
pub use enums::{SessionState, SessionType};
pub use ids::{MessageId, SessionId};
pub use message::{
    ExecutionMetadata, MessageMetadata, SessionMessage, ThinkingMetadata, ToolResultMeta,
};
pub use policy::{SessionAuthorization, SessionExecutionMode, SessionToolLimits};

use std::collections::VecDeque;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::events::EventBus;
use crate::graph::{GraphNode, NodeId, NodeKind, NodeProvenance, SessionGraph};
use crate::ir::{ContentPart, Message, Role};
use crate::session::types::{CompactRecord, Plan, TodoItem, TodoStatus};
use crate::session::{SessionError, SessionResult};
use crate::types::{CacheTtl, TokenUsage, Usage};

/// Transient content overrides for micro-compaction.
///
/// Applied in `to_api_messages()` to reduce token usage without
/// modifying the append-only graph. Keyed by graph `NodeId`.
/// Intentionally excluded from serialization: lost on reload.
#[derive(Clone, Debug, Default)]
pub struct ContentOverrides {
    replacements: std::collections::HashMap<NodeId, Vec<ContentPart>>,
}

impl ContentOverrides {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn is_empty(&self) -> bool {
        self.replacements.is_empty()
    }
    pub fn len(&self) -> usize {
        self.replacements.len()
    }
    pub fn set(&mut self, node_id: NodeId, content: Vec<ContentPart>) {
        self.replacements.insert(node_id, content);
    }
    pub fn remove(&mut self, node_id: &NodeId) {
        self.replacements.remove(node_id);
    }
    pub fn clear(&mut self) {
        self.replacements.clear();
    }
    pub fn get(&self, node_id: &NodeId) -> Option<&Vec<ContentPart>> {
        self.replacements.get(node_id)
    }
}

const MAX_COMPACT_HISTORY_SIZE: usize = 50;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub parent_id: Option<SessionId>,
    pub session_type: SessionType,
    pub tenant_id: Option<String>,
    pub principal_id: Option<String>,
    pub state: SessionState,
    pub config: SessionConfig,
    pub authorization: SessionAuthorization,
    pub messages: Vec<SessionMessage>,
    pub current_leaf_id: Option<MessageId>,
    pub summary: Option<String>,
    pub total_usage: TokenUsage,
    #[serde(default)]
    pub current_input_tokens: u64,
    pub total_cost_usd: Decimal,
    pub static_context_hash: Option<String>,
    #[serde(default)]
    pub graph: SessionGraph,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    #[serde(default)]
    pub todos: Vec<TodoItem>,
    #[serde(default)]
    pub current_plan: Option<Plan>,
    #[serde(default)]
    pub compact_history: VecDeque<CompactRecord>,
    #[serde(skip)]
    pub(crate) event_bus: Option<Arc<EventBus>>,
    #[serde(skip)]
    pub content_overrides: ContentOverrides,
}

impl Session {
    pub fn new(config: SessionConfig) -> Self {
        Self::from_id(SessionId::new(), config)
    }

    pub fn from_id(id: SessionId, config: SessionConfig) -> Self {
        Self::init(id, None, SessionType::Main, config)
    }

    pub fn new_subagent(
        parent_id: SessionId,
        agent_type: impl Into<String>,
        description: impl Into<String>,
        config: SessionConfig,
    ) -> Self {
        Self::new_subagent_with_id(SessionId::new(), parent_id, agent_type, description, config)
    }

    pub fn new_subagent_with_id(
        id: SessionId,
        parent_id: SessionId,
        agent_type: impl Into<String>,
        description: impl Into<String>,
        config: SessionConfig,
    ) -> Self {
        let session_type = SessionType::Subagent {
            agent_type: agent_type.into(),
            description: description.into(),
        };
        Self::init(id, Some(parent_id), session_type, config)
    }

    /// Attach an [`EventBus`] for non-blocking observability events.
    pub fn with_event_bus(&mut self, bus: Arc<EventBus>) {
        self.graph.with_event_bus(Arc::clone(&bus));
        self.event_bus = Some(bus);
    }

    fn init(
        id: SessionId,
        parent_id: Option<SessionId>,
        session_type: SessionType,
        config: SessionConfig,
    ) -> Self {
        let now = Utc::now();
        let expires_at = config
            .ttl_secs
            .map(|ttl| now + chrono::Duration::seconds(ttl as i64));

        Self {
            id,
            parent_id,
            session_type,
            tenant_id: None,
            principal_id: None,
            state: SessionState::Created,
            authorization: config.authorization.clone(),
            config,
            messages: Vec::with_capacity(32),
            current_leaf_id: None,
            summary: None,
            total_usage: TokenUsage::default(),
            current_input_tokens: 0,
            total_cost_usd: Decimal::ZERO,
            static_context_hash: None,
            graph: {
                let mut graph = SessionGraph::new("main");
                graph.id = id.0;
                graph.created_at = now;
                graph
            },
            created_at: now,
            updated_at: now,
            expires_at,
            error: None,
            todos: Vec::with_capacity(8),
            current_plan: None,
            compact_history: VecDeque::new(),
            event_bus: None,
            content_overrides: ContentOverrides::default(),
        }
    }

    pub fn is_subagent(&self) -> bool {
        matches!(self.session_type, SessionType::Subagent { .. })
    }

    pub fn is_running(&self) -> bool {
        self.state.is_running()
    }

    pub fn is_finalizing(&self) -> bool {
        self.state.is_finalizing()
    }

    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|expires| Utc::now() > expires)
    }

    pub fn add_message(&mut self, mut message: SessionMessage) -> SessionResult<()> {
        if let Some(leaf) = &self.current_leaf_id {
            message.parent_id = Some(leaf.clone());
        }
        if let Some(usage) = &message.usage {
            self.total_usage.add(usage);
        }
        self.record_message_in_graph(&message)?;
        if message.is_compact_summary {
            self.refresh_summary_cache();
        }
        // Derive current_leaf_id and messages projection from graph.
        self.refresh_message_projection();
        self.updated_at = Utc::now();

        if let Some(ref bus) = self.event_bus {
            bus.emit_simple(
                crate::events::EventKind::SessionChanged,
                serde_json::json!({
                    "session_id": self.id.to_string(),
                    "message_count": self.messages.len(),
                }),
            );
        }

        Ok(())
    }

    pub fn to_graph(&self) -> crate::graph::SessionGraph {
        self.graph.clone()
    }

    pub fn current_branch_graph_nodes(&self) -> Vec<&crate::graph::GraphNode> {
        self.graph.current_branch_nodes(self.graph.primary_branch)
    }

    fn graph_projected_messages(&self) -> Vec<SessionMessage> {
        let branch_nodes = self.current_branch_graph_nodes();
        let start_index = branch_nodes
            .iter()
            .rposition(|node| node.kind == NodeKind::Summary)
            .unwrap_or(0);

        branch_nodes
            .into_iter()
            .skip(start_index)
            .filter_map(Self::graph_node_to_session_message)
            .collect()
    }

    pub fn current_branch_messages(&self) -> Vec<SessionMessage> {
        self.graph_projected_messages()
    }

    pub fn export_current_branch(
        &self,
    ) -> crate::session::SessionResult<crate::graph::BranchExport> {
        crate::session::SessionExporter::export_branch(&self.graph, self.graph.primary_branch)
    }

    pub fn set_identity(&mut self, tenant_id: Option<String>, principal_id: Option<String>) {
        self.tenant_id = tenant_id;
        self.principal_id = principal_id;
        self.updated_at = Utc::now();
    }

    pub fn bookmark_current_head(
        &mut self,
        label: impl Into<String>,
        note: Option<String>,
    ) -> Option<uuid::Uuid> {
        let head = self.graph.branch_head(self.graph.primary_branch)?;
        let bookmark = self
            .graph
            .create_bookmark(
                head,
                label,
                note,
                self.principal_id.clone(),
                self.graph_provenance(),
            )
            .ok()?;
        self.updated_at = Utc::now();
        Some(bookmark)
    }

    pub fn checkpoint_current_head(
        &mut self,
        label: impl Into<String>,
        note: Option<String>,
        tags: Vec<String>,
    ) -> SessionResult<uuid::Uuid> {
        let checkpoint = self
            .graph
            .create_checkpoint(
                self.graph.primary_branch,
                label,
                note,
                tags,
                self.principal_id.clone(),
                self.graph_provenance(),
            )
            .map_err(|e| SessionError::Storage {
                message: format!("failed to create checkpoint on primary branch: {}", e),
            })?;
        self.current_leaf_id = Some(MessageId::from_string(checkpoint.to_string()));
        self.updated_at = Utc::now();
        Ok(checkpoint)
    }

    pub fn replay_input(
        &self,
        from_node: Option<crate::graph::NodeId>,
    ) -> SessionResult<crate::graph::ReplayInput> {
        crate::session::ReplayService::replay_input(&self.graph, from_node)
    }

    fn record_message_in_graph(&mut self, message: &SessionMessage) -> SessionResult<()> {
        let branch_id = self.graph.primary_branch;
        let node_id = parse_message_node_id(&message.id, "message.id")?;
        let parent_id = message
            .parent_id
            .as_ref()
            .map(|parent| parse_message_node_id(parent, "message.parent_id"))
            .transpose()?;
        self.graph
            .append_existing_node(
                branch_id,
                node_id,
                parent_id,
                graph_node_kind_for_message(message),
                graph_tags_for_message(message),
                graph_payload_for_message(message),
                message.timestamp,
                self.principal_id.clone(),
                self.graph_provenance(),
            )
            .map_err(|error| SessionError::Storage {
                message: format!(
                    "Failed to append message {} to session graph: {}",
                    message.id, error
                ),
            })?;
        Ok(())
    }

    fn graph_provenance(&self) -> Option<NodeProvenance> {
        build_graph_provenance(self.id, &self.session_type)
    }

    fn graph_node_to_session_message(node: &GraphNode) -> Option<SessionMessage> {
        let role = match node.kind {
            NodeKind::User => Role::User,
            NodeKind::Assistant | NodeKind::Summary => Role::Assistant,
            _ => return None,
        };
        let content: Vec<ContentPart> =
            serde_json::from_value(node.payload.get("content")?.clone()).ok()?;
        let mut message = match role {
            Role::User | Role::Tool => SessionMessage::user(content),
            Role::Assistant => SessionMessage::assistant(content),
        };
        message.id = MessageId::from_string(node.id.to_string());
        message.parent_id = node
            .parent_id
            .map(|id| MessageId::from_string(id.to_string()));
        message.timestamp = node.created_at;
        message.is_sidechain = node.tags.iter().any(|tag| tag == "sidechain");
        message.is_compact_summary =
            node.kind == NodeKind::Summary || node.tags.iter().any(|tag| tag == "compact_summary");
        if let Some(usage) = node.payload.get("usage").cloned() {
            message.usage = serde_json::from_value(usage).ok();
        }
        if let Some(metadata) = node.payload.get("metadata").cloned() {
            message.metadata = serde_json::from_value(metadata).unwrap_or_default();
        }
        if let Some(environment) = node.payload.get("environment").cloned() {
            message.environment = serde_json::from_value(environment).ok();
        }
        Some(message)
    }

    fn graph_summary(&self) -> Option<String> {
        self.graph.latest_summary()
    }

    pub fn refresh_summary_cache(&mut self) {
        self.summary = self.graph_summary();
        self.updated_at = Utc::now();
    }

    /// Convert session messages to API format with default caching (5m TTL).
    ///
    /// Cache hints are now applied at the codec/transport layer via
    /// `ProviderOptions::anthropic.cache_control`, not on individual
    /// content parts. The `ttl` parameter is retained for backward
    /// compatibility but is currently unused.
    pub fn to_api_messages(&self) -> Vec<Message> {
        self.to_api_messages_with_cache(Some(CacheTtl::FiveMinutes))
    }

    /// Convert session messages to API format.
    ///
    /// The `ttl` parameter is retained for signature compatibility but
    /// cache breakpoints are now handled by the codec layer.
    pub fn to_api_messages_with_cache(&self, _ttl: Option<CacheTtl>) -> Vec<Message> {
        let branch_messages = self.current_branch_messages();
        if branch_messages.is_empty() {
            return Vec::new();
        }

        if self.content_overrides.is_empty() {
            branch_messages
                .iter()
                .map(SessionMessage::to_api_message)
                .collect()
        } else {
            branch_messages
                .iter()
                .map(|sm| {
                    if let Ok(node_id) = sm.id.0.parse::<uuid::Uuid>()
                        && let Some(replacement) = self.content_overrides.get(&node_id)
                    {
                        return Message {
                            role: sm.role,
                            content: replacement.clone(),
                        };
                    }
                    sm.to_api_message()
                })
                .collect()
        }
    }

    pub fn set_state(&mut self, state: SessionState) {
        self.state = state;
        self.updated_at = Utc::now();
    }

    pub fn set_todos(&mut self, todos: Vec<TodoItem>) {
        self.todos = todos;
        self.updated_at = Utc::now();
    }

    pub fn todos_in_progress_count(&self) -> usize {
        self.todos
            .iter()
            .filter(|t| t.status == TodoStatus::InProgress)
            .count()
    }

    pub fn enter_plan_mode(&mut self, name: Option<String>) -> &Plan {
        let mut plan = Plan::new(self.id);
        if let Some(n) = name {
            plan = plan.name(n);
        }
        self.updated_at = Utc::now();
        self.current_plan.insert(plan)
    }

    pub fn update_plan_content(&mut self, content: String) {
        if let Some(ref mut plan) = self.current_plan {
            plan.content = content;
            self.updated_at = Utc::now();
        }
    }

    pub fn exit_plan_mode(&mut self) -> Option<Plan> {
        if let Some(ref mut plan) = self.current_plan {
            plan.approve();
            self.updated_at = Utc::now();
        }
        self.current_plan.take()
    }

    pub fn cancel_plan(&mut self) -> Option<Plan> {
        if let Some(ref mut plan) = self.current_plan {
            plan.cancel();
            self.updated_at = Utc::now();
        }
        self.current_plan.take()
    }

    pub fn is_in_plan_mode(&self) -> bool {
        self.current_plan
            .as_ref()
            .is_some_and(|p| !p.status.is_terminal())
    }

    pub fn record_compact(&mut self, record: CompactRecord) {
        if self.compact_history.len() >= MAX_COMPACT_HISTORY_SIZE {
            self.compact_history.pop_front();
        }
        self.compact_history.push_back(record);
        self.updated_at = Utc::now();
    }

    pub fn update_summary(&mut self, summary: impl Into<String>) -> SessionResult<()> {
        let summary = summary.into();
        self.graph.append_node_with_actor(
            self.graph.primary_branch,
            NodeKind::Summary,
            serde_json::json!({
                "content": [ContentPart::text(format!("[Previous conversation summary]\n\n{}", summary))],
                "summary": summary,
            }),
            self.principal_id.clone(),
            self.graph_provenance(),
        ).map_err(|e| SessionError::Storage {
            message: format!("failed to append summary to primary branch: {}", e),
        })?;
        self.refresh_summary_cache();
        self.refresh_message_projection();
        self.updated_at = Utc::now();
        Ok(())
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) -> SessionResult<()> {
        let msg = SessionMessage::user(vec![ContentPart::text(content.into())]);
        self.add_message(msg)
    }

    pub fn add_assistant_message(
        &mut self,
        content: Vec<ContentPart>,
        usage: Option<Usage>,
    ) -> SessionResult<()> {
        self.add_assistant_message_with_metadata(content, usage, MessageMetadata::default())
    }

    pub fn add_assistant_message_with_metadata(
        &mut self,
        content: Vec<ContentPart>,
        usage: Option<Usage>,
        metadata: MessageMetadata,
    ) -> SessionResult<()> {
        let mut msg = SessionMessage::assistant(content);
        msg.metadata = metadata;
        if let Some(u) = usage {
            self.current_input_tokens = u.context_usage() as u64;
            msg = msg.usage(TokenUsage {
                input_tokens: u.input_tokens as u64,
                output_tokens: u.output_tokens as u64,
                cache_read_input_tokens: u.cache_read_input_tokens.unwrap_or(0) as u64,
                cache_creation_input_tokens: u.cache_creation_input_tokens.unwrap_or(0) as u64,
                ..Default::default()
            });
        }
        self.add_message(msg)
    }

    pub fn add_tool_results(
        &mut self,
        results: Vec<crate::types::ToolResultBlock>,
    ) -> SessionResult<()> {
        let content: Vec<ContentPart> = results
            .into_iter()
            .map(|tr| ContentPart::ToolResult {
                tool_call_id: tr.tool_use_id,
                content: match tr.content {
                    Some(crate::types::ToolResultContent::Text(s)) => {
                        crate::ir::ToolResultContent::Text(s)
                    }
                    Some(crate::types::ToolResultContent::Blocks(blocks)) => {
                        let text = blocks
                            .iter()
                            .filter_map(|b| match b {
                                crate::types::ToolResultContentBlock::Text { text } => {
                                    Some(text.as_str())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        crate::ir::ToolResultContent::Text(text)
                    }
                    None => crate::ir::ToolResultContent::Text(String::new()),
                },
                is_error: tr.is_error.unwrap_or(false),
            })
            .collect();
        let msg = SessionMessage::user(content);
        self.add_message(msg)
    }

    pub fn update_latest_assistant_metadata(&mut self, metadata: MessageMetadata) -> bool {
        let Some(node_id) = self
            .current_branch_graph_nodes()
            .into_iter()
            .rev()
            .find(|node| matches!(node.kind, NodeKind::Assistant | NodeKind::Summary))
            .map(|node| node.id)
        else {
            return false;
        };

        let metadata_value = serde_json::to_value(&metadata).unwrap_or_default();
        if !self
            .graph
            .patch_node_metadata(node_id, metadata_value, self.principal_id.clone())
        {
            return false;
        }

        self.refresh_message_projection();
        self.updated_at = Utc::now();
        true
    }

    pub fn should_compact(&self, max_tokens: u64, threshold: f32) -> bool {
        !self.current_branch_messages().is_empty()
            && self.current_input_tokens as f64 > max_tokens as f64 * threshold as f64
    }

    pub fn update_usage(&mut self, usage: &Usage) {
        self.current_input_tokens = usage.context_usage() as u64;
        self.total_usage.add_usage(usage);
    }

    pub async fn compact(
        &mut self,
        client: &crate::Client,
    ) -> crate::Result<crate::types::CompactResult> {
        let executor = crate::session::compact::CompactService::new(
            crate::session::compact::CompactConfig::default(),
        );
        let result = executor.execute(self, client).await?;
        if matches!(result, crate::types::CompactResult::Compacted { .. }) {
            self.current_input_tokens = 0;
        }
        Ok(result)
    }

    pub fn clear_messages(&mut self) {
        self.messages.clear();
        self.current_leaf_id = None;
        self.content_overrides.clear();
        self.updated_at = Utc::now();
    }

    pub fn refresh_message_projection(&mut self) {
        self.messages = self.current_branch_messages();
        self.current_leaf_id = self
            .graph
            .branch_head(self.graph.primary_branch)
            .map(|node_id| MessageId::from_string(node_id.to_string()));
    }

    /// Fork this session at a specific graph node (or current head).
    ///
    /// Creates a new `Session` with an independent branch in the graph,
    /// sharing the full history up to the fork point. The forked session
    /// gets a new ID and can be executed independently.
    ///
    /// Use cases: A/B testing, checkpoint recovery, async analysis branches.
    pub fn fork_at(
        &self,
        node_id: Option<uuid::Uuid>,
        branch_name: Option<String>,
    ) -> SessionResult<Session> {
        let mut forked = self.clone();
        forked.id = SessionId::new();
        forked.parent_id = Some(self.id);
        forked.state = SessionState::Created;
        forked.error = None;
        forked.content_overrides = ContentOverrides::new();
        forked.created_at = Utc::now();
        forked.updated_at = Utc::now();

        let name = branch_name.unwrap_or_else(|| format!("fork-{}", &forked.id.to_string()[..8]));
        forked
            .graph
            .fork_branch(node_id, name)
            .map_err(|e| SessionError::Storage {
                message: format!("Failed to fork graph branch: {e}"),
            })?;

        forked.refresh_message_projection();

        Ok(forked)
    }
}

pub(crate) fn graph_node_kind_for_message(message: &SessionMessage) -> NodeKind {
    if message.is_compact_summary {
        NodeKind::Summary
    } else {
        match message.role {
            Role::User | Role::Tool => NodeKind::User,
            Role::Assistant => NodeKind::Assistant,
        }
    }
}

pub(crate) fn graph_tags_for_message(message: &SessionMessage) -> Vec<String> {
    let mut tags = Vec::new();
    if message.is_sidechain {
        tags.push("sidechain".to_string());
    }
    if message.is_compact_summary {
        tags.push("compact_summary".to_string());
    }
    tags
}

pub(crate) fn graph_payload_for_message(message: &SessionMessage) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "role": message.role,
        "content": message.content,
        "usage": message.usage,
        "metadata": message.metadata,
        "environment": message.environment,
    });
    if message.is_compact_summary
        && let Some(summary) = compact_summary_text(message)
    {
        payload["summary"] = serde_json::Value::String(summary);
    }
    payload
}

pub(crate) fn compact_summary_text(message: &SessionMessage) -> Option<String> {
    let text = message
        .content
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return None;
    }
    Some(
        text.strip_prefix("[Previous conversation summary]\n\n")
            .unwrap_or(&text)
            .to_string(),
    )
}

#[cfg(any(feature = "jsonl", feature = "postgres"))]
pub(crate) fn graph_node_id_for_message(message: &SessionMessage) -> SessionResult<uuid::Uuid> {
    parse_message_node_id(&message.id, "message.id")
}

#[cfg(any(feature = "jsonl", feature = "postgres"))]
pub(crate) fn graph_parent_node_id_for_message(
    message: &SessionMessage,
) -> SessionResult<Option<uuid::Uuid>> {
    message
        .parent_id
        .as_ref()
        .map(|parent| parse_message_node_id(parent, "message.parent_id"))
        .transpose()
}

pub(crate) fn build_graph_provenance(
    session_id: SessionId,
    session_type: &SessionType,
) -> Option<NodeProvenance> {
    let session_type_label = match session_type {
        SessionType::Main => "main".to_string(),
        SessionType::Subagent { .. } => "subagent".to_string(),
    };
    let (subagent_type, subagent_description) = match session_type {
        SessionType::Subagent {
            agent_type,
            description,
        } => (Some(agent_type.clone()), Some(description.clone())),
        SessionType::Main => (None, None),
    };
    Some(NodeProvenance {
        source_session_id: session_id.to_string(),
        session_type: session_type_label,
        task_id: matches!(session_type, SessionType::Subagent { .. })
            .then(|| session_id.to_string()),
        subagent_session_id: matches!(session_type, SessionType::Subagent { .. })
            .then(|| session_id.to_string()),
        subagent_type,
        subagent_description,
    })
}

fn parse_message_node_id(message_id: &MessageId, field: &str) -> SessionResult<uuid::Uuid> {
    uuid::Uuid::parse_str(&message_id.0).map_err(|error| SessionError::Storage {
        message: format!(
            "Session message {} '{}' is not a valid UUID: {}",
            field, message_id, error
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{ContentPart, Role};

    #[test]
    fn test_session_creation() {
        let config = SessionConfig::default();
        let session = Session::new(config);

        assert_eq!(session.state, SessionState::Created);
        assert!(session.current_branch_messages().is_empty());
        assert!(session.current_leaf_id.is_none());
    }

    #[test]
    fn test_add_message() {
        let mut session = Session::new(SessionConfig::default());

        let msg1 = SessionMessage::user(vec![ContentPart::text("Hello")]);
        session.add_message(msg1).unwrap();

        assert_eq!(session.current_branch_messages().len(), 1);
        assert!(session.current_leaf_id.is_some());
        assert_eq!(session.current_branch_graph_nodes().len(), 1);
    }

    #[test]
    fn test_add_message_rejects_invalid_message_uuid() {
        let mut session = Session::new(SessionConfig::default());
        let mut message = SessionMessage::user(vec![ContentPart::text("Hello")]);
        message.id = MessageId::from_string("not-a-uuid");

        let error = session.add_message(message).unwrap_err();
        assert!(matches!(error, SessionError::Storage { .. }));
    }

    #[test]
    fn test_graph_tracks_message_lineage() {
        let mut session = Session::new(SessionConfig::default());

        session
            .add_message(SessionMessage::user(vec![ContentPart::text("Hello")]))
            .unwrap();
        session
            .add_message(SessionMessage::assistant(vec![ContentPart::text(
                "Hi there!",
            )]))
            .unwrap();

        let branch = session.current_branch_graph_nodes();
        assert_eq!(branch.len(), 2);
        assert_eq!(branch[0].kind, crate::graph::NodeKind::User);
        assert_eq!(branch[1].kind, crate::graph::NodeKind::Assistant);
        assert_eq!(
            session.graph.branch_head(session.graph.primary_branch),
            Some(branch[1].id)
        );
    }

    #[test]
    fn test_refresh_message_projection_from_graph() {
        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(SessionMessage::user(vec![ContentPart::text("Hello")]))
            .unwrap();
        session.clear_messages();

        session.refresh_message_projection();

        assert_eq!(session.current_branch_messages().len(), 1);
        assert!(session.current_leaf_id.is_some());
    }

    #[test]
    fn test_refresh_message_projection_preserves_updated_at() {
        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(SessionMessage::user(vec![ContentPart::text("Hello")]))
            .unwrap();
        let updated_at = session.updated_at;
        session.clear_messages();
        session.updated_at = updated_at;

        session.refresh_message_projection();

        assert_eq!(session.updated_at, updated_at);
    }

    #[test]
    fn test_compact_summary_message_round_trips_as_summary_node() {
        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(
                SessionMessage::assistant(vec![ContentPart::text(
                    "[Previous conversation summary]\n\nSummary body",
                )])
                .as_compact_summary(),
            )
            .unwrap();

        let branch = session.current_branch_graph_nodes();
        assert_eq!(branch.len(), 1);
        assert_eq!(branch[0].kind, crate::graph::NodeKind::Summary);
        assert_eq!(session.summary.as_deref(), Some("Summary body"));
    }

    #[test]
    fn test_message_tree() {
        let mut session = Session::new(SessionConfig::default());

        let user_msg = SessionMessage::user(vec![ContentPart::text("Hello")]);
        session.add_message(user_msg).unwrap();

        let assistant_msg = SessionMessage::assistant(vec![ContentPart::text("Hi there!")]);
        session.add_message(assistant_msg).unwrap();

        let branch = session.current_branch_messages();
        assert_eq!(branch.len(), 2);
        assert_eq!(branch[0].role, Role::User);
        assert_eq!(branch[1].role, Role::Assistant);
    }

    #[test]
    fn test_session_expiry() {
        let config = SessionConfig {
            ttl_secs: Some(0),
            ..Default::default()
        };
        let session = Session::new(config);

        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(session.is_expired());
    }

    #[test]
    fn test_token_usage_accumulation() {
        let mut session = Session::new(SessionConfig::default());

        let msg1 =
            SessionMessage::assistant(vec![ContentPart::text("Response 1")]).usage(TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            });
        session.add_message(msg1).unwrap();

        let msg2 =
            SessionMessage::assistant(vec![ContentPart::text("Response 2")]).usage(TokenUsage {
                input_tokens: 150,
                output_tokens: 75,
                ..Default::default()
            });
        session.add_message(msg2).unwrap();

        assert_eq!(session.total_usage.input_tokens, 250);
        assert_eq!(session.total_usage.output_tokens, 125);
    }

    #[test]
    fn test_compact_history_limit() {
        let mut session = Session::new(SessionConfig::default());

        for i in 0..MAX_COMPACT_HISTORY_SIZE + 10 {
            let record = CompactRecord::new(session.id).summary(format!("Summary {}", i));
            session.record_compact(record);
        }

        assert_eq!(session.compact_history.len(), MAX_COMPACT_HISTORY_SIZE);
        assert!(session.compact_history[0].summary.contains("10"));
    }

    #[test]
    fn test_exit_plan_mode_takes_ownership() {
        let mut session = Session::new(SessionConfig::default());
        session.enter_plan_mode(Some("Test Plan".to_string()));

        let plan = session.exit_plan_mode();
        assert!(plan.is_some());
        assert!(session.current_plan.is_none());
    }

    #[test]
    fn test_to_api_messages_returns_conversation() {
        let mut session = Session::new(SessionConfig::default());

        session.add_user_message("First question").unwrap();
        session
            .add_message(SessionMessage::assistant(vec![ContentPart::text(
                "First answer",
            )]))
            .unwrap();
        session.add_user_message("Second question").unwrap();

        let messages = session.to_api_messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].text(), "First question");
        assert_eq!(messages[1].text(), "First answer");
        assert_eq!(messages[2].text(), "Second question");
    }

    #[test]
    fn test_to_api_messages_empty_session() {
        let session = Session::new(SessionConfig::default());
        let messages = session.to_api_messages();
        assert!(messages.is_empty());
    }

    #[test]
    fn test_to_api_messages_assistant_only() {
        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(SessionMessage::assistant(vec![ContentPart::text("Hi")]))
            .unwrap();

        let messages = session.to_api_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text(), "Hi");
    }
}
