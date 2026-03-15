use serde::{Deserialize, Serialize};

use super::types::{Bookmark, BranchId, Checkpoint, NodeId, NodeKind, NodeProvenance};
use super::{GraphError, SessionGraph};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub parent_id: Option<NodeId>,
    pub created_by_principal_id: Option<String>,
    pub provenance: Option<NodeProvenance>,
    pub provenance_digest: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub tags: Vec<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchExport {
    pub branch_id: BranchId,
    pub branch_name: String,
    pub head: Option<NodeId>,
    pub checkpoints: Vec<ExportCheckpoint>,
    pub bookmarks: Vec<ExportBookmark>,
    pub tree: Vec<ExportTreeNode>,
    pub nodes: Vec<ExportNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportCheckpoint {
    pub id: NodeId,
    pub label: String,
    pub note: Option<String>,
    pub created_by_principal_id: Option<String>,
    pub provenance: Option<NodeProvenance>,
    pub provenance_digest: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportTreeNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub depth: usize,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportBookmark {
    pub id: uuid::Uuid,
    pub node_id: NodeId,
    pub label: String,
    pub note: Option<String>,
    pub created_by_principal_id: Option<String>,
    pub provenance: Option<NodeProvenance>,
    pub provenance_digest: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl SessionGraph {
    pub fn export_branch(&self, branch_id: BranchId) -> Result<BranchExport, GraphError> {
        let branch = self
            .branches
            .get(&branch_id)
            .ok_or(GraphError::ExportBranchMissing { branch_id })?;
        let branch_nodes = self.current_branch_nodes(branch_id);
        let nodes = branch_nodes
            .iter()
            .map(|node| ExportNode {
                id: node.id,
                kind: node.kind,
                parent_id: node.parent_id,
                created_by_principal_id: node.created_by_principal_id.clone(),
                provenance: node.provenance.clone(),
                provenance_digest: crate::graph::ProvenanceSummaryService::render_node_digest(node),
                created_at: node.created_at,
                tags: node.tags.clone(),
                payload: node.payload.clone(),
            })
            .collect();
        let checkpoints = self
            .checkpoints_for_branch(branch_id)
            .into_iter()
            .map(checkpoint_to_export)
            .collect();
        let bookmarks = self
            .bookmarks_for_branch(branch_id)
            .into_iter()
            .map(bookmark_to_export)
            .collect();
        let tree = branch_nodes
            .iter()
            .map(|node| ExportTreeNode {
                id: node.id,
                kind: node.kind,
                depth: self.node_depth(node.id),
                created_at: node.created_at,
            })
            .collect();

        Ok(BranchExport {
            branch_id,
            branch_name: branch.name.clone(),
            head: branch.head,
            checkpoints,
            bookmarks,
            tree,
            nodes,
        })
    }
}

fn checkpoint_to_export(checkpoint: &Checkpoint) -> ExportCheckpoint {
    ExportCheckpoint {
        id: checkpoint.id,
        label: checkpoint.label.clone(),
        note: checkpoint.note.clone(),
        created_by_principal_id: checkpoint.created_by_principal_id.clone(),
        provenance: checkpoint.provenance.clone(),
        provenance_digest: crate::graph::explorer::checkpoint_digest(checkpoint),
        created_at: checkpoint.created_at,
        tags: checkpoint.tags.clone(),
    }
}

fn bookmark_to_export(bookmark: &Bookmark) -> ExportBookmark {
    ExportBookmark {
        id: bookmark.id,
        node_id: bookmark.node_id,
        label: bookmark.label.clone(),
        note: bookmark.note.clone(),
        created_by_principal_id: bookmark.created_by_principal_id.clone(),
        provenance: bookmark.provenance.clone(),
        provenance_digest: crate::graph::explorer::bookmark_digest(bookmark),
        created_at: bookmark.created_at,
    }
}
