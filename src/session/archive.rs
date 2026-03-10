use serde::{Deserialize, Serialize};

use crate::graph::{BranchExport, GraphSessionStats, GraphValidator, SessionGraph};
use crate::session::{
    ExportPolicy, Session, SessionConfig, SessionPermissions, SessionState, SessionType, TokenUsage,
};
use rust_decimal::Decimal;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivePolicy {
    pub include_identity: bool,
    pub include_provenance: bool,
    pub include_tool_payloads: bool,
    pub include_compact_history: bool,
    pub include_summaries: bool,
    pub include_queue_state: bool,
}

impl Default for ArchivePolicy {
    fn default() -> Self {
        Self {
            include_identity: true,
            include_provenance: true,
            include_tool_payloads: true,
            include_compact_history: true,
            include_summaries: true,
            include_queue_state: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionArchiveBundle {
    pub bundle_version: u32,
    pub session_id: String,
    pub tenant_id: Option<String>,
    pub principal_id: Option<String>,
    pub static_context_hash: Option<String>,
    pub session_type: SessionType,
    pub state: SessionState,
    pub config: SessionConfig,
    pub permissions: SessionPermissions,
    pub summary: Option<String>,
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
    pub summaries: Vec<crate::session::SummarySnapshot>,
    pub queue_state: Vec<crate::session::QueueItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreVerificationReport {
    pub graph_valid: bool,
    pub session_id_matches: bool,
    pub branch_count_matches: bool,
    pub node_count_matches: bool,
    pub bookmark_count_matches: bool,
    pub checkpoint_count_matches: bool,
    pub replay_message_count_matches: bool,
}

impl RestoreVerificationReport {
    pub fn is_valid(&self) -> bool {
        self.graph_valid
            && self.session_id_matches
            && self.branch_count_matches
            && self.node_count_matches
            && self.bookmark_count_matches
            && self.checkpoint_count_matches
            && self.replay_message_count_matches
    }
}

pub struct RestoreVerifier;

impl RestoreVerifier {
    pub fn verify(bundle: &SessionArchiveBundle, restored: &Session) -> RestoreVerificationReport {
        let graph_report = GraphValidator::validate_restore_pair(&bundle.graph, &restored.graph);
        RestoreVerificationReport {
            graph_valid: graph_report.is_valid(),
            session_id_matches: bundle.session_id == restored.id.to_string(),
            branch_count_matches: bundle.graph.branches.len() == restored.graph.branches.len(),
            node_count_matches: bundle.graph.nodes.len() == restored.graph.nodes.len(),
            bookmark_count_matches: bundle.graph.bookmarks.len() == restored.graph.bookmarks.len(),
            checkpoint_count_matches: bundle.graph.checkpoints.len()
                == restored.graph.checkpoints.len(),
            replay_message_count_matches: bundle.export.nodes.len()
                == restored
                    .export_current_branch()
                    .map(|export| export.nodes.len())
                    .unwrap_or_default(),
        }
    }
}

pub struct SessionArchiveService;

impl SessionArchiveService {
    pub fn export_bundle(
        session: &Session,
        export_policy: &ExportPolicy,
        archive_policy: &ArchivePolicy,
    ) -> Option<SessionArchiveBundle> {
        let export = crate::session::SessionExporter::export_branch_with_policy(
            &session.graph,
            session.graph.primary_branch,
            export_policy,
        )?;
        let stats = crate::graph::GraphSearchService::stats(&session.graph);

        Some(SessionArchiveBundle {
            bundle_version: 1,
            session_id: session.id.to_string(),
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
            permissions: session.permissions.clone(),
            summary: session.summary.clone(),
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
            graph: session.graph.clone(),
            compact_history: if archive_policy.include_compact_history {
                session.compact_history.iter().cloned().collect()
            } else {
                Vec::new()
            },
            summaries: Vec::new(),
            queue_state: Vec::new(),
        })
    }

    pub fn import_bundle(bundle: &SessionArchiveBundle) -> Session {
        let mut session = Session {
            id: crate::session::SessionId::from(bundle.session_id.clone()),
            parent_id: None,
            session_type: bundle.session_type.clone(),
            tenant_id: bundle.tenant_id.clone(),
            principal_id: bundle.principal_id.clone(),
            state: bundle.state,
            config: bundle.config.clone(),
            permissions: bundle.permissions.clone(),
            messages: Vec::new(),
            current_leaf_id: None,
            summary: bundle.summary.clone(),
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
        };
        session.refresh_message_projection();
        session
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionConfig, SessionMessage};
    use crate::types::ContentBlock;

    #[test]
    fn archive_bundle_and_restore_verifier_work() {
        let mut session = Session::new(SessionConfig::default());
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        session.add_message(SessionMessage::user(vec![ContentBlock::text("hello")]));

        let bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy::default(),
        )
        .expect("bundle should be created");

        let restored = SessionArchiveService::import_bundle(&bundle);
        let report = RestoreVerifier::verify(&bundle, &restored);
        assert!(report.is_valid());
    }

    #[test]
    fn archive_import_restores_identity_and_projection() {
        let mut session = Session::new(SessionConfig::default());
        session.set_identity(Some("tenant-a".to_string()), Some("user-1".to_string()));
        session.add_message(SessionMessage::user(vec![ContentBlock::text("hello")]));
        session.add_message(SessionMessage::assistant(vec![ContentBlock::text("world")]));

        let bundle = SessionArchiveService::export_bundle(
            &session,
            &crate::session::ExportPolicy::default(),
            &ArchivePolicy::default(),
        )
        .expect("bundle should be created");
        let restored = SessionArchiveService::import_bundle(&bundle);

        assert_eq!(restored.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(restored.principal_id.as_deref(), Some("user-1"));
        assert_eq!(restored.current_branch_messages().len(), 2);
    }
}
