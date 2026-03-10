use serde::{Deserialize, Serialize};

use crate::graph::{BranchExport, GraphSessionStats, SessionGraph};
use crate::session::{ExportPolicy, Session};

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
    pub stats: GraphSessionStats,
    pub export: BranchExport,
    pub graph: SessionGraph,
    pub compact_history: Vec<crate::session::CompactRecord>,
    pub summaries: Vec<crate::session::SummarySnapshot>,
    pub queue_state: Vec<crate::session::QueueItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreVerificationReport {
    pub session_id_matches: bool,
    pub branch_count_matches: bool,
    pub node_count_matches: bool,
    pub bookmark_count_matches: bool,
    pub checkpoint_count_matches: bool,
    pub replay_message_count_matches: bool,
}

impl RestoreVerificationReport {
    pub fn is_valid(&self) -> bool {
        self.session_id_matches
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
        RestoreVerificationReport {
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

        let report = RestoreVerifier::verify(&bundle, &session);
        assert!(report.is_valid());
    }
}
