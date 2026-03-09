use serde::{Deserialize, Serialize};

use super::SessionGraph;
use super::types::{Bookmark, BranchId, Checkpoint, NodeId, NodeKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub parent_id: Option<NodeId>,
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
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl SessionGraph {
    pub fn export_branch(&self, branch_id: BranchId) -> Option<BranchExport> {
        let branch = self.branches.get(&branch_id)?;
        let branch_nodes = self.current_branch_nodes(branch_id);
        let nodes = branch_nodes
            .iter()
            .map(|node| ExportNode {
                id: node.id,
                kind: node.kind,
                parent_id: node.parent_id,
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

        Some(BranchExport {
            branch_id,
            branch_name: branch.name.clone(),
            head: branch.head,
            checkpoints,
            bookmarks,
            tree,
            nodes,
        })
    }

    fn node_depth(&self, node_id: NodeId) -> usize {
        let mut depth = 0;
        let mut current = self.nodes.get(&node_id).and_then(|node| node.parent_id);
        while let Some(parent_id) = current {
            depth += 1;
            current = self.nodes.get(&parent_id).and_then(|node| node.parent_id);
        }
        depth
    }
}

fn checkpoint_to_export(checkpoint: &Checkpoint) -> ExportCheckpoint {
    ExportCheckpoint {
        id: checkpoint.id,
        label: checkpoint.label.clone(),
        note: checkpoint.note.clone(),
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
        created_at: bookmark.created_at,
    }
}
