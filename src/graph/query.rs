use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::types::{BranchId, GraphNode, NodeId, NodeKind};

#[derive(Debug, Clone, Default)]
pub struct GraphFilter {
    pub branch_id: Option<BranchId>,
    pub kind: Option<NodeKind>,
    pub tag: Option<String>,
    pub since: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplaySegment {
    pub branch_id: BranchId,
    pub from_node: Option<NodeId>,
    pub node_ids: Vec<NodeId>,
}

impl GraphFilter {
    pub fn matches(&self, node: &GraphNode) -> bool {
        if let Some(branch_id) = self.branch_id
            && node.branch_id != branch_id
        {
            return false;
        }
        if let Some(kind) = self.kind
            && node.kind != kind
        {
            return false;
        }
        if let Some(ref tag) = self.tag
            && !node.tags.iter().any(|t| t == tag)
        {
            return false;
        }
        if let Some(since) = self.since
            && node.created_at < since
        {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, Default)]
pub struct GraphQuery {
    filter: GraphFilter,
}

impl GraphQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn filter(mut self, filter: GraphFilter) -> Self {
        self.filter = filter;
        self
    }

    pub fn run<'a>(&self, nodes: &'a [GraphNode]) -> Vec<&'a GraphNode> {
        nodes
            .iter()
            .filter(|node| self.filter.matches(node))
            .collect()
    }

    pub fn lineage<'a>(&self, nodes: &'a [GraphNode], head: NodeId) -> Vec<&'a GraphNode> {
        let index: std::collections::HashMap<NodeId, &GraphNode> =
            nodes.iter().map(|node| (node.id, node)).collect();
        let mut result = Vec::new();
        let mut current = Some(head);

        while let Some(node_id) = current {
            let Some(node) = index.get(&node_id).copied() else {
                break;
            };
            if self.filter.matches(node) {
                result.push(node);
            }
            current = node.parent_id;
        }

        result.reverse();
        result
    }
}
