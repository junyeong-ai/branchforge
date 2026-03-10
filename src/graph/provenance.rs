use serde::{Deserialize, Serialize};

use super::BranchSummary;
use super::types::GraphNode;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceDigest {
    pub actor: Option<String>,
    pub session_type: Option<String>,
    pub subagent_type: Option<String>,
    pub task_id: Option<String>,
}

pub struct ProvenanceSummaryService;

impl ProvenanceSummaryService {
    pub fn node_digest(node: &GraphNode) -> Option<ProvenanceDigest> {
        let provenance = node.provenance.as_ref();
        Some(ProvenanceDigest {
            actor: node.created_by_principal_id.clone(),
            session_type: provenance.map(|p| p.session_type.clone()),
            subagent_type: provenance.and_then(|p| p.subagent_type.clone()),
            task_id: provenance.and_then(|p| p.task_id.clone()),
        })
    }

    pub fn branch_digest(summary: &BranchSummary) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(divergence) = summary.divergence_from_primary {
            lines.push(format!("divergence:{}", divergence));
        }
        if summary.tool_activity_count > 0 {
            lines.push(format!("tool_activity:{}", summary.tool_activity_count));
        }
        if summary.summary_count > 0 {
            lines.push(format!("summaries:{}", summary.summary_count));
        }
        lines
    }

    pub fn render_node_digest(node: &GraphNode) -> Option<String> {
        let digest = Self::node_digest(node)?;
        let mut parts = Vec::new();
        if let Some(actor) = digest.actor {
            parts.push(format!("actor:{}", actor));
        }
        if let Some(session_type) = digest.session_type {
            parts.push(format!("session:{}", session_type));
        }
        if let Some(subagent_type) = digest.subagent_type {
            parts.push(format!("subagent:{}", subagent_type));
        }
        if let Some(task_id) = digest.task_id {
            parts.push(format!("task:{}", task_id));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::NodeProvenance;
    use crate::graph::{GraphNode, NodeKind};
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn provenance_digest_renders_compact_summary() {
        let node = GraphNode {
            id: Uuid::new_v4(),
            branch_id: Uuid::new_v4(),
            kind: NodeKind::Assistant,
            parent_id: None,
            created_by_principal_id: Some("user-1".to_string()),
            provenance: Some(NodeProvenance {
                source_session_id: "s1".to_string(),
                session_type: "subagent".to_string(),
                task_id: Some("task-1".to_string()),
                subagent_session_id: Some("subagent-1".to_string()),
                subagent_type: Some("Explore".to_string()),
                subagent_description: Some("Inspect repo".to_string()),
            }),
            created_at: Utc::now(),
            tags: Vec::new(),
            payload: serde_json::json!({}),
        };

        let digest = ProvenanceSummaryService::render_node_digest(&node).unwrap();
        assert!(digest.contains("actor:user-1"));
        assert!(digest.contains("subagent:Explore"));
    }
}
