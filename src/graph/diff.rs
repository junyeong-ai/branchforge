use serde::{Deserialize, Serialize};

use super::GraphError;
use super::SessionGraph;
use super::explorer::{BranchSummary, NodeSummary, node_summary_from_graph_node};
use super::types::{BranchId, NodeId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchDiffSummary {
    pub left: BranchSummary,
    pub right: BranchSummary,
    pub common_ancestor: Option<NodeId>,
    pub left_only_count: usize,
    pub right_only_count: usize,
    pub left_only_preview: Vec<NodeSummary>,
    pub right_only_preview: Vec<NodeSummary>,
}

pub struct GraphDiffService;

impl GraphDiffService {
    pub fn branch_diff(
        graph: &SessionGraph,
        left: BranchId,
        right: BranchId,
    ) -> Result<BranchDiffSummary, GraphError> {
        let branches = super::GraphExplorer::list_branches(graph);
        let left_summary = branches
            .iter()
            .find(|branch| branch.id == left)
            .cloned()
            .ok_or(GraphError::MissingBranch { branch_id: left })?;
        let right_summary = branches
            .iter()
            .find(|branch| branch.id == right)
            .cloned()
            .ok_or(GraphError::MissingBranch { branch_id: right })?;

        let left_nodes = graph.current_branch_nodes(left);
        let right_nodes = graph.current_branch_nodes(right);

        let mut common_ancestor = None;
        let mut shared_len = 0;
        for (index, (left_node, right_node)) in
            left_nodes.iter().zip(right_nodes.iter()).enumerate()
        {
            if left_node.id == right_node.id {
                common_ancestor = Some(left_node.id);
                shared_len = index + 1;
            } else {
                break;
            }
        }

        let left_only: Vec<_> = left_nodes.into_iter().skip(shared_len).collect();
        let right_only: Vec<_> = right_nodes.into_iter().skip(shared_len).collect();

        Ok(BranchDiffSummary {
            left: left_summary,
            right: right_summary,
            common_ancestor,
            left_only_count: left_only.len(),
            right_only_count: right_only.len(),
            left_only_preview: left_only
                .into_iter()
                .take(5)
                .cloned()
                .map(node_summary_from_graph_node)
                .collect(),
            right_only_preview: right_only
                .into_iter()
                .take(5)
                .cloned()
                .map(node_summary_from_graph_node)
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{NodeKind, SessionGraph};

    #[test]
    fn branch_diff_finds_common_ancestor_and_divergence() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(
                graph.primary_branch,
                NodeKind::User,
                serde_json::json!({"content": [{"type": "text", "text": "root"}]}),
            )
            .unwrap();
        graph
            .append_node(
                graph.primary_branch,
                NodeKind::Assistant,
                serde_json::json!({"content": [{"type": "text", "text": "left"}]}),
            )
            .unwrap();
        let right_branch = graph.fork_branch(Some(root), "right").unwrap();
        graph
            .append_node(
                right_branch,
                NodeKind::Assistant,
                serde_json::json!({"content": [{"type": "text", "text": "right"}]}),
            )
            .unwrap();

        let diff =
            GraphDiffService::branch_diff(&graph, graph.primary_branch, right_branch).unwrap();
        assert_eq!(diff.common_ancestor, Some(root));
        assert_eq!(diff.left_only_count, 1);
        assert_eq!(diff.right_only_count, 1);
    }

    #[test]
    fn branch_diff_errors_for_missing_branch() {
        let graph = SessionGraph::default();
        let missing = uuid::Uuid::new_v4();

        let error =
            GraphDiffService::branch_diff(&graph, graph.primary_branch, missing).unwrap_err();
        assert!(matches!(
            error,
            GraphError::MissingBranch { branch_id } if branch_id == missing
        ));
    }
}
