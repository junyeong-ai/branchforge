use serde::{Deserialize, Serialize};

use crate::graph::{
    BranchExport, GraphEventBody, GraphSessionStats, GraphValidator, NodeKind, SessionGraph,
};
use crate::session::{
    ExportPolicy, Persistence, QueueItem, Session, SessionAuthorization, SessionConfig,
    SessionError, SessionResult, SessionState, SessionType, TokenUsage,
};
use rust_decimal::Decimal;

const CURRENT_ARCHIVE_BUNDLE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivePolicy {
    pub include_identity: bool,
    pub include_provenance: bool,
    pub include_tool_payloads: bool,
    pub include_compact_history: bool,
    pub include_queue_state: bool,
}

impl Default for ArchivePolicy {
    fn default() -> Self {
        Self {
            include_identity: true,
            include_provenance: true,
            include_tool_payloads: true,
            include_compact_history: true,
            include_queue_state: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionArchiveBundle {
    pub bundle_version: u32,
    pub session_id: String,
    pub parent_id: Option<String>,
    pub tenant_id: Option<String>,
    pub principal_id: Option<String>,
    pub static_context_hash: Option<String>,
    pub session_type: SessionType,
    pub state: SessionState,
    pub config: SessionConfig,
    pub authorization: SessionAuthorization,
    pub total_usage: TokenUsage,
    pub current_input_tokens: u64,
    pub total_cost_usd: Decimal,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
    pub todos: Vec<crate::session::TodoItem>,
    pub current_plan: Option<crate::session::Plan>,
    pub stats: GraphSessionStats,
    pub export: BranchExport,
    pub graph: SessionGraph,
    pub compact_history: Vec<crate::session::CompactRecord>,
    pub queue_state: Vec<crate::session::QueueItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreVerificationReport {
    pub graph_valid: bool,
    pub session_id_matches: bool,
    pub parent_id_matches: bool,
    pub tenant_id_matches: bool,
    pub principal_id_matches: bool,
    pub session_type_matches: bool,
    pub state_matches: bool,
    pub config_matches: bool,
    pub authorization_matches: bool,
    pub total_usage_matches: bool,
    pub current_input_tokens_matches: bool,
    pub total_cost_matches: bool,
    pub static_context_hash_matches: bool,
    pub created_at_matches: bool,
    pub updated_at_matches: bool,
    pub expires_at_matches: bool,
    pub error_matches: bool,
    pub todos_matches: bool,
    pub current_plan_matches: bool,
    pub compact_history_matches: bool,
    pub branch_count_matches: bool,
    pub node_count_matches: bool,
    pub bookmark_count_matches: bool,
    pub checkpoint_count_matches: bool,
    pub replay_message_count_matches: bool,
    pub queue_state_matches: bool,
}

impl RestoreVerificationReport {
    pub fn is_valid(&self) -> bool {
        self.graph_valid
            && self.session_id_matches
            && self.parent_id_matches
            && self.tenant_id_matches
            && self.principal_id_matches
            && self.session_type_matches
            && self.state_matches
            && self.config_matches
            && self.authorization_matches
            && self.total_usage_matches
            && self.current_input_tokens_matches
            && self.total_cost_matches
            && self.static_context_hash_matches
            && self.created_at_matches
            && self.updated_at_matches
            && self.expires_at_matches
            && self.error_matches
            && self.todos_matches
            && self.current_plan_matches
            && self.compact_history_matches
            && self.branch_count_matches
            && self.node_count_matches
            && self.bookmark_count_matches
            && self.checkpoint_count_matches
            && self.replay_message_count_matches
            && self.queue_state_matches
    }

    fn with_queue_state_matches(mut self, matches: bool) -> Self {
        self.queue_state_matches = matches;
        self
    }
}

pub struct RestoreVerifier;

impl RestoreVerifier {
    pub fn verify(bundle: &SessionArchiveBundle, restored: &Session) -> RestoreVerificationReport {
        let graph_report = GraphValidator::validate_restore_pair(&bundle.graph, &restored.graph);
        RestoreVerificationReport {
            graph_valid: graph_report.is_valid(),
            session_id_matches: bundle.session_id == restored.id.to_string(),
            parent_id_matches: bundle.parent_id.as_deref()
                == restored.parent_id.map(|id| id.to_string()).as_deref(),
            tenant_id_matches: bundle.tenant_id == restored.tenant_id,
            principal_id_matches: bundle.principal_id == restored.principal_id,
            session_type_matches: bundle.session_type == restored.session_type,
            state_matches: bundle.state == restored.state,
            config_matches: bundle.config == restored.config,
            authorization_matches: bundle.authorization == restored.authorization,
            total_usage_matches: bundle.total_usage == restored.total_usage,
            current_input_tokens_matches: bundle.current_input_tokens
                == restored.current_input_tokens,
            total_cost_matches: bundle.total_cost_usd == restored.total_cost_usd,
            static_context_hash_matches: bundle.static_context_hash == restored.static_context_hash,
            created_at_matches: bundle.created_at == restored.created_at,
            updated_at_matches: bundle.updated_at == restored.updated_at,
            expires_at_matches: bundle.expires_at == restored.expires_at,
            error_matches: bundle.error == restored.error,
            todos_matches: bundle.todos == restored.todos,
            current_plan_matches: bundle.current_plan == restored.current_plan,
            compact_history_matches: bundle
                .compact_history
                .iter()
                .eq(restored.compact_history.iter()),
            branch_count_matches: bundle.graph.branches.len() == restored.graph.branches.len(),
            node_count_matches: bundle.graph.nodes.len() == restored.graph.nodes.len(),
            bookmark_count_matches: bundle.graph.bookmarks.len() == restored.graph.bookmarks.len(),
            checkpoint_count_matches: bundle.graph.checkpoints.len()
                == restored.graph.checkpoints.len(),
            replay_message_count_matches: restored
                .export_current_branch()
                .map(|export| bundle.export.nodes.len() == export.nodes.len())
                .unwrap_or(false),
            queue_state_matches: true,
        }
    }

    pub fn verify_with_queue(
        bundle: &SessionArchiveBundle,
        restored: &Session,
        restored_queue: &[QueueItem],
    ) -> RestoreVerificationReport {
        Self::verify(bundle, restored).with_queue_state_matches(pending_queue_matches(
            &bundle.queue_state,
            restored_queue,
            restored.id,
        ))
    }
}

pub(crate) fn verify_restored_session_roundtrip(
    expected: &Session,
    expected_queue: &[QueueItem],
    restored: &Session,
    restored_queue: &[QueueItem],
) -> SessionResult<()> {
    let graph_report = GraphValidator::validate_restore_pair(&expected.graph, &restored.graph);
    let mut mismatches = Vec::new();

    if !graph_report.is_valid() {
        let issues = graph_report
            .issues
            .into_iter()
            .map(|issue| format!("{}: {}", issue.code, issue.message))
            .collect::<Vec<_>>()
            .join(", ");
        mismatches.push(format!("graph mismatch ({issues})"));
    }

    if expected.id != restored.id {
        mismatches.push("session_id".to_string());
    }
    if expected.parent_id != restored.parent_id {
        mismatches.push("parent_id".to_string());
    }
    if expected.tenant_id != restored.tenant_id {
        mismatches.push("tenant_id".to_string());
    }
    if expected.principal_id != restored.principal_id {
        mismatches.push("principal_id".to_string());
    }
    if expected.session_type != restored.session_type {
        mismatches.push("session_type".to_string());
    }
    if expected.state != restored.state {
        mismatches.push("state".to_string());
    }
    if expected.config != restored.config {
        mismatches.push("config".to_string());
    }
    if expected.authorization != restored.authorization {
        mismatches.push("authorization".to_string());
    }
    if expected.total_usage != restored.total_usage {
        mismatches.push("total_usage".to_string());
    }
    if expected.current_input_tokens != restored.current_input_tokens {
        mismatches.push("current_input_tokens".to_string());
    }
    if expected.total_cost_usd != restored.total_cost_usd {
        mismatches.push("total_cost_usd".to_string());
    }
    if expected.static_context_hash != restored.static_context_hash {
        mismatches.push("static_context_hash".to_string());
    }
    if expected.created_at != restored.created_at {
        mismatches.push("created_at".to_string());
    }
    if expected.updated_at != restored.updated_at {
        mismatches.push("updated_at".to_string());
    }
    if expected.expires_at != restored.expires_at {
        mismatches.push("expires_at".to_string());
    }
    if expected.error != restored.error {
        mismatches.push("error".to_string());
    }
    if expected.todos != restored.todos {
        mismatches.push("todos".to_string());
    }
    if expected.current_plan != restored.current_plan {
        mismatches.push("current_plan".to_string());
    }
    if expected
        .compact_history
        .iter()
        .ne(restored.compact_history.iter())
    {
        mismatches.push("compact_history".to_string());
    }
    if expected.graph.primary_branch != restored.graph.primary_branch {
        mismatches.push("primary_branch".to_string());
    }
    if expected.current_leaf_id != restored.current_leaf_id {
        mismatches.push("current_leaf_id".to_string());
    }
    if expected.graph.branches.len() != restored.graph.branches.len() {
        mismatches.push("branch_count".to_string());
    }
    if expected.graph.nodes.len() != restored.graph.nodes.len() {
        mismatches.push("node_count".to_string());
    }
    if expected.graph.bookmarks.len() != restored.graph.bookmarks.len() {
        mismatches.push("bookmark_count".to_string());
    }
    if expected.graph.checkpoints.len() != restored.graph.checkpoints.len() {
        mismatches.push("checkpoint_count".to_string());
    }
    let expected_export_len = expected
        .export_current_branch()
        .ok()
        .map(|export| export.nodes.len());
    let restored_export_len = restored
        .export_current_branch()
        .ok()
        .map(|export| export.nodes.len());
    if expected_export_len != restored_export_len {
        mismatches.push("replay_message_count".to_string());
    }
    if !pending_queue_matches(expected_queue, restored_queue, restored.id) {
        mismatches.push("queue_state".to_string());
    }

    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(SessionError::Storage {
            message: format!(
                "Archive restore verification failed: {}",
                mismatches.join(", ")
            ),
        })
    }
}

pub struct SessionArchiveService;

impl SessionArchiveService {
    fn validate_graph(graph: &SessionGraph, context: &str) -> SessionResult<()> {
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
            message: format!("Invalid {context}: {issues}"),
        })
    }

    fn validate_bundle_version(bundle: &SessionArchiveBundle) -> SessionResult<()> {
        if bundle.bundle_version != CURRENT_ARCHIVE_BUNDLE_VERSION {
            return Err(SessionError::Storage {
                message: format!(
                    "Unsupported archive bundle version {} (expected {})",
                    bundle.bundle_version, CURRENT_ARCHIVE_BUNDLE_VERSION
                ),
            });
        }
        Ok(())
    }

    fn parse_bundle_session_id(
        value: &str,
        field: &str,
    ) -> SessionResult<crate::session::SessionId> {
        crate::session::SessionId::parse(value).ok_or_else(|| SessionError::Storage {
            message: format!("Archive bundle {field} must be a valid session UUID, got '{value}'"),
        })
    }

    pub fn export_bundle(
        session: &Session,
        export_policy: &ExportPolicy,
        archive_policy: &ArchivePolicy,
        pending_queue: Vec<QueueItem>,
    ) -> SessionResult<SessionArchiveBundle> {
        let graph = graph_with_archive_policy(&session.graph, archive_policy);
        Self::validate_graph(&graph, "archive graph")?;
        let export = crate::session::SessionExporter::export_branch_with_policy(
            &graph,
            graph.primary_branch,
            export_policy,
        )?;
        let stats = crate::graph::GraphSearchService::stats(&graph);

        Ok(SessionArchiveBundle {
            bundle_version: CURRENT_ARCHIVE_BUNDLE_VERSION,
            session_id: session.id.to_string(),
            parent_id: session.parent_id.map(|id| id.to_string()),
            tenant_id: archive_policy
                .include_identity
                .then(|| session.tenant_id.clone())
                .flatten(),
            principal_id: archive_policy
                .include_identity
                .then(|| session.principal_id.clone())
                .flatten(),
            static_context_hash: session.static_context_hash.clone(),
            session_type: session.session_type.clone(),
            state: session.state,
            config: session.config.clone(),
            authorization: session.authorization.clone(),
            total_usage: session.total_usage.clone(),
            current_input_tokens: session.current_input_tokens,
            total_cost_usd: session.total_cost_usd,
            created_at: session.created_at,
            updated_at: session.updated_at,
            expires_at: session.expires_at,
            error: session.error.clone(),
            todos: session.todos.clone(),
            current_plan: session.current_plan.clone(),
            stats,
            export,
            graph,
            compact_history: if archive_policy.include_compact_history {
                session.compact_history.iter().cloned().collect()
            } else {
                Vec::new()
            },
            queue_state: if archive_policy.include_queue_state {
                pending_queue
            } else {
                Vec::new()
            },
        })
    }

    pub fn import_bundle(bundle: &SessionArchiveBundle) -> SessionResult<Session> {
        Self::validate_bundle_version(bundle)?;
        Self::validate_graph(&bundle.graph, "archive bundle graph")?;
        let session_id = Self::parse_bundle_session_id(&bundle.session_id, "session_id")?;
        let parent_id = bundle
            .parent_id
            .as_deref()
            .map(|value| Self::parse_bundle_session_id(value, "parent_id"))
            .transpose()?;
        let mut session = Session {
            id: session_id,
            parent_id,
            session_type: bundle.session_type.clone(),
            tenant_id: bundle.tenant_id.clone(),
            principal_id: bundle.principal_id.clone(),
            state: bundle.state,
            config: bundle.config.clone(),
            authorization: bundle.authorization.clone(),
            messages: Vec::new(),
            current_leaf_id: None,
            summary: bundle.graph.latest_summary(),
            total_usage: bundle.total_usage.clone(),
            current_input_tokens: bundle.current_input_tokens,
            total_cost_usd: bundle.total_cost_usd,
            static_context_hash: bundle.static_context_hash.clone(),
            graph: bundle.graph.clone(),
            created_at: bundle.created_at,
            updated_at: bundle.updated_at,
            expires_at: bundle.expires_at,
            error: bundle.error.clone(),
            todos: bundle.todos.clone(),
            current_plan: bundle.current_plan.clone(),
            compact_history: bundle.compact_history.iter().cloned().collect(),
            event_bus: None,
        };
        session.refresh_summary_cache();
        session.refresh_message_projection();
        Ok(session)
    }

    pub async fn restore_into(
        bundle: &SessionArchiveBundle,
        persistence: &dyn Persistence,
    ) -> SessionResult<Session> {
        let restored = Self::import_bundle(bundle)?;
        let report = RestoreVerifier::verify_with_queue(bundle, &restored, &bundle.queue_state);
        if !report.is_valid() {
            return Err(SessionError::Storage {
                message: "Archive restore verification failed".to_string(),
            });
        }
        persistence
            .restore_bundle(&restored, &bundle.queue_state)
            .await?;
        persistence
            .load(&restored.id)
            .await?
            .ok_or_else(|| SessionError::NotFound {
                id: restored.id.to_string(),
            })
    }
}

fn pending_queue_matches(
    expected: &[QueueItem],
    actual: &[QueueItem],
    session_id: crate::session::SessionId,
) -> bool {
    normalize_queue(expected, session_id) == normalize_queue(actual, session_id)
}

fn normalize_queue(items: &[QueueItem], session_id: crate::session::SessionId) -> Vec<QueueItem> {
    let mut normalized: Vec<QueueItem> = items
        .iter()
        .cloned()
        .map(|mut item| {
            item.session_id = session_id;
            item.status = crate::session::QueueStatus::Pending;
            item.processed_at = None;
            item
        })
        .collect();
    normalized.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then(left.created_at.cmp(&right.created_at))
            .then(left.id.cmp(&right.id))
    });
    normalized
}

fn graph_with_archive_policy(graph: &SessionGraph, policy: &ArchivePolicy) -> SessionGraph {
    let mut graph = graph.clone();

    if !policy.include_identity {
        for node in graph.nodes.values_mut() {
            node.created_by_principal_id = None;
        }
        for checkpoint in graph.checkpoints.values_mut() {
            checkpoint.created_by_principal_id = None;
        }
        for bookmark in graph.bookmarks.values_mut() {
            bookmark.created_by_principal_id = None;
        }
        for event in &mut graph.events {
            event.metadata.actor = None;
        }
    }

    if !policy.include_provenance {
        for node in graph.nodes.values_mut() {
            node.provenance = None;
            node.created_by_principal_id = None;
        }
        for checkpoint in graph.checkpoints.values_mut() {
            checkpoint.provenance = None;
            checkpoint.created_by_principal_id = None;
        }
        for bookmark in graph.bookmarks.values_mut() {
            bookmark.provenance = None;
            bookmark.created_by_principal_id = None;
        }
        for event in &mut graph.events {
            event.metadata.actor = None;
            match &mut event.body {
                GraphEventBody::NodeAppended { provenance, .. }
                | GraphEventBody::CheckpointCreated { provenance, .. }
                | GraphEventBody::BookmarkCreated { provenance, .. } => {
                    *provenance = None;
                }
                GraphEventBody::BranchForked { .. }
                | GraphEventBody::NodeMetadataPatched { .. } => {}
            }
        }
    }

    if !policy.include_tool_payloads {
        for node in graph.nodes.values_mut() {
            if matches!(node.kind, NodeKind::ToolCall | NodeKind::ToolResult) {
                node.payload = serde_json::json!({ "redacted": true });
            }
        }
        for event in &mut graph.events {
            if let GraphEventBody::NodeAppended { kind, payload, .. } = &mut event.body
                && matches!(kind, NodeKind::ToolCall | NodeKind::ToolResult)
            {
                *payload = serde_json::json!({ "redacted": true });
            }
        }
    }

    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{MemoryPersistence, Persistence, SessionConfig, SessionMessage};
    use crate::types::ContentBlock;
    use std::sync::Arc;

    #[test]
    fn archive_bundle_and_restore_verifier_work() {
        let mut session = Session::new(SessionConfig::default());
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("hello")]))
            .unwrap();

        let bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy::default(),
            Vec::new(),
        )
        .expect("bundle should be created");

        let restored = SessionArchiveService::import_bundle(&bundle).unwrap();
        let report = RestoreVerifier::verify(&bundle, &restored);
        assert!(report.is_valid());
    }

    #[test]
    fn archive_restore_verifier_detects_identity_mismatch() {
        let mut session = Session::new(SessionConfig::default());
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));

        let bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy::default(),
            Vec::new(),
        )
        .expect("bundle should be created");

        let mut restored = SessionArchiveService::import_bundle(&bundle).unwrap();
        restored.tenant_id = Some("tenant-b".to_string());

        let report = RestoreVerifier::verify(&bundle, &restored);
        assert!(!report.is_valid());
        assert!(!report.tenant_id_matches);
    }

    #[test]
    fn archive_import_restores_identity_and_projection() {
        let mut session = Session::new(SessionConfig::default());
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("hello")]))
            .unwrap();
        session
            .add_message(SessionMessage::assistant(vec![ContentBlock::text("world")]))
            .unwrap();

        let bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy::default(),
            Vec::new(),
        )
        .expect("bundle should be created");
        let restored = SessionArchiveService::import_bundle(&bundle).unwrap();

        assert_eq!(restored.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(restored.principal_id.as_deref(), Some("user-1"));
        assert_eq!(restored.current_branch_messages().len(), 2);
    }

    #[test]
    fn archive_import_rejects_invalid_graph() {
        let mut session = Session::new(SessionConfig::default());
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("hello")]))
            .unwrap();

        let mut bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy::default(),
            Vec::new(),
        )
        .expect("bundle should be created");

        let node = bundle
            .graph
            .nodes
            .values_mut()
            .next()
            .expect("graph has node");
        node.provenance = None;
        node.created_by_principal_id = Some("user-1".to_string());

        let error = SessionArchiveService::import_bundle(&bundle)
            .expect_err("invalid graph should fail import");
        assert!(error.to_string().contains("Invalid archive bundle graph"));
    }

    #[test]
    fn archive_export_rejects_invalid_graph() {
        let mut session = Session::new(SessionConfig::default());
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("hello")]))
            .unwrap();

        let node = session
            .graph
            .nodes
            .values_mut()
            .next()
            .expect("graph has node");
        node.provenance = None;
        node.created_by_principal_id = Some("user-1".to_string());

        let error = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy::default(),
            Vec::new(),
        )
        .expect_err("invalid graph should fail export");
        assert!(error.to_string().contains("Invalid archive graph"));
    }

    #[test]
    fn archive_policy_redacts_graph_identity_provenance_and_tool_payloads() {
        let mut session = Session::new(SessionConfig::default());
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("hello")]))
            .unwrap();
        session
            .graph
            .append_node_with_actor(
                session.graph.primary_branch,
                NodeKind::ToolCall,
                serde_json::json!({"tool":"Read","file_path":"secret.txt"}),
                session.principal_id.clone(),
                crate::session::state::build_graph_provenance(session.id, &session.session_type),
            )
            .unwrap();
        session
            .graph
            .create_checkpoint(
                session.graph.primary_branch,
                "cp",
                Some("note".to_string()),
                vec![],
                session.principal_id.clone(),
                crate::session::state::build_graph_provenance(session.id, &session.session_type),
            )
            .unwrap();

        let bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy {
                include_identity: false,
                include_provenance: false,
                include_tool_payloads: false,
                include_compact_history: true,
                include_queue_state: false,
            },
            Vec::new(),
        )
        .expect("bundle should be created");

        assert!(bundle.tenant_id.is_none());
        assert!(bundle.principal_id.is_none());
        assert!(
            bundle
                .graph
                .nodes
                .values()
                .all(|node| node.created_by_principal_id.is_none() && node.provenance.is_none())
        );
        assert!(
            bundle
                .graph
                .events
                .iter()
                .all(|event| event.metadata.actor.is_none())
        );
        assert!(bundle.graph.nodes.values().any(|node| {
            matches!(node.kind, NodeKind::ToolCall | NodeKind::ToolResult)
                && node.payload == serde_json::json!({"redacted": true})
        }));
    }

    #[test]
    fn archive_policy_is_a_redaction_floor_for_embedded_export() {
        let mut session = Session::new(SessionConfig::default());
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        session
            .graph
            .append_node_with_actor(
                session.graph.primary_branch,
                NodeKind::ToolCall,
                serde_json::json!({"tool":"Read","file_path":"secret.txt"}),
                session.principal_id.clone(),
                crate::session::state::build_graph_provenance(session.id, &session.session_type),
            )
            .unwrap();

        let bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy {
                include_identity: true,
                include_provenance: true,
                include_tool_payloads: true,
            },
            &ArchivePolicy {
                include_identity: false,
                include_provenance: false,
                include_tool_payloads: false,
                include_compact_history: true,
                include_queue_state: false,
            },
            Vec::new(),
        )
        .expect("bundle should be created");

        assert!(
            bundle
                .export
                .nodes
                .iter()
                .all(|node| node.created_by_principal_id.is_none() && node.provenance.is_none())
        );
        assert!(bundle.export.nodes.iter().any(|node| {
            matches!(node.kind, NodeKind::ToolCall | NodeKind::ToolResult)
                && node.payload == serde_json::json!({"redacted": true})
        }));
    }

    #[tokio::test]
    async fn archive_restore_into_persistence_preserves_parent_and_pending_queue() {
        let persistence = Arc::new(MemoryPersistence::new());
        let parent = Session::new(SessionConfig::default());
        persistence.save(&parent).await.unwrap();

        let mut session = Session::new_subagent(
            parent.id,
            "explore",
            "Review auth",
            SessionConfig::default(),
        );
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        session
            .add_message(SessionMessage::user(vec![ContentBlock::text("hello")]))
            .unwrap();

        let pending = vec![
            crate::session::QueueItem::enqueue(session.id, "first").priority(10),
            crate::session::QueueItem::enqueue(session.id, "second").priority(1),
        ];

        let bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy {
                include_queue_state: true,
                ..ArchivePolicy::default()
            },
            pending.clone(),
        )
        .expect("bundle should be created");

        let restored = SessionArchiveService::restore_into(&bundle, persistence.as_ref())
            .await
            .expect("restore should succeed");

        assert_eq!(restored.parent_id, Some(parent.id));

        let queue = persistence.pending_queue(&restored.id).await.unwrap();
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].id, pending[0].id);
        assert_eq!(queue[1].id, pending[1].id);
    }

    #[tokio::test]
    async fn archive_restore_refuses_to_overwrite_existing_session() {
        let persistence = Arc::new(MemoryPersistence::new());
        let mut existing = Session::new(SessionConfig::default());
        existing.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        existing
            .add_message(SessionMessage::user(vec![ContentBlock::text("original")]))
            .unwrap();
        persistence.save(&existing).await.unwrap();

        let mut imported = Session::new(SessionConfig::default());
        imported.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        imported
            .add_message(SessionMessage::assistant(vec![ContentBlock::text(
                "replacement",
            )]))
            .unwrap();

        let mut bundle = SessionArchiveService::export_bundle(
            &imported,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy::default(),
            Vec::new(),
        )
        .expect("bundle should be created");
        bundle.session_id = existing.id.to_string();

        let error = SessionArchiveService::restore_into(&bundle, persistence.as_ref())
            .await
            .expect_err("restore should refuse overwrite");
        assert!(error.to_string().contains("refuses to overwrite"));

        let stored = persistence.load(&existing.id).await.unwrap().unwrap();
        let messages = stored.current_branch_messages();
        assert_eq!(messages.len(), 1);
        let text = messages[0].content.iter().find_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        });
        assert_eq!(text, Some("original"));
    }

    #[tokio::test]
    async fn archive_restore_verification_failure_rolls_back_restored_session() {
        let persistence = Arc::new(MemoryPersistence::new());
        let mut session = Session::new(SessionConfig::default());
        session
            .add_message(SessionMessage::assistant(vec![ContentBlock::text(
                "restored output",
            )]))
            .unwrap();

        let mut bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy {
                include_queue_state: true,
                ..ArchivePolicy::default()
            },
            vec![crate::session::QueueItem::enqueue(session.id, "queued")],
        )
        .expect("bundle should be created");

        bundle.export.nodes.clear();

        let error = SessionArchiveService::restore_into(&bundle, persistence.as_ref())
            .await
            .expect_err("restore should fail verification");
        assert!(
            error
                .to_string()
                .contains("Archive restore verification failed")
        );
        assert!(persistence.load(&session.id).await.unwrap().is_none());
        assert!(
            persistence
                .pending_queue(&session.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn archive_import_rejects_unsupported_bundle_version() {
        let session = Session::new(SessionConfig::default());
        let mut bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy::default(),
            Vec::new(),
        )
        .expect("bundle should be created");
        bundle.bundle_version = CURRENT_ARCHIVE_BUNDLE_VERSION + 1;

        let error = SessionArchiveService::import_bundle(&bundle)
            .expect_err("unsupported archive version should fail");
        assert!(
            error
                .to_string()
                .contains("Unsupported archive bundle version")
        );
    }

    #[test]
    fn archive_import_rejects_invalid_session_uuid() {
        let session = Session::new(SessionConfig::default());
        let mut bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy::default(),
            Vec::new(),
        )
        .expect("bundle should be created");
        bundle.session_id = "not-a-uuid".to_string();

        let error = SessionArchiveService::import_bundle(&bundle)
            .expect_err("invalid session id should fail");
        assert!(
            error
                .to_string()
                .contains("Archive bundle session_id must be a valid session UUID")
        );
    }
}
