use std::collections::HashMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::GraphError;
use super::event::{EventMetadata, GraphEvent, GraphEventBody};
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

impl SessionGraph {
    /// Walk up the parent chain from a node to compute its depth in the tree.
    pub fn node_depth(&self, node_id: NodeId) -> usize {
        let mut depth = 0;
        let mut current = self.nodes.get(&node_id).and_then(|node| node.parent_id);
        while let Some(parent_id) = current {
            depth += 1;
            current = self.nodes.get(&parent_id).and_then(|node| node.parent_id);
        }
        depth
    }
}

impl Default for SessionGraph {
    fn default() -> Self {
        Self::new("main")
    }
}

impl SessionGraph {
    pub(crate) fn branch_lineage_node_ids(
        &self,
        branch_id: BranchId,
    ) -> Result<Vec<NodeId>, GraphError> {
        let branch = self
            .branches
            .get(&branch_id)
            .ok_or(GraphError::MissingBranch { branch_id })?;
        let Some(head) = branch.head else {
            return Ok(Vec::new());
        };

        let mut nodes = Vec::new();
        let mut current = Some(head);
        while let Some(node_id) = current {
            let node = self
                .nodes
                .get(&node_id)
                .ok_or(GraphError::MissingNode { node_id })?;
            nodes.push(node_id);
            current = node.parent_id;
        }
        nodes.reverse();
        Ok(nodes)
    }

    fn validated_branch_head(
        &self,
        branch_id: BranchId,
        node_id: NodeId,
    ) -> Result<Option<NodeId>, GraphError> {
        let branch = self
            .branches
            .get(&branch_id)
            .ok_or(GraphError::MissingBranch { branch_id })?;
        let Some(parent_id) = branch.head else {
            return Ok(None);
        };
        let parent = self
            .nodes
            .get(&parent_id)
            .ok_or(GraphError::MissingParent { node_id, parent_id })?;
        if parent.branch_id != branch_id && branch.forked_from != Some(parent_id) {
            return Err(GraphError::ParentBranchMismatch {
                node_id,
                branch_id,
                parent_id,
                parent_branch_id: parent.branch_id,
            });
        }
        Ok(Some(parent_id))
    }

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
    ) -> Result<NodeId, GraphError> {
        self.append_node_with_actor(branch_id, kind, payload, None, None)
    }

    pub fn append_node_with_actor(
        &mut self,
        branch_id: BranchId,
        kind: NodeKind,
        payload: serde_json::Value,
        created_by_principal_id: Option<String>,
        provenance: Option<NodeProvenance>,
    ) -> Result<NodeId, GraphError> {
        let node_id = Uuid::new_v4();
        let parent_id = self.validated_branch_head(branch_id, node_id)?;
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
        )?;
        Ok(node_id)
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
    ) -> Result<(), GraphError> {
        let branch = self
            .branches
            .get(&branch_id)
            .ok_or(GraphError::MissingBranch { branch_id })?;
        if self.nodes.contains_key(&node_id) {
            return Err(GraphError::DuplicateNode { node_id });
        }
        if let Some(parent_id) = parent_id {
            let parent = self
                .nodes
                .get(&parent_id)
                .ok_or(GraphError::MissingParent { node_id, parent_id })?;
            if parent.branch_id != branch_id && branch.forked_from != Some(parent_id) {
                return Err(GraphError::ParentBranchMismatch {
                    node_id,
                    branch_id,
                    parent_id,
                    parent_branch_id: parent.branch_id,
                });
            }
        }
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
                provenance: self
                    .nodes
                    .get(&node_id)
                    .and_then(|node| node.provenance.clone()),
            },
        });
        Ok(())
    }

    pub fn patch_node_metadata(
        &mut self,
        node_id: NodeId,
        metadata: serde_json::Value,
        actor: Option<String>,
    ) -> bool {
        let Some(node) = self.nodes.get_mut(&node_id) else {
            return false;
        };

        if let Some(payload) = node.payload.as_object_mut() {
            payload.insert("metadata".to_string(), metadata.clone());
        } else {
            node.payload = serde_json::json!({ "metadata": metadata.clone() });
        }

        self.events.push(GraphEvent {
            metadata: super::EventMetadata::new(actor),
            body: GraphEventBody::NodeMetadataPatched { node_id, metadata },
        });

        true
    }

    pub fn fork_branch(
        &mut self,
        from_node: Option<NodeId>,
        name: impl Into<String>,
    ) -> Result<BranchId, GraphError> {
        if let Some(node_id) = from_node
            && !self.nodes.contains_key(&node_id)
        {
            return Err(GraphError::MissingForkSource { node_id });
        }
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
        Ok(branch_id)
    }

    pub fn create_checkpoint(
        &mut self,
        branch_id: BranchId,
        label: impl Into<String>,
        note: Option<String>,
        tags: Vec<String>,
        created_by_principal_id: Option<String>,
        provenance: Option<NodeProvenance>,
    ) -> Result<NodeId, GraphError> {
        if !self.branches.contains_key(&branch_id) {
            return Err(GraphError::MissingBranch { branch_id });
        }
        let checkpoint_id = Uuid::new_v4();
        let parent_id = self.validated_branch_head(branch_id, checkpoint_id)?;
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
                parent_id,
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
        self.events.push(GraphEvent::with_metadata(
            EventMetadata {
                id: Uuid::new_v4(),
                occurred_at: checkpoint.created_at,
                actor: self
                    .checkpoints
                    .get(&checkpoint_id)
                    .and_then(|checkpoint| checkpoint.created_by_principal_id.clone()),
            },
            GraphEventBody::CheckpointCreated {
                checkpoint_id,
                branch_id,
                label: self
                    .checkpoints
                    .get(&checkpoint_id)
                    .map(|c| c.label.clone())
                    .unwrap_or_default(),
                note,
                tags,
                provenance: self
                    .checkpoints
                    .get(&checkpoint_id)
                    .and_then(|checkpoint| checkpoint.provenance.clone()),
            },
        ));
        Ok(checkpoint_id)
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
    ) -> Result<Uuid, GraphError> {
        let node = self
            .nodes
            .get(&node_id)
            .ok_or(GraphError::MissingBookmarkTarget { node_id })?;
        if !self.branches.contains_key(&node.branch_id) {
            return Err(GraphError::MissingBranch {
                branch_id: node.branch_id,
            });
        }
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
        if let Some(bookmark) = self.bookmarks.get(&bookmark_id) {
            self.events.push(GraphEvent::with_metadata(
                EventMetadata {
                    id: Uuid::new_v4(),
                    occurred_at: bookmark.created_at,
                    actor: bookmark.created_by_principal_id.clone(),
                },
                GraphEventBody::BookmarkCreated {
                    bookmark_id,
                    node_id,
                    branch_id: bookmark.branch_id,
                    label: bookmark.label.clone(),
                    note: bookmark.note.clone(),
                    provenance: bookmark.provenance.clone(),
                },
            ));
        }
        Ok(bookmark_id)
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
        let Ok(branch_ids) = self.branch_lineage_node_ids(branch_id) else {
            return Vec::new();
        };
        let start_index = match from {
            Some(from_id) => branch_ids
                .iter()
                .position(|node_id| *node_id == from_id)
                .unwrap_or(branch_ids.len()),
            None => 0,
        };

        branch_ids
            .into_iter()
            .skip(start_index)
            .filter_map(|node_id| self.nodes.get(&node_id))
            .collect()
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
        self.branch_lineage_node_ids(branch_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|node_id| self.nodes.get(&node_id))
            .collect()
    }

    pub fn latest_summary(&self) -> Option<String> {
        self.current_branch_nodes(self.primary_branch)
            .into_iter()
            .rev()
            .find(|node| node.kind == NodeKind::Summary)
            .and_then(|node| {
                node.payload
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_nodes_to_branch_head() {
        let mut graph = SessionGraph::new("main");
        let branch = graph.primary_branch;
        let first = graph
            .append_node(branch, NodeKind::User, serde_json::json!({ "text": "hi" }))
            .unwrap();
        let second = graph
            .append_node(
                branch,
                NodeKind::Assistant,
                serde_json::json!({ "text": "hello" }),
            )
            .unwrap();

        assert_eq!(graph.branch_head(branch), Some(second));
        assert_eq!(
            graph.nodes.get(&second).and_then(|node| node.parent_id),
            Some(first)
        );
    }

    #[test]
    fn forks_new_branch_from_existing_node() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(
                graph.primary_branch,
                NodeKind::User,
                serde_json::json!({ "text": "root" }),
            )
            .unwrap();

        let branch = graph.fork_branch(Some(root), "experiment").unwrap();

        assert_eq!(graph.branch_head(branch), Some(root));
    }

    #[test]
    fn lists_children_and_checkpoints() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let branch = graph.fork_branch(Some(root), "alt").unwrap();
        let child = graph
            .append_node(branch, NodeKind::Assistant, serde_json::json!({}))
            .unwrap();
        graph
            .create_checkpoint(
                branch,
                "milestone",
                None,
                vec!["tag".to_string()],
                None,
                None,
            )
            .unwrap();

        assert_eq!(graph.children_of(root).len(), 1);
        assert_eq!(graph.branch_at(child), Some(branch));
        assert_eq!(graph.checkpoints_for_branch(branch).len(), 1);
    }

    #[test]
    fn creates_branch_bookmark() {
        let mut graph = SessionGraph::default();
        let node = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let bookmark = graph
            .create_bookmark(node, "start", Some("entry".to_string()), None, None)
            .unwrap();

        assert_ne!(bookmark, Uuid::nil());
        assert_eq!(graph.bookmarks_for_branch(graph.primary_branch).len(), 1);
    }

    #[test]
    fn extracts_latest_summary_from_primary_branch() {
        let mut graph = SessionGraph::default();
        let branch = graph.primary_branch;
        graph
            .append_node(branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        graph
            .append_node(
                branch,
                NodeKind::Summary,
                serde_json::json!({
                    "summary": "old",
                    "content": [],
                }),
            )
            .unwrap();
        graph
            .append_node(
                branch,
                NodeKind::Summary,
                serde_json::json!({
                    "summary": "new",
                    "content": [],
                }),
            )
            .unwrap();

        assert_eq!(graph.latest_summary().as_deref(), Some("new"));
    }

    #[test]
    fn checkpoint_and_bookmark_events_preserve_actor_metadata() {
        let mut graph = SessionGraph::default();
        let node = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let provenance = NodeProvenance {
            source_session_id: Uuid::new_v4().to_string(),
            session_type: "main".to_string(),
            task_id: None,
            subagent_session_id: None,
            subagent_type: None,
            subagent_description: None,
        };

        let checkpoint_id = graph
            .create_checkpoint(
                graph.primary_branch,
                "milestone",
                None,
                vec![],
                Some("user-1".to_string()),
                Some(provenance.clone()),
            )
            .unwrap();
        let bookmark_id = graph
            .create_bookmark(
                node,
                "start",
                None,
                Some("user-1".to_string()),
                Some(provenance),
            )
            .expect("bookmark should exist");

        let restored = crate::graph::GraphMaterializer::from_events(&graph.events);
        assert_eq!(
            restored
                .checkpoints
                .get(&checkpoint_id)
                .and_then(|checkpoint| checkpoint.created_by_principal_id.as_deref()),
            Some("user-1")
        );
        assert_eq!(
            restored
                .bookmarks
                .get(&bookmark_id)
                .and_then(|bookmark| bookmark.created_by_principal_id.as_deref()),
            Some("user-1")
        );
    }

    #[test]
    fn rejects_missing_branch_and_parent_mutations() {
        let mut graph = SessionGraph::default();
        let missing_branch = Uuid::new_v4();
        let missing_parent = Uuid::new_v4();

        assert!(matches!(
            graph.append_node(missing_branch, NodeKind::User, serde_json::json!({})),
            Err(GraphError::MissingBranch { .. })
        ));
        assert!(matches!(
            graph.append_existing_node(
                graph.primary_branch,
                Uuid::new_v4(),
                Some(missing_parent),
                NodeKind::User,
                Vec::new(),
                serde_json::json!({}),
                Utc::now(),
                None,
                None,
            ),
            Err(GraphError::MissingParent { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_nodes_and_cross_branch_parents() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let main_follow_up = graph
            .append_node(
                graph.primary_branch,
                NodeKind::Assistant,
                serde_json::json!({}),
            )
            .unwrap();
        let side = graph.fork_branch(Some(root), "side").unwrap();

        assert!(matches!(
            graph.append_existing_node(
                graph.primary_branch,
                root,
                Some(root),
                NodeKind::Assistant,
                Vec::new(),
                serde_json::json!({}),
                Utc::now(),
                None,
                None,
            ),
            Err(GraphError::DuplicateNode { node_id }) if node_id == root
        ));

        assert!(matches!(
            graph.append_existing_node(
                side,
                Uuid::new_v4(),
                Some(main_follow_up),
                NodeKind::Assistant,
                Vec::new(),
                serde_json::json!({}),
                Utc::now(),
                None,
                None,
            ),
            Err(GraphError::ParentBranchMismatch { branch_id, parent_id, .. })
                if branch_id == side && parent_id == main_follow_up
        ));
    }
}
