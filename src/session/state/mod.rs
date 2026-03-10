//! Session state management.

mod config;
mod enums;
mod ids;
mod message;
mod policy;

pub use config::SessionConfig;
pub use enums::{SessionState, SessionType};
pub use ids::{MessageId, SessionId};
pub use message::{MessageMetadata, SessionMessage, ThinkingMetadata, ToolResultMeta};
pub use policy::{PermissionMode, SessionPermissions, SessionToolLimits};

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::graph::{GraphNode, NodeKind, SessionGraph};
use crate::session::types::{CompactRecord, Plan, TodoItem, TodoStatus};
use crate::types::{CacheControl, CacheTtl, ContentBlock, Message, Role, TokenUsage, Usage};

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
    pub permissions: SessionPermissions,
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
        let session_type = SessionType::Subagent {
            agent_type: agent_type.into(),
            description: description.into(),
        };
        Self::init(SessionId::new(), Some(parent_id), session_type, config)
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
            permissions: config.permissions.clone(),
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
        }
    }

    pub fn is_subagent(&self) -> bool {
        matches!(self.session_type, SessionType::Subagent { .. })
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.state,
            SessionState::Active | SessionState::WaitingForTools
        )
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            SessionState::Completed | SessionState::Failed | SessionState::Cancelled
        )
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at.is_some_and(|expires| Utc::now() > expires)
    }

    pub fn add_message(&mut self, mut message: SessionMessage) {
        if let Some(leaf) = &self.current_leaf_id {
            message.parent_id = Some(leaf.clone());
        }
        self.record_message_in_graph(&message);
        self.current_leaf_id = Some(message.id.clone());
        if let Some(usage) = &message.usage {
            self.total_usage.add(usage);
        }
        self.messages.push(message);
        self.updated_at = Utc::now();
    }

    pub fn to_graph(&self) -> crate::graph::SessionGraph {
        self.graph.clone()
    }

    pub fn current_branch_graph_nodes(&self) -> Vec<&crate::graph::GraphNode> {
        self.graph.current_branch_nodes(self.graph.primary_branch)
    }

    fn graph_projected_messages(&self) -> Vec<SessionMessage> {
        self.current_branch_graph_nodes()
            .into_iter()
            .filter_map(Self::graph_node_to_session_message)
            .collect()
    }

    pub fn current_branch_messages(&self) -> Vec<SessionMessage> {
        self.graph_projected_messages()
    }

    pub fn export_current_branch(&self) -> Option<crate::graph::BranchExport> {
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
        self.graph
            .create_bookmark(head, label, note, self.principal_id.clone())
    }

    pub fn replay_input(
        &self,
        from_node: Option<crate::graph::NodeId>,
    ) -> crate::graph::ReplayInput {
        crate::session::ReplayService::replay_input(&self.graph, from_node)
    }

    fn record_message_in_graph(&mut self, message: &SessionMessage) {
        let branch_id = self.graph.primary_branch;
        let node_id = uuid::Uuid::parse_str(&message.id.0).unwrap_or_else(|_| uuid::Uuid::new_v4());
        let parent_id = message
            .parent_id
            .as_ref()
            .and_then(|parent| uuid::Uuid::parse_str(&parent.0).ok());
        let kind = match message.role {
            Role::User => NodeKind::User,
            Role::Assistant => NodeKind::Assistant,
        };
        let mut tags = Vec::new();
        if message.is_sidechain {
            tags.push("sidechain".to_string());
        }
        if message.is_compact_summary {
            tags.push("compact_summary".to_string());
        }
        self.graph.append_existing_node(
            branch_id,
            node_id,
            parent_id,
            kind,
            tags,
            serde_json::json!({
                "role": message.role,
                "content": message.content,
                "usage": message.usage,
            }),
            message.timestamp,
            self.principal_id.clone(),
        );
    }

    fn graph_node_to_session_message(node: &GraphNode) -> Option<SessionMessage> {
        let role = match node.kind {
            NodeKind::User => Role::User,
            NodeKind::Assistant | NodeKind::Summary => Role::Assistant,
            _ => return None,
        };
        let content: Vec<ContentBlock> =
            serde_json::from_value(node.payload.get("content")?.clone()).ok()?;
        let mut message = match role {
            Role::User => SessionMessage::user(content),
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
        Some(message)
    }

    /// Convert session messages to API format with default caching (5m TTL).
    pub fn to_api_messages(&self) -> Vec<Message> {
        self.to_api_messages_with_cache(Some(CacheTtl::FiveMinutes))
    }

    /// Convert session messages to API format with optional caching.
    ///
    /// Per Anthropic best practices, caches the last user message with the specified TTL.
    /// Pass `None` to disable caching.
    pub fn to_api_messages_with_cache(&self, ttl: Option<CacheTtl>) -> Vec<Message> {
        let branch_messages = self.current_branch_messages();
        if branch_messages.is_empty() {
            return Vec::new();
        }

        let mut messages: Vec<Message> = branch_messages
            .iter()
            .map(SessionMessage::to_api_message)
            .collect();

        if let Some(ttl) = ttl {
            self.apply_cache_breakpoint(&mut messages, ttl);
        }

        messages
    }

    /// Apply cache breakpoint to the last user message.
    ///
    /// Per Anthropic best practices for multi-turn conversations,
    /// only the last user message needs cache_control to enable
    /// caching of the entire conversation history before it.
    fn apply_cache_breakpoint(&self, messages: &mut [Message], ttl: CacheTtl) {
        let last_user_idx = messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.role == Role::User)
            .map(|(i, _)| i);

        if let Some(idx) = last_user_idx {
            messages[idx].set_cache_on_last_block(CacheControl::ephemeral().ttl(ttl));
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

    pub fn update_summary(&mut self, summary: impl Into<String>) {
        self.summary = Some(summary.into());
        self.updated_at = Utc::now();
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        let msg = SessionMessage::user(vec![ContentBlock::text(content.into())]);
        self.add_message(msg);
    }

    pub fn add_assistant_message(&mut self, content: Vec<ContentBlock>, usage: Option<Usage>) {
        let mut msg = SessionMessage::assistant(content);
        if let Some(u) = usage {
            msg = msg.usage(TokenUsage {
                input_tokens: u.input_tokens as u64,
                output_tokens: u.output_tokens as u64,
                cache_read_input_tokens: u.cache_read_input_tokens.unwrap_or(0) as u64,
                cache_creation_input_tokens: u.cache_creation_input_tokens.unwrap_or(0) as u64,
            });
        }
        self.add_message(msg);
    }

    pub fn add_tool_results(&mut self, results: Vec<crate::types::ToolResultBlock>) {
        let content: Vec<ContentBlock> =
            results.into_iter().map(ContentBlock::ToolResult).collect();
        let msg = SessionMessage::user(content);
        self.add_message(msg);
    }

    pub fn should_compact(&self, max_tokens: u64, threshold: f32) -> bool {
        !self.current_branch_messages().is_empty()
            && self.current_input_tokens as f32 > max_tokens as f32 * threshold
    }

    pub fn update_usage(&mut self, usage: &Usage) {
        self.current_input_tokens = usage.context_usage() as u64;
        self.total_usage.add_usage(usage);
    }

    pub async fn compact(
        &mut self,
        client: &crate::Client,
    ) -> crate::Result<crate::types::CompactResult> {
        let executor = crate::session::compact::CompactExecutor::new(
            crate::session::compact::CompactStrategy::default(),
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
        self.updated_at = Utc::now();
    }

    pub fn refresh_message_projection(&mut self) {
        self.messages = self.current_branch_messages();
        self.current_leaf_id = self.messages.last().map(|message| message.id.clone());
        self.updated_at = Utc::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlock, Role};

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

        let msg1 = SessionMessage::user(vec![ContentBlock::text("Hello")]);
        session.add_message(msg1);

        assert_eq!(session.current_branch_messages().len(), 1);
        assert!(session.current_leaf_id.is_some());
        assert_eq!(session.current_branch_graph_nodes().len(), 1);
    }

    #[test]
    fn test_graph_tracks_message_lineage() {
        let mut session = Session::new(SessionConfig::default());

        session.add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]));
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text(
            "Hi there!",
        )]));

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
        session.add_message(SessionMessage::user(vec![ContentBlock::text("Hello")]));
        session.clear_messages();

        session.refresh_message_projection();

        assert_eq!(session.current_branch_messages().len(), 1);
        assert!(session.current_leaf_id.is_some());
    }

    #[test]
    fn test_message_tree() {
        let mut session = Session::new(SessionConfig::default());

        let user_msg = SessionMessage::user(vec![ContentBlock::text("Hello")]);
        session.add_message(user_msg);

        let assistant_msg = SessionMessage::assistant(vec![ContentBlock::text("Hi there!")]);
        session.add_message(assistant_msg);

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
            SessionMessage::assistant(vec![ContentBlock::text("Response 1")]).usage(TokenUsage {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            });
        session.add_message(msg1);

        let msg2 =
            SessionMessage::assistant(vec![ContentBlock::text("Response 2")]).usage(TokenUsage {
                input_tokens: 150,
                output_tokens: 75,
                ..Default::default()
            });
        session.add_message(msg2);

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
    fn test_message_caching_applies_to_last_user_turn() {
        let mut session = Session::new(SessionConfig::default());

        session.add_user_message("First question");
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text(
            "First answer",
        )]));
        session.add_user_message("Second question");

        let messages = session.to_api_messages();

        assert_eq!(messages.len(), 3);
        assert!(!messages[0].has_cache_control());
        assert!(!messages[1].has_cache_control());
        assert!(messages[2].has_cache_control());
    }

    #[test]
    fn test_message_caching_disabled() {
        let mut session = Session::new(SessionConfig::default());

        session.add_user_message("Question");

        // Pass None to disable caching
        let messages = session.to_api_messages_with_cache(None);

        assert_eq!(messages.len(), 1);
        assert!(!messages[0].has_cache_control());
    }

    #[test]
    fn test_message_caching_empty_session() {
        let session = Session::new(SessionConfig::default());
        let messages = session.to_api_messages();
        assert!(messages.is_empty());
    }

    #[test]
    fn test_message_caching_assistant_only() {
        let mut session = Session::new(SessionConfig::default());
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text("Hi")]));

        let messages = session.to_api_messages();

        assert_eq!(messages.len(), 1);
        assert!(!messages[0].has_cache_control());
    }
}
