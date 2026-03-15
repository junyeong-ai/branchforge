use serde::{Deserialize, Serialize};

use super::GraphError;
use super::SessionGraph;
use super::query::ReplaySegment;
use super::types::{BranchId, NodeId, NodeKind};
use crate::types::{ContentBlock, Message, Role};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayInput {
    pub branch_id: BranchId,
    pub from_node: Option<NodeId>,
    pub messages: Vec<Message>,
}

impl SessionGraph {
    pub fn replay_segment(
        &self,
        branch_id: BranchId,
        from_node: Option<NodeId>,
    ) -> Result<ReplaySegment, GraphError> {
        let node_ids = self.branch_lineage_node_ids(branch_id)?;
        let from_index = match from_node {
            Some(from_id) => node_ids
                .iter()
                .position(|node_id| *node_id == from_id)
                .ok_or(GraphError::ReplayStartOutsideBranch {
                    node_id: from_id,
                    branch_id,
                })?,
            None => 0,
        };

        Ok(ReplaySegment {
            branch_id,
            from_node,
            node_ids: node_ids.into_iter().skip(from_index).collect(),
        })
    }

    pub fn replay_input(
        &self,
        branch_id: BranchId,
        from_node: Option<NodeId>,
    ) -> Result<ReplayInput, GraphError> {
        let segment = self.replay_segment(branch_id, from_node)?;
        let messages = segment
            .node_ids
            .iter()
            .filter_map(|node_id| self.nodes.get(node_id))
            .filter_map(node_to_message)
            .collect();

        Ok(ReplayInput {
            branch_id,
            from_node,
            messages,
        })
    }
}

fn node_to_message(node: &super::types::GraphNode) -> Option<Message> {
    let role = match node.kind {
        NodeKind::User => Role::User,
        NodeKind::Assistant | NodeKind::Summary => Role::Assistant,
        _ => return None,
    };

    let content: Vec<ContentBlock> =
        serde_json::from_value(node.payload.get("content")?.clone()).ok()?;
    Some(Message { role, content })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::NodeKind;

    #[test]
    fn replay_includes_ancestor_nodes_for_forked_branch() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(
                graph.primary_branch,
                NodeKind::User,
                serde_json::json!({"content": [{"type": "text", "text": "root"}]}),
            )
            .unwrap();
        let branch = graph.fork_branch(Some(root), "fork").unwrap();
        graph
            .append_node(
                branch,
                NodeKind::Assistant,
                serde_json::json!({"content": [{"type": "text", "text": "fork"}]}),
            )
            .unwrap();

        let replay = graph.replay_input(branch, None).unwrap();
        assert_eq!(replay.messages.len(), 2);
        assert_eq!(replay.messages[0].role, Role::User);
        assert_eq!(replay.messages[1].role, Role::Assistant);
    }

    #[test]
    fn replay_rejects_start_node_outside_branch_lineage() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(
                graph.primary_branch,
                NodeKind::User,
                serde_json::json!({"content": [{"type": "text", "text": "root"}]}),
            )
            .unwrap();
        let primary_head = graph
            .append_node(
                graph.primary_branch,
                NodeKind::Assistant,
                serde_json::json!({"content": [{"type": "text", "text": "left"}]}),
            )
            .unwrap();
        let branch = graph.fork_branch(Some(root), "fork").unwrap();

        let error = graph.replay_input(branch, Some(primary_head)).unwrap_err();
        assert!(matches!(error, GraphError::ReplayStartOutsideBranch { .. }));
    }
}
