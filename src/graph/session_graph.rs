use std::collections::HashMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::event::{GraphEvent, GraphEventBody};
use super::types::{
    Bookmark, Branch, BranchId, Checkpoint, GraphNode, NodeId, NodeKind, NodeProvenance,
    SessionGraphId,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionGraph {
    pub id: SessionGraphId,
    pub created_at: chrono::DateTime<Utc>,
    pub events: Vec<GraphEvent>,
    pub branches: HashMap<BranchId, Branch>,
    pub nodes: HashMap<NodeId, GraphNode>,
    pub checkpoints: HashMap<NodeId, Checkpoint>,
    pub bookmarks: HashMap<Uuid, Bookmark>,
    pub primary_branch: BranchId,
}

impl Default for SessionGraph {
    fn default() -> Self {
        Self::new("main")
    }
}

impl SessionGraph {
    pub fn new(primary_branch_name: impl Into<String>) -> Self {
        let branch_id = Uuid::new_v4();
        let now = Utc::now();
        let branch = Branch {
            id: branch_id,
            name: primary_branch_name.into(),
            forked_from: None,
            created_at: now,
            head: None,
        };

        Self {
            id: Uuid::new_v4(),
            created_at: now,
            events: Vec::new(),
            branches: [(branch_id, branch)].into_iter().collect(),
            nodes: HashMap::new(),
            checkpoints: HashMap::new(),
            bookmarks: HashMap::new(),
            primary_branch: branch_id,
        }
    }

    pub fn append_node(
        &mut self,
        branch_id: BranchId,
        kind: NodeKind,
        payload: serde_json::Value,
    ) -> NodeId {
        self.append_node_with_actor(branch_id, kind, payload, None, None)
    }

    pub fn append_node_with_actor(
        &mut self,
        branch_id: BranchId,
        kind: NodeKind,
        payload: serde_json::Value,
        created_by_principal_id: Option<String>,
        provenance: Option<NodeProvenance>,
    ) -> NodeId {
        let node_id = Uuid::new_v4();
        let parent_id = self.branches.get(&branch_id).and_then(|branch| branch.head);
        self.append_existing_node(
            branch_id,
            node_id,
            parent_id,
            kind,
            Vec::new(),
            payload,
            Utc::now(),
            created_by_principal_id,
            provenance,
        );
        node_id
    }

    pub fn append_existing_node(
        &mut self,
        branch_id: BranchId,
        node_id: NodeId,
        parent_id: Option<NodeId>,
        kind: NodeKind,
        tags: Vec<String>,
        payload: serde_json::Value,
        created_at: chrono::DateTime<Utc>,
        created_by_principal_id: Option<String>,
        provenance: Option<NodeProvenance>,
    ) {
        let node = GraphNode {
            id: node_id,
            branch_id,
            kind,
            parent_id,
            created_by_principal_id: created_by_principal_id.clone(),
            provenance,
            created_at,
            tags: tags.clone(),
            payload: payload.clone(),
        };
        self.nodes.insert(node_id, node);
        if let Some(branch) = self.branches.get_mut(&branch_id) {
            branch.head = Some(node_id);
        }
        self.events.push(GraphEvent {
            metadata: super::EventMetadata {
                id: Uuid::new_v4(),
                occurred_at: created_at,
                actor: created_by_principal_id,
            },
            body: GraphEventBody::NodeAppended {
                node_id,
                branch_id,
                parent_id,
                kind,
                tags,
                payload,
            },
        });
    }

    pub fn fork_branch(&mut self, from_node: Option<NodeId>, name: impl Into<String>) -> BranchId {
        let branch_id = Uuid::new_v4();
        let branch = Branch {
            id: branch_id,
            name: name.into(),
            forked_from: from_node,
            created_at: Utc::now(),
            head: from_node,
        };
        self.branches.insert(branch_id, branch.clone());
        self.events
            .push(GraphEvent::new(GraphEventBody::BranchForked {
                branch_id,
                name: branch.name,
                forked_from: branch.forked_from,
            }));
        branch_id
    }

    pub fn create_checkpoint(
        &mut self,
        branch_id: BranchId,
        label: impl Into<String>,
        note: Option<String>,
        tags: Vec<String>,
        created_by_principal_id: Option<String>,
        provenance: Option<NodeProvenance>,
    ) -> NodeId {
        let checkpoint_id = Uuid::new_v4();
        let checkpoint = Checkpoint {
            id: checkpoint_id,
            branch_id,
            label: label.into(),
            note: note.clone(),
            tags: tags.clone(),
            created_by_principal_id: created_by_principal_id.clone(),
            provenance: provenance.clone(),
            created_at: Utc::now(),
        };
        self.checkpoints.insert(checkpoint_id, checkpoint.clone());
        self.nodes.insert(
            checkpoint_id,
            GraphNode {
                id: checkpoint_id,
                branch_id,
                kind: NodeKind::Checkpoint,
                parent_id: self.branches.get(&branch_id).and_then(|branch| branch.head),
                created_by_principal_id,
                provenance,
                created_at: checkpoint.created_at,
                tags: checkpoint.tags.clone(),
                payload: serde_json::json!({
                    "label": checkpoint.label,
                    "note": checkpoint.note,
                }),
            },
        );
        if let Some(branch) = self.branches.get_mut(&branch_id) {
            branch.head = Some(checkpoint_id);
        }
        self.events
            .push(GraphEvent::new(GraphEventBody::CheckpointCreated {
                checkpoint_id,
                branch_id,
                label: self
                    .checkpoints
                    .get(&checkpoint_id)
                    .map(|c| c.label.clone())
                    .unwrap_or_default(),
                note,
                tags,
            }));
        checkpoint_id
    }

    pub fn branch_head(&self, branch_id: BranchId) -> Option<NodeId> {
        self.branches.get(&branch_id).and_then(|branch| branch.head)
    }

    pub fn branch_ids(&self) -> Vec<BranchId> {
        let mut branch_ids: Vec<_> = self.branches.keys().copied().collect();
        branch_ids.sort_by_key(|branch_id| self.branches.get(branch_id).map(|b| b.created_at));
        branch_ids
    }

    pub fn children_of(&self, node_id: NodeId) -> Vec<&GraphNode> {
        let mut children: Vec<_> = self
            .nodes
            .values()
            .filter(|node| node.parent_id == Some(node_id))
            .collect();
        children.sort_by_key(|node| node.created_at);
        children
    }

    pub fn branch_at(&self, node_id: NodeId) -> Option<BranchId> {
        self.nodes.get(&node_id).map(|node| node.branch_id)
    }

    pub fn checkpoints_for_branch(&self, branch_id: BranchId) -> Vec<&Checkpoint> {
        let mut checkpoints: Vec<_> = self
            .checkpoints
            .values()
            .filter(|checkpoint| checkpoint.branch_id == branch_id)
            .collect();
        checkpoints.sort_by_key(|checkpoint| checkpoint.created_at);
        checkpoints
    }

    pub fn create_bookmark(
        &mut self,
        node_id: NodeId,
        label: impl Into<String>,
        note: Option<String>,
        created_by_principal_id: Option<String>,
        provenance: Option<NodeProvenance>,
    ) -> Option<Uuid> {
        let node = self.nodes.get(&node_id)?;
        let bookmark_id = Uuid::new_v4();
        self.bookmarks.insert(
            bookmark_id,
            Bookmark {
                id: bookmark_id,
                node_id,
                branch_id: node.branch_id,
                label: label.into(),
                note,
                created_by_principal_id,
                provenance,
                created_at: Utc::now(),
            },
        );
        Some(bookmark_id)
    }

    pub fn bookmarks_for_branch(&self, branch_id: BranchId) -> Vec<&Bookmark> {
        let mut bookmarks: Vec<_> = self
            .bookmarks
            .values()
            .filter(|bookmark| bookmark.branch_id == branch_id)
            .collect();
        bookmarks.sort_by_key(|bookmark| bookmark.created_at);
        bookmarks
    }

    pub fn replay_slice(&self, from: Option<NodeId>, branch_id: BranchId) -> Vec<&GraphNode> {
        let branch = self.current_branch_nodes(branch_id);
        match from {
            Some(from_id) => branch
                .into_iter()
                .skip_while(|node| node.id != from_id)
                .collect(),
            None => branch,
        }
    }

    pub fn branch_nodes(&self, branch_id: BranchId) -> Vec<&GraphNode> {
        let mut nodes: Vec<&GraphNode> = self
            .nodes
            .values()
            .filter(|node| node.branch_id == branch_id)
            .collect();
        nodes.sort_by_key(|node| node.created_at);
        nodes
    }

    pub fn current_branch_nodes(&self, branch_id: BranchId) -> Vec<&GraphNode> {
        let Some(head) = self.branch_head(branch_id) else {
            return Vec::new();
        };

        let mut nodes = Vec::new();
        let mut current = Some(head);
        while let Some(node_id) = current {
            let Some(node) = self.nodes.get(&node_id) else {
                break;
            };
            nodes.push(node);
            current = node.parent_id;
        }
        nodes.reverse();
        nodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_nodes_to_branch_head() {
        let mut graph = SessionGraph::new("main");
        let branch = graph.primary_branch;
        let first = graph.append_node(branch, NodeKind::User, serde_json::json!({ "text": "hi" }));
        let second = graph.append_node(
            branch,
            NodeKind::Assistant,
            serde_json::json!({ "text": "hello" }),
        );

        assert_eq!(graph.branch_head(branch), Some(second));
        assert_eq!(
            graph.nodes.get(&second).and_then(|node| node.parent_id),
            Some(first)
        );
    }

    #[test]
    fn forks_new_branch_from_existing_node() {
        let mut graph = SessionGraph::default();
        let root = graph.append_node(
            graph.primary_branch,
            NodeKind::User,
            serde_json::json!({ "text": "root" }),
        );

        let branch = graph.fork_branch(Some(root), "experiment");

        assert_eq!(graph.branch_head(branch), Some(root));
    }

    #[test]
    fn lists_children_and_checkpoints() {
        let mut graph = SessionGraph::default();
        let root = graph.append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}));
        let branch = graph.fork_branch(Some(root), "alt");
        let child = graph.append_node(branch, NodeKind::Assistant, serde_json::json!({}));
        graph.create_checkpoint(
            branch,
            "milestone",
            None,
            vec!["tag".to_string()],
            None,
            None,
        );

        assert_eq!(graph.children_of(root).len(), 1);
        assert_eq!(graph.branch_at(child), Some(branch));
        assert_eq!(graph.checkpoints_for_branch(branch).len(), 1);
    }

    #[test]
    fn creates_branch_bookmark() {
        let mut graph = SessionGraph::default();
        let node = graph.append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}));
        let bookmark = graph.create_bookmark(node, "start", Some("entry".to_string()), None, None);

        assert!(bookmark.is_some());
        assert_eq!(graph.bookmarks_for_branch(graph.primary_branch).len(), 1);
    }
}
