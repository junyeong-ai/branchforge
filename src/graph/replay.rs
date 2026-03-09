use serde::{Deserialize, Serialize};

use super::SessionGraph;
use super::query::{GraphQuery, ReplaySegment};
use super::types::{BranchId, NodeId, NodeKind};
use crate::types::{ContentBlock, Message, Role};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayInput {
    pub branch_id: BranchId,
    pub from_node: Option<NodeId>,
    pub messages: Vec<Message>,
}

impl SessionGraph {
    pub fn replay_segment(&self, branch_id: BranchId, from_node: Option<NodeId>) -> ReplaySegment {
        let all_nodes: Vec<_> = self.nodes.values().cloned().collect();
        GraphQuery::new().replay_segment(&all_nodes, branch_id, from_node)
    }

    pub fn replay_input(&self, branch_id: BranchId, from_node: Option<NodeId>) -> ReplayInput {
        let segment = self.replay_segment(branch_id, from_node);
        let messages = segment
            .node_ids
            .iter()
            .filter_map(|node_id| self.nodes.get(node_id))
            .filter_map(node_to_message)
            .collect();

        ReplayInput {
            branch_id,
            from_node,
            messages,
        }
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
