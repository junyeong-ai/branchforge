use uuid::Uuid;

use super::{BranchId, NodeId};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    #[error("Graph branch {branch_id} does not exist")]
    MissingBranch { branch_id: BranchId },

    #[error("Graph node {node_id} does not exist")]
    MissingNode { node_id: NodeId },

    #[error("Graph node {node_id} references missing parent {parent_id}")]
    MissingParent { node_id: NodeId, parent_id: NodeId },

    #[error("Graph node {node_id} already exists")]
    DuplicateNode { node_id: NodeId },

    #[error(
        "Graph node {node_id} in branch {branch_id} references parent {parent_id} from branch {parent_branch_id}"
    )]
    ParentBranchMismatch {
        node_id: NodeId,
        branch_id: BranchId,
        parent_id: NodeId,
        parent_branch_id: BranchId,
    },

    #[error("Graph branch fork source {node_id} does not exist")]
    MissingForkSource { node_id: NodeId },

    #[error("Graph replay start node {node_id} is not reachable from branch {branch_id}")]
    ReplayStartOutsideBranch {
        node_id: NodeId,
        branch_id: BranchId,
    },

    #[error("Graph bookmark target node {node_id} does not exist")]
    MissingBookmarkTarget { node_id: NodeId },

    #[error("Graph branch export failed because branch {branch_id} does not exist")]
    ExportBranchMissing { branch_id: BranchId },

    #[error("Graph primary branch {branch_id} does not exist")]
    MissingPrimaryBranch { branch_id: BranchId },

    #[error("Graph bookmark {bookmark_id} belongs to missing branch {branch_id}")]
    BookmarkBranchMissing {
        bookmark_id: Uuid,
        branch_id: BranchId,
    },

    #[error("Graph checkpoint {checkpoint_id} belongs to missing branch {branch_id}")]
    CheckpointBranchMissing {
        checkpoint_id: NodeId,
        branch_id: BranchId,
    },

    #[error("Graph node {node_id} belongs to missing branch {branch_id}")]
    NodeBranchMissing {
        node_id: NodeId,
        branch_id: BranchId,
    },
}
