use serde::{Deserialize, Serialize};

use super::SessionGraph;
use super::types::{BranchId, NodeId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphReference {
    Bookmark {
        id: uuid::Uuid,
        label: String,
        node_id: NodeId,
    },
    Checkpoint {
        id: NodeId,
        label: String,
        node_id: NodeId,
    },
}

pub struct GraphReferenceResolver;

impl GraphReferenceResolver {
    pub fn bookmark_by_label(
        graph: &SessionGraph,
        label: &str,
        branch_id: Option<BranchId>,
    ) -> Result<GraphReference, String> {
        let mut matches: Vec<_> = graph
            .bookmarks
            .values()
            .filter(|bookmark| bookmark.label == label)
            .filter(|bookmark| {
                branch_id
                    .map(|branch| bookmark.branch_id == branch)
                    .unwrap_or(true)
            })
            .collect();
        matches.sort_by_key(|bookmark| bookmark.created_at);

        match matches.as_slice() {
            [bookmark] => Ok(GraphReference::Bookmark {
                id: bookmark.id,
                label: bookmark.label.clone(),
                node_id: bookmark.node_id,
            }),
            [] => Err(format!("Bookmark label '{}' not found", label)),
            _ => Err(format!(
                "Bookmark label '{}' is ambiguous; provide a branch id or bookmark id",
                label
            )),
        }
    }

    pub fn checkpoint_by_label(
        graph: &SessionGraph,
        label: &str,
        branch_id: Option<BranchId>,
    ) -> Result<GraphReference, String> {
        let mut matches: Vec<_> = graph
            .checkpoints
            .values()
            .filter(|checkpoint| checkpoint.label == label)
            .filter(|checkpoint| {
                branch_id
                    .map(|branch| checkpoint.branch_id == branch)
                    .unwrap_or(true)
            })
            .collect();
        matches.sort_by_key(|checkpoint| checkpoint.created_at);

        match matches.as_slice() {
            [checkpoint] => Ok(GraphReference::Checkpoint {
                id: checkpoint.id,
                label: checkpoint.label.clone(),
                node_id: checkpoint.id,
            }),
            [] => Err(format!("Checkpoint label '{}' not found", label)),
            _ => Err(format!(
                "Checkpoint label '{}' is ambiguous; provide a branch id or checkpoint id",
                label
            )),
        }
    }

    pub fn node_id(reference: &GraphReference) -> NodeId {
        match reference {
            GraphReference::Bookmark { node_id, .. } => *node_id,
            GraphReference::Checkpoint { node_id, .. } => *node_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{NodeKind, SessionGraph};

    #[test]
    fn resolves_bookmark_and_checkpoint_by_label() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        graph
            .create_bookmark(root, "start", None, None, None)
            .unwrap();
        graph
            .create_checkpoint(graph.primary_branch, "mark", None, vec![], None, None)
            .unwrap();

        let bookmark = GraphReferenceResolver::bookmark_by_label(&graph, "start", None).unwrap();
        let checkpoint = GraphReferenceResolver::checkpoint_by_label(&graph, "mark", None).unwrap();

        assert_eq!(GraphReferenceResolver::node_id(&bookmark), root);
        assert_eq!(
            GraphReferenceResolver::node_id(&checkpoint),
            graph.branch_head(graph.primary_branch).unwrap()
        );
    }
}
