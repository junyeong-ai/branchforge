use std::collections::HashMap;

use super::event::GraphEventBody;
use super::session_graph::SessionGraph;
use super::types::{Bookmark, Branch, Checkpoint, GraphNode, NodeKind};

#[derive(Debug, Default)]
pub struct GraphMaterializer;

impl GraphMaterializer {
    /// Materialize a graph from events with an explicit primary branch ID.
    ///
    /// Persistence backends should always use this method when restoring,
    /// passing the stored primary_branch_id to avoid event-order dependence.
    pub fn from_events_with_primary(
        events: &[super::GraphEvent],
        primary_branch_id: Option<super::types::BranchId>,
    ) -> SessionGraph {
        let mut graph = Self::from_events(events);
        if let Some(branch_id) = primary_branch_id
            && graph.branches.contains_key(&branch_id)
        {
            graph.primary_branch = branch_id;
        }
        graph
    }

    pub fn from_events(events: &[super::GraphEvent]) -> SessionGraph {
        let mut graph = SessionGraph::default();
        graph.events.clear();
        graph.nodes = HashMap::new();
        graph.checkpoints = HashMap::new();
        graph.bookmarks = HashMap::new();

        let fallback_primary = graph.primary_branch;
        graph.branches = HashMap::new();

        for event in events {
            match &event.body {
                GraphEventBody::NodeAppended {
                    node_id,
                    branch_id,
                    parent_id,
                    kind,
                    tags,
                    payload,
                    provenance,
                } => {
                    if graph.branches.is_empty() {
                        graph.primary_branch = *branch_id;
                    }
                    graph.branches.entry(*branch_id).or_insert_with(|| Branch {
                        id: *branch_id,
                        name: if *branch_id == graph.primary_branch {
                            "main".to_string()
                        } else {
                            format!("branch-{}", branch_id)
                        },
                        forked_from: None,
                        created_at: event.metadata.occurred_at,
                        head: None,
                    });
                    graph.nodes.insert(
                        *node_id,
                        GraphNode {
                            id: *node_id,
                            branch_id: *branch_id,
                            kind: *kind,
                            parent_id: *parent_id,
                            created_by_principal_id: event.metadata.actor.clone(),
                            provenance: provenance.clone(),
                            created_at: event.metadata.occurred_at,
                            tags: tags.clone(),
                            payload: payload.clone(),
                        },
                    );
                    if let Some(branch) = graph.branches.get_mut(branch_id) {
                        branch.head = Some(*node_id);
                    }
                }
                GraphEventBody::NodeMetadataPatched { node_id, metadata } => {
                    if let Some(node) = graph.nodes.get_mut(node_id) {
                        if let Some(payload) = node.payload.as_object_mut() {
                            payload.insert("metadata".to_string(), metadata.clone());
                        } else {
                            node.payload = serde_json::json!({ "metadata": metadata });
                        }
                    }
                }
                GraphEventBody::BranchForked {
                    branch_id,
                    name,
                    forked_from,
                } => {
                    if graph.branches.is_empty() {
                        graph.primary_branch = *branch_id;
                    }
                    graph.branches.insert(
                        *branch_id,
                        Branch {
                            id: *branch_id,
                            name: name.clone(),
                            forked_from: *forked_from,
                            created_at: event.metadata.occurred_at,
                            head: *forked_from,
                        },
                    );
                }
                GraphEventBody::CheckpointCreated {
                    checkpoint_id,
                    branch_id,
                    label,
                    note,
                    tags,
                    provenance,
                } => {
                    if graph.branches.is_empty() {
                        graph.primary_branch = *branch_id;
                    }
                    graph.branches.entry(*branch_id).or_insert_with(|| Branch {
                        id: *branch_id,
                        name: if *branch_id == graph.primary_branch {
                            "main".to_string()
                        } else {
                            format!("branch-{}", branch_id)
                        },
                        forked_from: None,
                        created_at: event.metadata.occurred_at,
                        head: None,
                    });
                    graph.checkpoints.insert(
                        *checkpoint_id,
                        Checkpoint {
                            id: *checkpoint_id,
                            branch_id: *branch_id,
                            label: label.clone(),
                            note: note.clone(),
                            tags: tags.clone(),
                            created_by_principal_id: event.metadata.actor.clone(),
                            provenance: provenance.clone(),
                            created_at: event.metadata.occurred_at,
                        },
                    );
                    graph.nodes.insert(
                        *checkpoint_id,
                        GraphNode {
                            id: *checkpoint_id,
                            branch_id: *branch_id,
                            kind: NodeKind::Checkpoint,
                            parent_id: graph.branches.get(branch_id).and_then(|branch| branch.head),
                            created_by_principal_id: event.metadata.actor.clone(),
                            provenance: provenance.clone(),
                            created_at: event.metadata.occurred_at,
                            tags: tags.clone(),
                            payload: serde_json::json!({
                                "label": label,
                                "note": note,
                            }),
                        },
                    );
                    if let Some(branch) = graph.branches.get_mut(branch_id) {
                        branch.head = Some(*checkpoint_id);
                    }
                }
                GraphEventBody::BookmarkCreated {
                    bookmark_id,
                    node_id,
                    branch_id,
                    label,
                    note,
                    provenance,
                } => {
                    if graph.branches.is_empty() {
                        graph.primary_branch = *branch_id;
                    }
                    graph.branches.entry(*branch_id).or_insert_with(|| Branch {
                        id: *branch_id,
                        name: if *branch_id == graph.primary_branch {
                            "main".to_string()
                        } else {
                            format!("branch-{}", branch_id)
                        },
                        forked_from: None,
                        created_at: event.metadata.occurred_at,
                        head: None,
                    });
                    graph.bookmarks.insert(
                        *bookmark_id,
                        Bookmark {
                            id: *bookmark_id,
                            node_id: *node_id,
                            branch_id: *branch_id,
                            label: label.clone(),
                            note: note.clone(),
                            created_by_principal_id: event.metadata.actor.clone(),
                            provenance: provenance.clone(),
                            created_at: event.metadata.occurred_at,
                        },
                    );
                }
            }
            graph.events.push(event.clone());
        }

        if graph.branches.is_empty() {
            graph.primary_branch = fallback_primary;
            graph.branches.insert(
                fallback_primary,
                Branch {
                    id: fallback_primary,
                    name: "main".to_string(),
                    forked_from: None,
                    created_at: graph.created_at,
                    head: None,
                },
            );
        }

        if graph
            .branches
            .get(&graph.primary_branch)
            .and_then(|branch| branch.head)
            .is_none()
            && let Some((branch_id, _)) = graph
                .branches
                .iter()
                .find(|(_, branch)| branch.head.is_some())
        {
            graph.primary_branch = *branch_id;
        }

        graph
    }

    pub fn empty() -> SessionGraph {
        SessionGraph::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphEvent, NodeKind};
    use chrono::Utc;

    #[test]
    fn rebuilds_graph_from_events() {
        let branch_id = uuid::Uuid::new_v4();
        let node_id = uuid::Uuid::new_v4();
        let graph = GraphMaterializer::from_events(&[
            GraphEvent {
                metadata: crate::graph::EventMetadata::new(None),
                body: GraphEventBody::BranchForked {
                    branch_id,
                    name: "exp".to_string(),
                    forked_from: None,
                },
            },
            GraphEvent {
                metadata: crate::graph::EventMetadata {
                    id: uuid::Uuid::new_v4(),
                    occurred_at: Utc::now(),
                    actor: None,
                },
                body: GraphEventBody::NodeAppended {
                    node_id,
                    branch_id,
                    parent_id: None,
                    kind: NodeKind::User,
                    tags: Vec::new(),
                    payload: serde_json::json!({"text": "hi"}),
                    provenance: None,
                },
            },
        ]);

        assert_eq!(graph.branch_head(branch_id), Some(node_id));
    }
}
