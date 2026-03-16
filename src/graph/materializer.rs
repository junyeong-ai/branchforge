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
    use crate::graph::event::EventMetadata;
    use crate::graph::validator::GraphValidator;
    use crate::graph::{GraphEvent, NodeKind};
    use chrono::Utc;
    use uuid::Uuid;

    // ---------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------

    fn meta() -> EventMetadata {
        EventMetadata::new(None)
    }

    fn node_appended(
        node_id: Uuid,
        branch_id: Uuid,
        parent_id: Option<Uuid>,
        kind: NodeKind,
        payload: serde_json::Value,
    ) -> GraphEvent {
        GraphEvent {
            metadata: meta(),
            body: GraphEventBody::NodeAppended {
                node_id,
                branch_id,
                parent_id,
                kind,
                tags: Vec::new(),
                payload,
                provenance: None,
            },
        }
    }

    fn branch_forked(branch_id: Uuid, name: &str, forked_from: Option<Uuid>) -> GraphEvent {
        GraphEvent {
            metadata: meta(),
            body: GraphEventBody::BranchForked {
                branch_id,
                name: name.to_string(),
                forked_from,
            },
        }
    }

    fn checkpoint_created(checkpoint_id: Uuid, branch_id: Uuid, label: &str) -> GraphEvent {
        GraphEvent {
            metadata: meta(),
            body: GraphEventBody::CheckpointCreated {
                checkpoint_id,
                branch_id,
                label: label.to_string(),
                note: None,
                tags: Vec::new(),
                provenance: None,
            },
        }
    }

    fn bookmark_created(
        bookmark_id: Uuid,
        node_id: Uuid,
        branch_id: Uuid,
        label: &str,
    ) -> GraphEvent {
        GraphEvent {
            metadata: meta(),
            body: GraphEventBody::BookmarkCreated {
                bookmark_id,
                node_id,
                branch_id,
                label: label.to_string(),
                note: None,
                provenance: None,
            },
        }
    }

    // ---------------------------------------------------------------
    // Original test (kept)
    // ---------------------------------------------------------------

    #[test]
    fn rebuilds_graph_from_events() {
        let branch_id = Uuid::new_v4();
        let node_id = Uuid::new_v4();
        let graph = GraphMaterializer::from_events(&[
            branch_forked(branch_id, "exp", None),
            node_appended(
                node_id,
                branch_id,
                None,
                NodeKind::User,
                serde_json::json!({"text": "hi"}),
            ),
        ]);

        assert_eq!(graph.branch_head(branch_id), Some(node_id));
    }

    // ---------------------------------------------------------------
    // empty_events_produce_empty_graph
    // ---------------------------------------------------------------

    #[test]
    fn empty_events_produce_empty_graph() {
        let graph = GraphMaterializer::from_events(&[]);

        assert!(graph.nodes.is_empty());
        assert!(graph.bookmarks.is_empty());
        assert!(graph.checkpoints.is_empty());
        // Should still have a fallback primary branch
        assert_eq!(graph.branches.len(), 1);
        assert!(graph.branches.contains_key(&graph.primary_branch));

        let report = GraphValidator::validate(&graph);
        assert!(
            report.is_valid(),
            "Empty materialized graph should be valid: {:?}",
            report.issues
        );
    }

    // ---------------------------------------------------------------
    // single_branch_materialization
    // ---------------------------------------------------------------

    #[test]
    fn single_branch_materialization() {
        let branch_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();
        let n2 = Uuid::new_v4();
        let n3 = Uuid::new_v4();

        let graph = GraphMaterializer::from_events(&[
            branch_forked(branch_id, "main", None),
            node_appended(
                n1,
                branch_id,
                None,
                NodeKind::User,
                serde_json::json!({"q": 1}),
            ),
            node_appended(
                n2,
                branch_id,
                Some(n1),
                NodeKind::Assistant,
                serde_json::json!({"a": 1}),
            ),
            node_appended(
                n3,
                branch_id,
                Some(n2),
                NodeKind::User,
                serde_json::json!({"q": 2}),
            ),
        ]);

        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.branches.len(), 1);
        assert_eq!(graph.branch_head(branch_id), Some(n3));

        // Verify parent chain
        assert_eq!(graph.nodes[&n1].parent_id, None);
        assert_eq!(graph.nodes[&n2].parent_id, Some(n1));
        assert_eq!(graph.nodes[&n3].parent_id, Some(n2));

        // Verify node kinds
        assert_eq!(graph.nodes[&n1].kind, NodeKind::User);
        assert_eq!(graph.nodes[&n2].kind, NodeKind::Assistant);
        assert_eq!(graph.nodes[&n3].kind, NodeKind::User);

        // Verify payloads
        assert_eq!(graph.nodes[&n1].payload, serde_json::json!({"q": 1}));
        assert_eq!(graph.nodes[&n2].payload, serde_json::json!({"a": 1}));

        let report = GraphValidator::validate(&graph);
        assert!(
            report.is_valid(),
            "Single-branch graph should be valid: {:?}",
            report.issues
        );
    }

    // ---------------------------------------------------------------
    // multi_branch_materialization
    // ---------------------------------------------------------------

    #[test]
    fn multi_branch_materialization() {
        let main_id = Uuid::new_v4();
        let side_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();
        let n2 = Uuid::new_v4();
        let n3 = Uuid::new_v4(); // on side branch

        let graph = GraphMaterializer::from_events(&[
            branch_forked(main_id, "main", None),
            node_appended(n1, main_id, None, NodeKind::User, serde_json::json!({})),
            node_appended(
                n2,
                main_id,
                Some(n1),
                NodeKind::Assistant,
                serde_json::json!({}),
            ),
            branch_forked(side_id, "side", Some(n1)),
            node_appended(
                n3,
                side_id,
                Some(n1),
                NodeKind::Assistant,
                serde_json::json!({"alt": true}),
            ),
        ]);

        assert_eq!(graph.branches.len(), 2);
        assert_eq!(graph.nodes.len(), 3);

        // Main branch head advanced to n2
        assert_eq!(graph.branch_head(main_id), Some(n2));
        // Side branch head advanced to n3
        assert_eq!(graph.branch_head(side_id), Some(n3));

        // Side branch fork source
        assert_eq!(graph.branches[&side_id].forked_from, Some(n1));

        // n3 parent is n1 (the fork point)
        assert_eq!(graph.nodes[&n3].parent_id, Some(n1));
        assert_eq!(graph.nodes[&n3].branch_id, side_id);
    }

    // ---------------------------------------------------------------
    // checkpoint_materialization
    // ---------------------------------------------------------------

    #[test]
    fn checkpoint_materialization() {
        let branch_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();
        let cp_id = Uuid::new_v4();

        let graph = GraphMaterializer::from_events(&[
            branch_forked(branch_id, "main", None),
            node_appended(n1, branch_id, None, NodeKind::User, serde_json::json!({})),
            checkpoint_created(cp_id, branch_id, "milestone"),
        ]);

        // Checkpoint present in checkpoints map
        assert_eq!(graph.checkpoints.len(), 1);
        assert!(graph.checkpoints.contains_key(&cp_id));
        assert_eq!(graph.checkpoints[&cp_id].label, "milestone");
        assert_eq!(graph.checkpoints[&cp_id].branch_id, branch_id);

        // Checkpoint also created a node
        assert!(graph.nodes.contains_key(&cp_id));
        assert_eq!(graph.nodes[&cp_id].kind, NodeKind::Checkpoint);
        assert_eq!(graph.nodes[&cp_id].branch_id, branch_id);

        // Checkpoint node's parent should be the previous head (n1)
        assert_eq!(graph.nodes[&cp_id].parent_id, Some(n1));

        // Branch head advances to checkpoint
        assert_eq!(graph.branch_head(branch_id), Some(cp_id));

        let report = GraphValidator::validate(&graph);
        assert!(
            report.is_valid(),
            "Graph with checkpoint should be valid: {:?}",
            report.issues
        );
    }

    // ---------------------------------------------------------------
    // bookmark_materialization
    // ---------------------------------------------------------------

    #[test]
    fn bookmark_materialization() {
        let branch_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();
        let n2 = Uuid::new_v4();
        let bm_id = Uuid::new_v4();

        let graph = GraphMaterializer::from_events(&[
            branch_forked(branch_id, "main", None),
            node_appended(n1, branch_id, None, NodeKind::User, serde_json::json!({})),
            node_appended(
                n2,
                branch_id,
                Some(n1),
                NodeKind::Assistant,
                serde_json::json!({}),
            ),
            bookmark_created(bm_id, n1, branch_id, "start"),
        ]);

        assert_eq!(graph.bookmarks.len(), 1);
        assert!(graph.bookmarks.contains_key(&bm_id));
        assert_eq!(graph.bookmarks[&bm_id].label, "start");
        assert_eq!(graph.bookmarks[&bm_id].node_id, n1);
        assert_eq!(graph.bookmarks[&bm_id].branch_id, branch_id);

        // Bookmarks do not affect head
        assert_eq!(graph.branch_head(branch_id), Some(n2));
    }

    // ---------------------------------------------------------------
    // metadata_patch_applied
    // ---------------------------------------------------------------

    #[test]
    fn metadata_patch_applied() {
        let branch_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();

        let graph = GraphMaterializer::from_events(&[
            branch_forked(branch_id, "main", None),
            node_appended(
                n1,
                branch_id,
                None,
                NodeKind::User,
                serde_json::json!({"text": "hi"}),
            ),
            GraphEvent {
                metadata: meta(),
                body: GraphEventBody::NodeMetadataPatched {
                    node_id: n1,
                    metadata: serde_json::json!({"tokens": 42}),
                },
            },
        ]);

        // Payload should have the metadata merged in
        let payload = &graph.nodes[&n1].payload;
        assert_eq!(payload["text"], "hi");
        assert_eq!(payload["metadata"]["tokens"], 42);
    }

    #[test]
    fn metadata_patch_on_nonobject_payload_replaces() {
        let branch_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();

        // Start with a non-object payload (string)
        let graph = GraphMaterializer::from_events(&[
            branch_forked(branch_id, "main", None),
            GraphEvent {
                metadata: meta(),
                body: GraphEventBody::NodeAppended {
                    node_id: n1,
                    branch_id,
                    parent_id: None,
                    kind: NodeKind::User,
                    tags: Vec::new(),
                    payload: serde_json::json!("raw string"),
                    provenance: None,
                },
            },
            GraphEvent {
                metadata: meta(),
                body: GraphEventBody::NodeMetadataPatched {
                    node_id: n1,
                    metadata: serde_json::json!({"key": "value"}),
                },
            },
        ]);

        // Non-object payload should be replaced with {"metadata": ...}
        let payload = &graph.nodes[&n1].payload;
        assert_eq!(payload["metadata"]["key"], "value");
    }

    #[test]
    fn metadata_patch_on_missing_node_is_noop() {
        let branch_id = Uuid::new_v4();
        let phantom = Uuid::new_v4();

        let graph = GraphMaterializer::from_events(&[
            branch_forked(branch_id, "main", None),
            GraphEvent {
                metadata: meta(),
                body: GraphEventBody::NodeMetadataPatched {
                    node_id: phantom,
                    metadata: serde_json::json!({"x": 1}),
                },
            },
        ]);

        // Node should not exist - patch was a no-op
        assert!(!graph.nodes.contains_key(&phantom));
    }

    // ---------------------------------------------------------------
    // primary_branch_preserved
    // ---------------------------------------------------------------

    #[test]
    fn primary_branch_preserved_via_explicit_id() {
        let main_id = Uuid::new_v4();
        let side_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();
        let n2 = Uuid::new_v4();

        let events = vec![
            branch_forked(side_id, "side", None),
            node_appended(n1, side_id, None, NodeKind::User, serde_json::json!({})),
            branch_forked(main_id, "main", None),
            node_appended(n2, main_id, None, NodeKind::User, serde_json::json!({})),
        ];

        // Without explicit primary, the first branch with events becomes primary
        let graph_auto = GraphMaterializer::from_events(&events);
        assert_eq!(graph_auto.primary_branch, side_id);

        // With explicit primary, we override
        let graph_explicit = GraphMaterializer::from_events_with_primary(&events, Some(main_id));
        assert_eq!(graph_explicit.primary_branch, main_id);
    }

    #[test]
    fn primary_branch_explicit_ignores_nonexistent_branch() {
        let branch_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();
        let phantom = Uuid::new_v4();

        let events = vec![
            branch_forked(branch_id, "main", None),
            node_appended(n1, branch_id, None, NodeKind::User, serde_json::json!({})),
        ];

        let graph = GraphMaterializer::from_events_with_primary(&events, Some(phantom));
        // Should fall back to the auto-detected primary since phantom doesn't exist
        assert_eq!(graph.primary_branch, branch_id);
    }

    // ---------------------------------------------------------------
    // round_trip_preserves_structure
    // ---------------------------------------------------------------

    #[test]
    fn round_trip_preserves_structure() {
        // Build a graph using the SessionGraph API
        let mut original = SessionGraph::default();
        let primary = original.primary_branch;

        let n1 = original
            .append_node(primary, NodeKind::User, serde_json::json!({"text": "q1"}))
            .unwrap();
        let _n2 = original
            .append_node(
                primary,
                NodeKind::Assistant,
                serde_json::json!({"text": "a1"}),
            )
            .unwrap();

        let side = original.fork_branch(Some(n1), "alt").unwrap();
        let _side_node = original
            .append_node(
                side,
                NodeKind::Assistant,
                serde_json::json!({"text": "alt-a"}),
            )
            .unwrap();

        original
            .create_bookmark(n1, "mark", None, None, None)
            .unwrap();
        original
            .create_checkpoint(primary, "cp1", None, vec![], None, None)
            .unwrap();

        // Round-trip: extract events -> materialize -> compare
        let events = original.events.clone();
        let restored = GraphMaterializer::from_events_with_primary(&events, Some(primary));

        // Same number of nodes, branches, bookmarks, checkpoints
        assert_eq!(original.nodes.len(), restored.nodes.len());
        assert_eq!(original.branches.len(), restored.branches.len());
        assert_eq!(original.bookmarks.len(), restored.bookmarks.len());
        assert_eq!(original.checkpoints.len(), restored.checkpoints.len());

        // All original node IDs are present in restored
        for node_id in original.nodes.keys() {
            assert!(
                restored.nodes.contains_key(node_id),
                "Node {} missing from restored graph",
                node_id
            );
        }

        // Node kinds match
        for (node_id, node) in &original.nodes {
            assert_eq!(
                node.kind, restored.nodes[node_id].kind,
                "Node {} kind mismatch",
                node_id
            );
        }

        // Parent pointers match
        for (node_id, node) in &original.nodes {
            assert_eq!(
                node.parent_id, restored.nodes[node_id].parent_id,
                "Node {} parent mismatch",
                node_id
            );
        }

        // Node payloads are identical after round-trip
        for (node_id, node) in &original.nodes {
            assert_eq!(
                node.payload, restored.nodes[node_id].payload,
                "Node {} payload mismatch",
                node_id
            );
        }

        // Branch heads match
        for (branch_id, branch) in &original.branches {
            assert_eq!(
                branch.head, restored.branches[branch_id].head,
                "Branch {} head mismatch",
                branch_id
            );
        }

        // Branch names are preserved
        for (branch_id, branch) in &original.branches {
            assert_eq!(
                branch.name, restored.branches[branch_id].name,
                "Branch {} name mismatch",
                branch_id
            );
        }

        // Checkpoint labels are preserved
        for (cp_id, cp) in &original.checkpoints {
            let restored_cp = &restored.checkpoints[cp_id];
            assert_eq!(
                cp.label, restored_cp.label,
                "Checkpoint {} label mismatch",
                cp_id
            );
            assert_eq!(
                cp.note, restored_cp.note,
                "Checkpoint {} note mismatch",
                cp_id
            );
        }

        // Bookmark labels are preserved
        for (bm_id, bm) in &original.bookmarks {
            let restored_bm = &restored.bookmarks[bm_id];
            assert_eq!(
                bm.label, restored_bm.label,
                "Bookmark {} label mismatch",
                bm_id
            );
            assert_eq!(
                bm.node_id, restored_bm.node_id,
                "Bookmark {} node_id mismatch",
                bm_id
            );
        }

        // Validate the restored graph structurally
        let report = GraphValidator::validate(&restored);
        assert!(
            report.is_valid(),
            "Restored graph should be valid: {:?}",
            report.issues
        );

        // Validate the restore pair
        let pair_report = GraphValidator::validate_restore_pair(&original, &restored);
        assert!(
            pair_report.is_valid(),
            "Restore pair should be valid: {:?}",
            pair_report.issues
        );
    }

    // ---------------------------------------------------------------
    // implicit branch creation from NodeAppended
    // ---------------------------------------------------------------

    #[test]
    fn node_appended_creates_branch_implicitly() {
        let branch_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();

        // No explicit BranchForked event; NodeAppended should create the branch
        let graph = GraphMaterializer::from_events(&[node_appended(
            n1,
            branch_id,
            None,
            NodeKind::User,
            serde_json::json!({}),
        )]);

        assert!(graph.branches.contains_key(&branch_id));
        assert_eq!(graph.branch_head(branch_id), Some(n1));
        assert_eq!(graph.primary_branch, branch_id);
    }

    // ---------------------------------------------------------------
    // events are replayed into the graph's event log
    // ---------------------------------------------------------------

    #[test]
    fn events_are_stored_in_materialized_graph() {
        let branch_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();
        let n2 = Uuid::new_v4();

        let events = vec![
            branch_forked(branch_id, "main", None),
            node_appended(n1, branch_id, None, NodeKind::User, serde_json::json!({})),
            node_appended(
                n2,
                branch_id,
                Some(n1),
                NodeKind::Assistant,
                serde_json::json!({}),
            ),
        ];

        let graph = GraphMaterializer::from_events(&events);
        assert_eq!(graph.events.len(), 3);
    }

    // ---------------------------------------------------------------
    // GraphMaterializer::empty()
    // ---------------------------------------------------------------

    #[test]
    fn empty_materializer_produces_default_graph() {
        let graph = GraphMaterializer::empty();
        assert!(graph.nodes.is_empty());
        assert_eq!(graph.branches.len(), 1);
        assert!(graph.branches.contains_key(&graph.primary_branch));

        let report = GraphValidator::validate(&graph);
        assert!(report.is_valid());
    }

    // ---------------------------------------------------------------
    // actor metadata propagation
    // ---------------------------------------------------------------

    #[test]
    fn actor_propagated_to_materialized_nodes() {
        let branch_id = Uuid::new_v4();
        let n1 = Uuid::new_v4();

        let graph = GraphMaterializer::from_events(&[GraphEvent {
            metadata: EventMetadata {
                id: Uuid::new_v4(),
                occurred_at: Utc::now(),
                actor: Some("user-42".to_string()),
            },
            body: GraphEventBody::NodeAppended {
                node_id: n1,
                branch_id,
                parent_id: None,
                kind: NodeKind::User,
                tags: vec!["tagged".to_string()],
                payload: serde_json::json!({}),
                provenance: None,
            },
        }]);

        let node = &graph.nodes[&n1];
        assert_eq!(node.created_by_principal_id.as_deref(), Some("user-42"));
        assert_eq!(node.tags, vec!["tagged".to_string()]);
    }

    // ---------------------------------------------------------------
    // complex multi-branch round-trip with bookmarks + checkpoints
    // ---------------------------------------------------------------

    #[test]
    fn complex_round_trip_with_all_event_types() {
        let mut original = SessionGraph::default();
        let primary = original.primary_branch;

        // Build a non-trivial graph
        let n1 = original
            .append_node(primary, NodeKind::User, serde_json::json!({"q": 1}))
            .unwrap();
        let n2 = original
            .append_node(primary, NodeKind::Assistant, serde_json::json!({"a": 1}))
            .unwrap();
        let n3 = original
            .append_node(primary, NodeKind::User, serde_json::json!({"q": 2}))
            .unwrap();

        // Fork two branches
        let side_a = original.fork_branch(Some(n1), "side-a").unwrap();
        original
            .append_node(
                side_a,
                NodeKind::Assistant,
                serde_json::json!({"alt_a": true}),
            )
            .unwrap();

        let side_b = original.fork_branch(Some(n2), "side-b").unwrap();
        let side_b_node = original
            .append_node(side_b, NodeKind::User, serde_json::json!({"alt_b": true}))
            .unwrap();

        // Bookmarks on different branches
        original
            .create_bookmark(n1, "root-mark", None, None, None)
            .unwrap();
        original
            .create_bookmark(
                side_b_node,
                "side-b-mark",
                Some("a note".to_string()),
                None,
                None,
            )
            .unwrap();

        // Checkpoints
        original
            .create_checkpoint(primary, "main-cp", None, vec!["v1".to_string()], None, None)
            .unwrap();
        original
            .create_checkpoint(
                side_a,
                "side-a-cp",
                Some("note".to_string()),
                vec![],
                None,
                None,
            )
            .unwrap();

        // Patch metadata
        original.patch_node_metadata(n3, serde_json::json!({"tokens": 100}), None);

        // Round-trip
        let events = original.events.clone();
        let restored = GraphMaterializer::from_events_with_primary(&events, Some(primary));

        assert_eq!(original.nodes.len(), restored.nodes.len());
        assert_eq!(original.branches.len(), restored.branches.len());
        assert_eq!(original.bookmarks.len(), restored.bookmarks.len());
        assert_eq!(original.checkpoints.len(), restored.checkpoints.len());

        // Node payloads are identical after round-trip
        for (node_id, node) in &original.nodes {
            assert_eq!(
                node.payload, restored.nodes[node_id].payload,
                "Node {} payload mismatch",
                node_id
            );
        }

        // Metadata patch was applied
        assert_eq!(restored.nodes[&n3].payload["metadata"]["tokens"], 100);

        // Checkpoint labels and notes are preserved
        for (cp_id, cp) in &original.checkpoints {
            let restored_cp = &restored.checkpoints[cp_id];
            assert_eq!(
                cp.label, restored_cp.label,
                "Checkpoint {} label mismatch",
                cp_id
            );
            assert_eq!(
                cp.note, restored_cp.note,
                "Checkpoint {} note mismatch",
                cp_id
            );
            assert_eq!(
                cp.tags, restored_cp.tags,
                "Checkpoint {} tags mismatch",
                cp_id
            );
        }

        // Bookmark labels are preserved
        for (bm_id, bm) in &original.bookmarks {
            let restored_bm = &restored.bookmarks[bm_id];
            assert_eq!(
                bm.label, restored_bm.label,
                "Bookmark {} label mismatch",
                bm_id
            );
            assert_eq!(
                bm.note, restored_bm.note,
                "Bookmark {} note mismatch",
                bm_id
            );
            assert_eq!(
                bm.node_id, restored_bm.node_id,
                "Bookmark {} node_id mismatch",
                bm_id
            );
        }

        // Branch names are preserved
        for (branch_id, branch) in &original.branches {
            assert_eq!(
                branch.name, restored.branches[branch_id].name,
                "Branch {} name mismatch",
                branch_id
            );
        }

        let report = GraphValidator::validate(&restored);
        assert!(
            report.is_valid(),
            "Complex restored graph should be valid: {:?}",
            report.issues
        );

        let pair_report = GraphValidator::validate_restore_pair(&original, &restored);
        assert!(
            pair_report.is_valid(),
            "Complex restore pair should be valid: {:?}",
            pair_report.issues
        );
    }

    // ---------------------------------------------------------------
    // primary branch fallback when first branch has no head
    // ---------------------------------------------------------------

    #[test]
    fn primary_branch_falls_back_to_branch_with_head() {
        let empty_branch = Uuid::new_v4();
        let active_branch = Uuid::new_v4();
        let n1 = Uuid::new_v4();

        // Create empty branch first (becomes primary by position), then branch with a node
        let graph = GraphMaterializer::from_events(&[
            branch_forked(empty_branch, "empty", None),
            branch_forked(active_branch, "active", None),
            node_appended(
                n1,
                active_branch,
                None,
                NodeKind::User,
                serde_json::json!({}),
            ),
        ]);

        // The primary should switch to the active branch since the first branch has no head
        assert_eq!(graph.primary_branch, active_branch);
    }
}
