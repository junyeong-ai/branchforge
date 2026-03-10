use serde::{Deserialize, Serialize};

use super::SessionGraph;
use super::types::{BranchId, NodeId, NodeKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSummary {
    pub id: NodeId,
    pub branch_id: BranchId,
    pub kind: NodeKind,
    pub parent_id: Option<NodeId>,
    pub created_by_principal_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub tags: Vec<String>,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchSummary {
    pub id: BranchId,
    pub name: String,
    pub forked_from: Option<NodeId>,
    pub head: Option<NodeId>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub node_count: usize,
    pub checkpoint_count: usize,
    pub bookmark_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNodeSummary {
    pub node: NodeSummary,
    pub depth: usize,
    pub has_children: bool,
    pub has_checkpoint: bool,
    pub has_bookmark: bool,
}

pub struct GraphExplorer;

impl GraphExplorer {
    pub fn list_branches(graph: &SessionGraph) -> Vec<BranchSummary> {
        graph
            .branch_ids()
            .into_iter()
            .filter_map(|branch_id| {
                graph
                    .branches
                    .get(&branch_id)
                    .map(|branch| (branch_id, branch))
            })
            .map(|(branch_id, branch)| BranchSummary {
                id: branch_id,
                name: branch.name.clone(),
                forked_from: branch.forked_from,
                head: branch.head,
                created_at: branch.created_at,
                node_count: graph.branch_nodes(branch_id).len(),
                checkpoint_count: graph.checkpoints_for_branch(branch_id).len(),
                bookmark_count: graph.bookmarks_for_branch(branch_id).len(),
            })
            .collect()
    }

    pub fn tree_view(graph: &SessionGraph, branch_id: BranchId) -> Vec<TreeNodeSummary> {
        graph
            .current_branch_nodes(branch_id)
            .into_iter()
            .map(|node| TreeNodeSummary {
                node: summarize_node(node),
                depth: node_depth(graph, node.id),
                has_children: !graph.children_of(node.id).is_empty(),
                has_checkpoint: graph.checkpoints.contains_key(&node.id),
                has_bookmark: graph
                    .bookmarks
                    .values()
                    .any(|bookmark| bookmark.node_id == node.id),
            })
            .collect()
    }

    pub fn bookmarks(graph: &SessionGraph, branch_id: Option<BranchId>) -> Vec<super::Bookmark> {
        let mut bookmarks: Vec<_> = graph.bookmarks.values().cloned().collect();
        if let Some(branch_id) = branch_id {
            bookmarks.retain(|bookmark| bookmark.branch_id == branch_id);
        }
        bookmarks.sort_by_key(|bookmark| bookmark.created_at);
        bookmarks
    }

    pub fn checkpoints(
        graph: &SessionGraph,
        branch_id: Option<BranchId>,
    ) -> Vec<super::Checkpoint> {
        let mut checkpoints: Vec<_> = graph.checkpoints.values().cloned().collect();
        if let Some(branch_id) = branch_id {
            checkpoints.retain(|checkpoint| checkpoint.branch_id == branch_id);
        }
        checkpoints.sort_by_key(|checkpoint| checkpoint.created_at);
        checkpoints
    }

    pub fn node_summary(graph: &SessionGraph, node_id: NodeId) -> Option<NodeSummary> {
        graph.nodes.get(&node_id).map(summarize_node)
    }
}

pub(crate) fn node_summary_from_graph_node(node: super::GraphNode) -> NodeSummary {
    summarize_node(&node)
}

fn summarize_node(node: &super::GraphNode) -> NodeSummary {
    NodeSummary {
        id: node.id,
        branch_id: node.branch_id,
        kind: node.kind,
        parent_id: node.parent_id,
        created_by_principal_id: node.created_by_principal_id.clone(),
        created_at: node.created_at,
        tags: node.tags.clone(),
        preview: preview_payload(&node.payload),
    }
}

fn preview_payload(payload: &serde_json::Value) -> Option<String> {
    if let Some(content) = payload.get("content")
        && let Some(blocks) = content.as_array()
    {
        for block in blocks {
            if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                return Some(truncate_preview(text));
            }
        }
    }

    if let Some(summary) = payload.get("summary").and_then(serde_json::Value::as_str) {
        return Some(truncate_preview(summary));
    }

    if let Some(label) = payload.get("label").and_then(serde_json::Value::as_str) {
        return Some(truncate_preview(label));
    }

    None
}

fn truncate_preview(text: &str) -> String {
    let mut chars = text.chars();
    let preview: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        format!("{}...", preview)
    } else {
        preview
    }
}

fn node_depth(graph: &SessionGraph, node_id: NodeId) -> usize {
    let mut depth = 0;
    let mut current = graph.nodes.get(&node_id).and_then(|node| node.parent_id);
    while let Some(parent_id) = current {
        depth += 1;
        current = graph.nodes.get(&parent_id).and_then(|node| node.parent_id);
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{NodeKind, SessionGraph};

    #[test]
    fn explorer_lists_branches_and_tree() {
        let mut graph = SessionGraph::default();
        let root = graph.append_node(
            graph.primary_branch,
            NodeKind::User,
            serde_json::json!({"content": [{"type": "text", "text": "hello"}]}),
        );
        let branch = graph.fork_branch(Some(root), "experiment");
        graph.append_node(
            branch,
            NodeKind::Assistant,
            serde_json::json!({"content": [{"type": "text", "text": "world"}]}),
        );
        graph.create_checkpoint(branch, "milestone", None, vec![], None);
        graph.create_bookmark(root, "start", None, None);

        let branches = GraphExplorer::list_branches(&graph);
        let tree = GraphExplorer::tree_view(&graph, branch);

        assert_eq!(branches.len(), 2);
        assert!(!tree.is_empty());
        assert!(tree.iter().any(|node| node.has_checkpoint));
    }
}
