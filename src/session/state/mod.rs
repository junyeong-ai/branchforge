//! Session state management.

#![allow(missing_docs)]

mod config;
mod enums;
mod ids;
mod message;
mod policy;

pub use config::SessionConfig;
pub use enums::{SessionState, SessionTransitionError, SessionType};
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

/// Compute the total context window tokens consumed by a usage record.
///
/// Counts every token that occupies the model's context window:
/// fresh input plus cache reads plus cache writes. Used for compaction
/// triggering — when this value exceeds the configured threshold, the
/// session is compacted.
///
/// Note: this differs from `ir::Usage::billable_input_tokens()` which
/// returns only the fresh input portion (used for cost calculation).
#[inline]
fn context_window_usage(usage: &crate::ir::Usage) -> u64 {
    usage.input_tokens
        + usage.cached_input_tokens.unwrap_or(0)
        + usage.cache_creation_tokens.unwrap_or(0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub parent_id: Option<SessionId>,
    pub session_type: SessionType,
    /// Tenant identifier — set via [`Self::set_identity`] only.
    pub(crate) tenant_id: Option<String>,
    /// Principal (user/service) identifier — set via [`Self::set_identity`] only.
    pub(crate) principal_id: Option<String>,
    /// Lifecycle state. Mutated only via [`Self::transition`] or
    /// [`Self::finalize`] so the FSM invariant is enforced at every
    /// mutation point. Read access stays public.
    pub(crate) state: SessionState,
    pub config: SessionConfig,
    pub authorization: SessionAuthorization,
    /// Latest cached compaction summary. Refreshed by
    /// [`Self::refresh_summary_cache`] / [`Self::update_summary`].
    pub(crate) summary: Option<String>,
    /// Aggregate token usage. Mutated via [`Self::update_usage`] which
    /// also keeps `current_input_tokens` in sync.
    pub(crate) total_usage: crate::ir::Usage,
    #[serde(default)]
    pub(crate) current_input_tokens: u64,
    /// Accumulated cost. Updated by the budget tracker via crate-internal
    /// session manipulation; external readers should use
    /// [`Self::total_cost_usd`].
    pub(crate) total_cost_usd: Decimal,
    /// Hash of the static context blob used for cache identity. Set by
    /// the agent builder during session creation.
    pub(crate) static_context_hash: Option<String>,
    /// The canonical session graph — the single source of truth for
    /// messages, branches, checkpoints, bookmarks, and provenance. Marked
    /// `pub(crate)` because external mutation would silently break the
    /// SSoT invariant; use [`Self::graph`] for read access and the
    /// `add_message`/`fork_at`/`bookmark_*`/`checkpoint_*` methods for
    /// the mutation entry points.
    #[serde(default)]
    pub(crate) graph: SessionGraph,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    /// TTL expiry timestamp. Read-only after init.
    pub(crate) expires_at: Option<DateTime<Utc>>,
    /// Latest error string when the session is in a Failed-like state.
    /// Mutated by the task registry / executor; external readers should
    /// use [`Self::error`].
    pub(crate) error: Option<String>,
    /// Todo list, mutated via [`Self::set_todos`].
    #[serde(default)]
    pub(crate) todos: Vec<TodoItem>,
    /// Active plan, mutated via the `enter_plan_mode`/`exit_plan_mode`
    /// methods.
    #[serde(default)]
    pub(crate) current_plan: Option<Plan>,
    /// Compaction history, appended via [`Self::record_compact`].
    #[serde(default)]
    pub(crate) compact_history: VecDeque<CompactRecord>,
    #[serde(skip)]
    pub(crate) event_bus: Option<Arc<EventBus>>,
    /// Transient micro-compaction overrides applied at projection time.
    /// `pub(crate)` because direct mutation would bypass the compaction
    /// strategy contract; the compact module owns this state.
    #[serde(skip)]
    pub(crate) content_overrides: ContentOverrides,
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
            summary: None,
            total_usage: crate::ir::Usage::default(),
            current_input_tokens: 0,
            total_cost_usd: Decimal::ZERO,
            static_context_hash: None,
            graph: {
                let mut graph = SessionGraph::new("main");
                graph.id = crate::graph::SessionGraphId::from_uuid(id.as_uuid());
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
        if let Some(leaf) = self.current_leaf_id() {
            message.parent_id = Some(leaf);
        }
        if let Some(usage) = &message.usage {
            self.total_usage.add(usage);
        }
        self.record_message_in_graph(&message)?;
        if message.is_compact_summary {
            self.refresh_summary_cache();
        }
        self.updated_at = Utc::now();

        if let Some(ref bus) = self.event_bus {
            bus.emit_simple(
                crate::events::EventKind::SessionChanged,
                serde_json::json!({
                    "session_id": self.id.to_string(),
                    "message_count": self.current_branch_messages().len(),
                }),
            );
        }

        Ok(())
    }

    /// Returns a reference to the underlying [`SessionGraph`].
    ///
    /// The graph is the canonical source of truth — every projection
    /// (`messages`, `current_leaf_id`, summary, etc.) is derived from it.
    pub fn graph(&self) -> &crate::graph::SessionGraph {
        &self.graph
    }

    /// Returns the current lifecycle state.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Returns the cached summary, if any.
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// Returns the aggregate token usage for this session.
    pub fn total_usage(&self) -> &crate::ir::Usage {
        &self.total_usage
    }

    /// Returns the most recently observed input-token count for this
    /// session — used by the compaction trigger.
    pub fn current_input_tokens(&self) -> u64 {
        self.current_input_tokens
    }

    /// Returns the latest error message, if the session is in an error state.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Returns the accumulated cost in USD for this session.
    pub fn total_cost_usd(&self) -> rust_decimal::Decimal {
        self.total_cost_usd
    }

    /// Returns the static-context hash if one was registered.
    pub fn static_context_hash(&self) -> Option<&str> {
        self.static_context_hash.as_deref()
    }

    /// Returns the expiry timestamp if the session has a TTL.
    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.expires_at
    }

    /// Returns the todo list for this session.
    pub fn todos(&self) -> &[TodoItem] {
        &self.todos
    }

    /// Returns the active plan, if plan mode is engaged.
    pub fn current_plan(&self) -> Option<&Plan> {
        self.current_plan.as_ref()
    }

    /// Returns the compaction history (most recent at the back).
    pub fn compact_history(&self) -> &VecDeque<CompactRecord> {
        &self.compact_history
    }

    /// Returns the session creation timestamp (read-only after init).
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Returns the timestamp of the most recent mutation.
    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    /// Returns the transient micro-compaction overrides table. These are
    /// applied at projection time and are not persisted across reload.
    pub fn content_overrides(&self) -> &ContentOverrides {
        &self.content_overrides
    }

    /// Apply a transient content override for a graph node. Used by the
    /// micro-compaction strategy and by debugging tooling that needs to
    /// inject a smaller projection without rewriting the graph.
    pub fn set_content_override(
        &mut self,
        node_id: crate::graph::NodeId,
        content: Vec<ContentPart>,
    ) {
        self.content_overrides.set(node_id, content);
        self.updated_at = Utc::now();
    }

    /// Drop all transient content overrides — the next call to
    /// [`Self::to_api_messages`] will return the unredacted projection.
    pub fn clear_content_overrides(&mut self) {
        self.content_overrides.clear();
        self.updated_at = Utc::now();
    }

    /// Returns the tenant identifier, if one was set via [`Self::set_identity`].
    pub fn tenant_id(&self) -> Option<&str> {
        self.tenant_id.as_deref()
    }

    /// Returns the principal identifier, if one was set via [`Self::set_identity`].
    pub fn principal_id(&self) -> Option<&str> {
        self.principal_id.as_deref()
    }

    /// Returns the current leaf message id for the primary branch.
    ///
    /// Computed lazily from `self.graph.branch_head(primary_branch)`. There
    /// is no cached field — the graph is the only source of truth.
    pub fn current_leaf_id(&self) -> Option<MessageId> {
        self.graph
            .branch_head(self.graph.primary_branch)
            .map(|node_id| MessageId::from_string(node_id.to_string()))
    }

    pub fn current_branch_graph_nodes(&self) -> Vec<&crate::graph::GraphNode> {
        self.graph.current_branch_nodes(self.graph.primary_branch)
    }

    fn graph_projected_messages(&self) -> Vec<SessionMessage> {
        let branch_nodes = self.current_branch_graph_nodes();

        // Filter out archived nodes: if a watermark is set, skip any
        // primary-branch node whose created_at precedes the watermark node.
        let branch_nodes: Vec<_> = if let Some(watermark_id) = self.graph.archived_watermark() {
            if let Some(watermark_ts) = self.graph.nodes().get(&watermark_id).map(|n| n.created_at)
            {
                branch_nodes
                    .into_iter()
                    .filter(|n| n.created_at >= watermark_ts)
                    .collect()
            } else {
                branch_nodes
            }
        } else {
            branch_nodes
        };

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
    ) -> Option<crate::graph::BookmarkId> {
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
        // The graph head now reflects the new checkpoint; current_leaf_id()
        // is derived from it on demand.
        self.updated_at = Utc::now();
        Ok(checkpoint.into_inner())
    }

    pub fn replay_input(
        &self,
        from_node: Option<crate::graph::NodeId>,
    ) -> SessionResult<crate::graph::ReplayInput> {
        crate::session::Replayer::replay_input(&self.graph, from_node)
    }

    fn record_message_in_graph(&mut self, message: &SessionMessage) -> SessionResult<()> {
        let branch_id = self.graph.primary_branch;
        let node_id =
            crate::graph::NodeId::from_uuid(parse_message_node_id(&message.id, "message.id")?);
        let parent_id = message
            .parent_id
            .as_ref()
            .map(|parent| {
                parse_message_node_id(parent, "message.parent_id")
                    .map(crate::graph::NodeId::from_uuid)
            })
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

    /// Convert session messages to API format.
    ///
    /// Cache hints are applied at the codec/transport layer via
    /// `ProviderOptions::anthropic.cache_control`, not on individual
    /// content parts.
    pub fn to_api_messages(&self) -> Vec<Message> {
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
                    if let Ok(node_id) = sm
                        .id
                        .as_str()
                        .parse::<uuid::Uuid>()
                        .map(crate::graph::NodeId::from_uuid)
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

    /// Validated lifecycle transition. Returns [`SessionTransitionError`]
    /// if the move is illegal per the [`SessionState`] FSM.
    ///
    /// This is the only mutation entry point for `Session::state` —
    /// direct field assignment is not permitted outside this module
    /// (except for the fork reset, which constructs a fresh session).
    pub fn transition(&mut self, next: SessionState) -> Result<(), SessionTransitionError> {
        self.state = self.state.transition_to(next)?;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Reset a terminal session to [`SessionState::Created`] so it can
    /// be re-used by the task tracker's resume path.
    ///
    /// This is the single documented escape hatch from the forward-only
    /// FSM. It is valid **only** when the current state is terminal —
    /// resuming an in-flight session is rejected.
    ///
    /// The session's `error` field is cleared; graph, identity, usage,
    /// and history are preserved so a resume sees the prior context.
    pub fn reset_for_resume(&mut self) -> Result<(), SessionTransitionError> {
        self.state = self.state.try_reset()?;
        self.error = None;
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Drive the FSM to a terminal state, walking through any required
    /// intermediate phases (`Created → Running → <finalizing> → <terminal>`).
    ///
    /// This is the one-shot "finalize" entry point for external consumers
    /// that want to force an end state without caring about intermediate
    /// phases. Idempotent: returns `Ok(())` if `self.state == terminal`.
    /// Errors if `terminal` is not a terminal state or if the current
    /// state has already committed to a different finalizing lane.
    pub fn finalize(&mut self, terminal: SessionState) -> Result<(), SessionTransitionError> {
        if self.state == terminal {
            return Ok(());
        }
        let finalizing = terminal.finalizing_phase().ok_or(SessionTransitionError {
            from: self.state,
            to: terminal,
        })?;
        if self.state == SessionState::Created {
            self.transition(SessionState::Running)?;
        }
        if !self.state.is_finalizing() {
            self.transition(finalizing)?;
        }
        self.transition(terminal)?;
        Ok(())
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
            // Plan exits plan-mode by transitioning Draft → Approved.
            // A plan already past Draft (previously approved, for
            // example) is left at its current state.
            if plan.state() == crate::session::types::PlanState::Draft {
                let _ = plan.transition(crate::session::types::PlanState::Approved);
            }
            self.updated_at = Utc::now();
        }
        self.current_plan.take()
    }

    pub fn cancel_plan(&mut self) -> Option<Plan> {
        if let Some(ref mut plan) = self.current_plan {
            // Cancel is reachable from any non-terminal plan state.
            let _ = plan.transition(crate::session::types::PlanState::Cancelled);
            self.updated_at = Utc::now();
        }
        self.current_plan.take()
    }

    pub fn is_in_plan_mode(&self) -> bool {
        self.current_plan
            .as_ref()
            .is_some_and(|p| !p.state().is_terminal())
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
        usage: Option<crate::ir::Usage>,
    ) -> SessionResult<()> {
        self.add_assistant_message_with_metadata(content, usage, MessageMetadata::default())
    }

    pub fn add_assistant_message_with_metadata(
        &mut self,
        content: Vec<ContentPart>,
        usage: Option<crate::ir::Usage>,
        metadata: MessageMetadata,
    ) -> SessionResult<()> {
        let mut msg = SessionMessage::assistant(content);
        msg.metadata = metadata;
        if let Some(u) = usage {
            self.current_input_tokens = context_window_usage(&u);
            msg = msg.usage(u);
        }
        self.add_message(msg)
    }

    pub fn add_tool_results(&mut self, results: Vec<crate::ir::ContentPart>) -> SessionResult<()> {
        let msg = SessionMessage::user(results);
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

        self.updated_at = Utc::now();
        true
    }

    pub fn should_compact(&self, max_tokens: u64, threshold: f32) -> bool {
        !self.current_branch_messages().is_empty()
            && self.current_input_tokens as f64 > max_tokens as f64 * threshold as f64
    }

    pub fn update_usage(&mut self, usage: &crate::ir::Usage) {
        self.current_input_tokens = context_window_usage(usage);
        self.total_usage.add(usage);
    }

    pub async fn compact(
        &mut self,
        llm: &dyn crate::client::LlmCall,
    ) -> crate::Result<crate::session::compact::CompactResult> {
        let executor = crate::session::compact::Compactor::new(
            crate::session::compact::CompactConfig::default(),
        );
        let result = executor.execute(self, llm).await?;
        if matches!(
            result,
            crate::session::compact::CompactResult::Compacted { .. }
        ) {
            self.current_input_tokens = 0;
        }
        Ok(result)
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
        node_id: Option<crate::graph::NodeId>,
        branch_name: Option<String>,
    ) -> SessionResult<Session> {
        let mut forked = self.clone();
        forked.id = SessionId::new();
        forked.parent_id = Some(self.id);
        // fsm-init: forked session begins a fresh lifecycle by construction,
        // not by a forward transition — terminal → Created is not a legal move.
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
pub(crate) fn graph_node_id_for_message(
    message: &SessionMessage,
) -> SessionResult<crate::graph::NodeId> {
    parse_message_node_id(&message.id, "message.id").map(crate::graph::NodeId::from_uuid)
}

#[cfg(any(feature = "jsonl", feature = "postgres"))]
pub(crate) fn graph_parent_node_id_for_message(
    message: &SessionMessage,
) -> SessionResult<Option<crate::graph::NodeId>> {
    message
        .parent_id
        .as_ref()
        .map(|parent| {
            parse_message_node_id(parent, "message.parent_id").map(crate::graph::NodeId::from_uuid)
        })
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
    uuid::Uuid::parse_str(message_id.as_str()).map_err(|error| SessionError::Storage {
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
        assert!(session.current_leaf_id().is_none());
    }

    #[test]
    fn test_add_message() {
        let mut session = Session::new(SessionConfig::default());

        let msg1 = SessionMessage::user(vec![ContentPart::text("Hello")]);
        session.add_message(msg1).unwrap();

        assert_eq!(session.current_branch_messages().len(), 1);
        assert!(session.current_leaf_id().is_some());
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
    fn test_messages_lazy_projection_from_graph() {
        // After SSoT cleanup there is no messages cache to clear or refresh:
        // every call to current_branch_messages() rebuilds from the graph,
        // and current_leaf_id() reads the graph head directly.
        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(SessionMessage::user(vec![ContentPart::text("Hello")]))
            .unwrap();

        assert_eq!(session.current_branch_messages().len(), 1);
        assert!(session.current_leaf_id().is_some());
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

        let msg1 = SessionMessage::assistant(vec![ContentPart::text("Response 1")]).usage(
            crate::ir::Usage {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            },
        );
        session.add_message(msg1).unwrap();

        let msg2 = SessionMessage::assistant(vec![ContentPart::text("Response 2")]).usage(
            crate::ir::Usage {
                input_tokens: 150,
                output_tokens: 75,
                ..Default::default()
            },
        );
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
