use serde::{Deserialize, Serialize};

use super::SessionGraph;
use super::explorer::NodeSummary;
use super::types::{BranchId, NodeKind};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphSearchQuery {
    pub text: Option<String>,
    pub branch_id: Option<BranchId>,
    pub kind: Option<NodeKind>,
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphSessionStats {
    pub branch_count: usize,
    pub node_count: usize,
    pub bookmark_count: usize,
    pub checkpoint_count: usize,
    pub tool_call_count: usize,
    pub tool_result_count: usize,
    pub summary_count: usize,
}

pub struct GraphSearchService;

impl GraphSearchService {
    pub fn search(graph: &SessionGraph, query: &GraphSearchQuery) -> Vec<NodeSummary> {
        let mut nodes: Vec<_> = graph
            .nodes
            .values()
            .filter(|node| match query.branch_id {
                Some(branch_id) => node.branch_id == branch_id,
                None => true,
            })
            .filter(|node| match query.kind {
                Some(kind) => node.kind == kind,
                None => true,
            })
            .filter(|node| match query.tag.as_ref() {
                Some(tag) => node.tags.iter().any(|node_tag| node_tag == tag),
                None => true,
            })
            .filter(|node| match query.text.as_ref() {
                Some(text) => payload_contains_text(&node.payload, text),
                None => true,
            })
            .cloned()
            .collect();
        nodes.sort_by_key(|node| node.created_at);
        nodes
            .into_iter()
            .map(super::explorer::node_summary_from_graph_node)
            .collect()
    }

    pub fn stats(graph: &SessionGraph) -> GraphSessionStats {
        GraphSessionStats {
            branch_count: graph.branches.len(),
            node_count: graph.nodes.len(),
            bookmark_count: graph.bookmarks.len(),
            checkpoint_count: graph.checkpoints.len(),
            tool_call_count: graph
                .nodes
                .values()
                .filter(|node| node.kind == NodeKind::ToolCall)
                .count(),
            tool_result_count: graph
                .nodes
                .values()
                .filter(|node| node.kind == NodeKind::ToolResult)
                .count(),
            summary_count: graph
                .nodes
                .values()
                .filter(|node| node.kind == NodeKind::Summary)
                .count(),
        }
    }
}

fn payload_contains_text(payload: &serde_json::Value, query: &str) -> bool {
    payload
        .to_string()
        .to_lowercase()
        .contains(&query.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{NodeKind, SessionGraph};

    #[test]
    fn search_finds_matching_nodes_and_stats() {
        let mut graph = SessionGraph::default();
        graph.append_node(
            graph.primary_branch,
            NodeKind::User,
            serde_json::json!({"content": [{"type": "text", "text": "alpha"}]}),
        );
        graph.append_node(
            graph.primary_branch,
            NodeKind::ToolCall,
            serde_json::json!({"tool_name": "Read"}),
        );

        let results = GraphSearchService::search(
            &graph,
            &GraphSearchQuery {
                text: Some("alpha".to_string()),
                ..Default::default()
            },
        );
        let stats = GraphSearchService::stats(&graph);

        assert_eq!(results.len(), 1);
        assert_eq!(stats.tool_call_count, 1);
        assert_eq!(stats.node_count, 2);
    }
}
