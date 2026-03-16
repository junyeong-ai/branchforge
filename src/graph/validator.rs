use serde::Serialize;

use super::SessionGraph;

#[derive(Debug, Clone, Serialize)]
pub struct GraphValidationIssue {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphValidationReport {
    pub valid: bool,
    pub issues: Vec<GraphValidationIssue>,
}

impl GraphValidationReport {
    pub fn is_valid(&self) -> bool {
        self.valid
    }
}

pub struct GraphValidator;

impl GraphValidator {
    pub fn validate(graph: &SessionGraph) -> GraphValidationReport {
        let mut issues = Vec::new();

        if !graph.branches.contains_key(&graph.primary_branch) {
            issues.push(issue(
                "primary_branch_missing",
                format!(
                    "Primary branch {} does not exist in graph",
                    graph.primary_branch
                ),
            ));
        }

        for (branch_id, branch) in &graph.branches {
            if let Some(forked_from) = branch.forked_from
                && !graph.nodes.contains_key(&forked_from)
            {
                issues.push(issue(
                    "branch_fork_source_missing",
                    format!(
                        "Branch {} fork source node {} is missing",
                        branch_id, forked_from
                    ),
                ));
            }
            if let Some(head) = branch.head {
                match graph.nodes.get(&head) {
                    Some(node)
                        if node.branch_id == *branch_id || branch.forked_from == Some(head) => {}
                    Some(_) => issues.push(issue(
                        "branch_head_wrong_branch",
                        format!(
                            "Branch {} head does not belong to the same branch",
                            branch_id
                        ),
                    )),
                    None => issues.push(issue(
                        "branch_head_missing",
                        format!("Branch {} head node is missing", branch_id),
                    )),
                }
            }
        }

        for (node_id, node) in &graph.nodes {
            let branch_fork_source = graph
                .branches
                .get(&node.branch_id)
                .and_then(|branch| branch.forked_from);
            if !graph.branches.contains_key(&node.branch_id) {
                issues.push(issue(
                    "node_branch_missing",
                    format!(
                        "Node {} belongs to missing branch {}",
                        node_id, node.branch_id
                    ),
                ));
            }
            if let Some(parent_id) = node.parent_id {
                match graph.nodes.get(&parent_id) {
                    Some(parent)
                        if parent.branch_id == node.branch_id
                            || branch_fork_source == Some(parent_id) => {}
                    Some(parent) => issues.push(issue(
                        "parent_branch_mismatch",
                        format!(
                            "Node {} belongs to branch {} but parent {} belongs to branch {}",
                            node_id, node.branch_id, parent_id, parent.branch_id
                        ),
                    )),
                    None => issues.push(issue(
                        "missing_parent",
                        format!("Node {} references missing parent {}", node_id, parent_id),
                    )),
                }
            }
        }

        for (bookmark_id, bookmark) in &graph.bookmarks {
            if !graph.branches.contains_key(&bookmark.branch_id) {
                issues.push(issue(
                    "bookmark_branch_missing",
                    format!(
                        "Bookmark {} belongs to missing branch {}",
                        bookmark_id, bookmark.branch_id
                    ),
                ));
            }
            if !graph.nodes.contains_key(&bookmark.node_id) {
                issues.push(issue(
                    "bookmark_target_missing",
                    format!(
                        "Bookmark {} points to missing node {}",
                        bookmark_id, bookmark.node_id
                    ),
                ));
            }
            if let Some(node) = graph.nodes.get(&bookmark.node_id)
                && node.branch_id != bookmark.branch_id
            {
                issues.push(issue(
                    "bookmark_branch_mismatch",
                    format!(
                        "Bookmark {} branch {} does not match target node branch {}",
                        bookmark_id, bookmark.branch_id, node.branch_id
                    ),
                ));
            }
        }

        for (checkpoint_id, checkpoint) in &graph.checkpoints {
            if !graph.branches.contains_key(&checkpoint.branch_id) {
                issues.push(issue(
                    "checkpoint_branch_missing",
                    format!(
                        "Checkpoint {} belongs to missing branch {}",
                        checkpoint_id, checkpoint.branch_id
                    ),
                ));
            }
            if !graph.nodes.contains_key(checkpoint_id) {
                issues.push(issue(
                    "checkpoint_node_missing",
                    format!("Checkpoint {} is missing its graph node", checkpoint_id),
                ));
            } else if let Some(node) = graph.nodes.get(checkpoint_id) {
                if node.branch_id != checkpoint.branch_id {
                    issues.push(issue(
                        "checkpoint_branch_mismatch",
                        format!(
                            "Checkpoint {} branch {} does not match node branch {}",
                            checkpoint_id, checkpoint.branch_id, node.branch_id
                        ),
                    ));
                }
                if node.kind != super::NodeKind::Checkpoint {
                    issues.push(issue(
                        "checkpoint_node_kind_mismatch",
                        format!(
                            "Checkpoint {} is backed by node kind {:?} instead of Checkpoint",
                            checkpoint_id, node.kind
                        ),
                    ));
                }
            }
        }

        for (node_id, node) in &graph.nodes {
            if node.created_by_principal_id.is_some() && node.provenance.is_none() {
                issues.push(issue(
                    "provenance_missing",
                    format!("Node {} has creator identity but no provenance", node_id),
                ));
            }
        }

        GraphValidationReport {
            valid: issues.is_empty(),
            issues,
        }
    }

    pub fn validate_restore_pair(
        original: &SessionGraph,
        restored: &SessionGraph,
    ) -> GraphValidationReport {
        let mut report = Self::validate(restored);

        if original.nodes.len() != restored.nodes.len() {
            report.issues.push(issue(
                "restore_node_count_mismatch",
                format!(
                    "Original graph node count {} does not match restored graph node count {}",
                    original.nodes.len(),
                    restored.nodes.len()
                ),
            ));
        }

        if original.bookmarks.len() != restored.bookmarks.len() {
            report.issues.push(issue(
                "restore_bookmark_count_mismatch",
                format!(
                    "Original bookmark count {} does not match restored bookmark count {}",
                    original.bookmarks.len(),
                    restored.bookmarks.len()
                ),
            ));
        }

        if original.checkpoints.len() != restored.checkpoints.len() {
            report.issues.push(issue(
                "restore_checkpoint_count_mismatch",
                format!(
                    "Original checkpoint count {} does not match restored checkpoint count {}",
                    original.checkpoints.len(),
                    restored.checkpoints.len()
                ),
            ));
        }

        report.valid = report.issues.is_empty();
        report
    }
}

fn issue(code: &str, message: String) -> GraphValidationIssue {
    GraphValidationIssue {
        code: code.to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{
        Bookmark, Branch, Checkpoint, GraphNode, NodeKind, NodeProvenance, SessionGraph,
    };
    use chrono::Utc;
    use uuid::Uuid;

    // ---------------------------------------------------------------
    // Test helpers
    // ---------------------------------------------------------------

    fn test_node(id: Uuid, kind: NodeKind, branch_id: Uuid, parent_id: Option<Uuid>) -> GraphNode {
        GraphNode {
            id,
            branch_id,
            kind,
            parent_id,
            created_by_principal_id: None,
            provenance: None,
            created_at: Utc::now(),
            tags: Vec::new(),
            payload: serde_json::json!({}),
        }
    }

    /// Build a linear graph with `n` User/Assistant nodes on the primary branch.
    fn linear_graph(n: usize) -> SessionGraph {
        let mut graph = SessionGraph::default();
        for i in 0..n {
            let kind = if i % 2 == 0 {
                NodeKind::User
            } else {
                NodeKind::Assistant
            };
            graph
                .append_node(graph.primary_branch, kind, serde_json::json!({}))
                .unwrap();
        }
        graph
    }

    /// Build a graph with primary branch (2 nodes) and a fork (1 extra node).
    fn forked_graph() -> (SessionGraph, Uuid, Uuid) {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        graph
            .append_node(
                graph.primary_branch,
                NodeKind::Assistant,
                serde_json::json!({}),
            )
            .unwrap();
        let side = graph.fork_branch(Some(root), "side").unwrap();
        graph
            .append_node(side, NodeKind::Assistant, serde_json::json!({}))
            .unwrap();
        (graph, root, side)
    }

    fn has_issue(report: &GraphValidationReport, code: &str) -> bool {
        report.issues.iter().any(|i| i.code == code)
    }

    // ---------------------------------------------------------------
    // primary_branch_missing
    // ---------------------------------------------------------------

    #[test]
    fn primary_branch_missing() {
        let mut graph = linear_graph(1);
        let fake_branch = Uuid::new_v4();
        graph.primary_branch = fake_branch;
        graph.branches.remove(&fake_branch); // ensure it doesn't accidentally exist

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "primary_branch_missing"));
    }

    // ---------------------------------------------------------------
    // branch_fork_source_missing
    // ---------------------------------------------------------------

    #[test]
    fn branch_fork_source_missing() {
        let mut graph = SessionGraph::default();
        let phantom_node = Uuid::new_v4();
        let side_id = Uuid::new_v4();
        graph.branches.insert(
            side_id,
            Branch {
                id: side_id,
                name: "side".to_string(),
                forked_from: Some(phantom_node), // node does not exist
                created_at: Utc::now(),
                head: None,
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "branch_fork_source_missing"));
    }

    // ---------------------------------------------------------------
    // branch_head_wrong_branch
    // ---------------------------------------------------------------

    #[test]
    fn branch_head_wrong_branch() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let side_id = Uuid::new_v4();
        // Create branch whose head points to a node that belongs to a different branch
        graph.branches.insert(
            side_id,
            Branch {
                id: side_id,
                name: "side".to_string(),
                forked_from: None,
                created_at: Utc::now(),
                head: Some(root), // root belongs to primary_branch, not side_id
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "branch_head_wrong_branch"));
    }

    #[test]
    fn branch_head_at_fork_source_is_valid() {
        // When a branch has just been forked, its head points to the fork source node
        // which belongs to the parent branch. This is allowed.
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let side = graph.fork_branch(Some(root), "side").unwrap();

        // Verify the branch head is the fork source
        assert_eq!(graph.branch_head(side), Some(root));

        let report = GraphValidator::validate(&graph);
        assert!(report.is_valid());
    }

    // ---------------------------------------------------------------
    // branch_head_missing
    // ---------------------------------------------------------------

    #[test]
    fn branch_head_missing() {
        let mut graph = SessionGraph::default();
        let phantom_node = Uuid::new_v4();
        // Directly set primary branch head to non-existent node
        graph.branches.get_mut(&graph.primary_branch).unwrap().head = Some(phantom_node);

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "branch_head_missing"));
    }

    // ---------------------------------------------------------------
    // node_branch_missing
    // ---------------------------------------------------------------

    #[test]
    fn node_branch_missing() {
        let mut graph = SessionGraph::default();
        let phantom_branch = Uuid::new_v4();
        let node_id = Uuid::new_v4();
        graph.nodes.insert(
            node_id,
            test_node(node_id, NodeKind::User, phantom_branch, None),
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "node_branch_missing"));
    }

    // ---------------------------------------------------------------
    // parent_branch_mismatch
    // ---------------------------------------------------------------

    #[test]
    fn parent_branch_mismatch() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let main_next = graph
            .append_node(
                graph.primary_branch,
                NodeKind::Assistant,
                serde_json::json!({}),
            )
            .unwrap();
        let side = graph.fork_branch(Some(root), "side").unwrap();

        // Manually insert a node on the side branch whose parent is main_next,
        // which is NOT the fork source for this branch (root is).
        let bad_node = Uuid::new_v4();
        graph.nodes.insert(
            bad_node,
            test_node(bad_node, NodeKind::Assistant, side, Some(main_next)),
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "parent_branch_mismatch"));
    }

    #[test]
    fn parent_on_fork_source_is_valid() {
        // A node whose parent is the fork source of its branch should NOT trigger
        // parent_branch_mismatch.
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let side = graph.fork_branch(Some(root), "side").unwrap();
        graph
            .append_node(side, NodeKind::Assistant, serde_json::json!({}))
            .unwrap();

        let report = GraphValidator::validate(&graph);
        assert!(report.is_valid());
    }

    // ---------------------------------------------------------------
    // missing_parent
    // ---------------------------------------------------------------

    #[test]
    fn missing_parent() {
        let mut graph = SessionGraph::default();
        let phantom_parent = Uuid::new_v4();
        let node_id = Uuid::new_v4();
        graph.nodes.insert(
            node_id,
            test_node(
                node_id,
                NodeKind::User,
                graph.primary_branch,
                Some(phantom_parent),
            ),
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "missing_parent"));
    }

    // ---------------------------------------------------------------
    // bookmark_branch_missing
    // ---------------------------------------------------------------

    #[test]
    fn bookmark_branch_missing() {
        let mut graph = SessionGraph::default();
        let node = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let phantom_branch = Uuid::new_v4();
        let bm_id = Uuid::new_v4();
        graph.bookmarks.insert(
            bm_id,
            Bookmark {
                id: bm_id,
                node_id: node,
                branch_id: phantom_branch,
                label: "bad".to_string(),
                note: None,
                created_by_principal_id: None,
                provenance: None,
                created_at: Utc::now(),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "bookmark_branch_missing"));
    }

    // ---------------------------------------------------------------
    // bookmark_target_missing
    // ---------------------------------------------------------------

    #[test]
    fn bookmark_target_missing() {
        let mut graph = SessionGraph::default();
        let phantom_node = Uuid::new_v4();
        let bm_id = Uuid::new_v4();
        graph.bookmarks.insert(
            bm_id,
            Bookmark {
                id: bm_id,
                node_id: phantom_node,
                branch_id: graph.primary_branch,
                label: "lost".to_string(),
                note: None,
                created_by_principal_id: None,
                provenance: None,
                created_at: Utc::now(),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "bookmark_target_missing"));
    }

    // ---------------------------------------------------------------
    // bookmark_branch_mismatch
    // ---------------------------------------------------------------

    #[test]
    fn bookmark_branch_mismatch() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let side = graph.fork_branch(Some(root), "side").unwrap();
        let side_node = graph
            .append_node(side, NodeKind::Assistant, serde_json::json!({}))
            .unwrap();

        // Bookmark claims to be on the primary branch but references a node on `side`
        let bm_id = Uuid::new_v4();
        graph.bookmarks.insert(
            bm_id,
            Bookmark {
                id: bm_id,
                node_id: side_node,
                branch_id: graph.primary_branch,
                label: "cross".to_string(),
                note: None,
                created_by_principal_id: None,
                provenance: None,
                created_at: Utc::now(),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "bookmark_branch_mismatch"));
    }

    // ---------------------------------------------------------------
    // checkpoint_branch_missing
    // ---------------------------------------------------------------

    #[test]
    fn checkpoint_branch_missing() {
        let mut graph = SessionGraph::default();
        let phantom_branch = Uuid::new_v4();
        let cp_id = Uuid::new_v4();
        // Insert a checkpoint node so we don't also trigger checkpoint_node_missing
        graph.nodes.insert(
            cp_id,
            test_node(cp_id, NodeKind::Checkpoint, graph.primary_branch, None),
        );
        graph.checkpoints.insert(
            cp_id,
            Checkpoint {
                id: cp_id,
                branch_id: phantom_branch,
                label: "cp".to_string(),
                note: None,
                tags: Vec::new(),
                created_by_principal_id: None,
                provenance: None,
                created_at: Utc::now(),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "checkpoint_branch_missing"));
    }

    // ---------------------------------------------------------------
    // checkpoint_node_missing
    // ---------------------------------------------------------------

    #[test]
    fn checkpoint_node_missing() {
        let mut graph = SessionGraph::default();
        let cp_id = Uuid::new_v4();
        // Register checkpoint but do NOT insert a matching node
        graph.checkpoints.insert(
            cp_id,
            Checkpoint {
                id: cp_id,
                branch_id: graph.primary_branch,
                label: "gone".to_string(),
                note: None,
                tags: Vec::new(),
                created_by_principal_id: None,
                provenance: None,
                created_at: Utc::now(),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "checkpoint_node_missing"));
    }

    // ---------------------------------------------------------------
    // checkpoint_branch_mismatch
    // ---------------------------------------------------------------

    #[test]
    fn checkpoint_branch_mismatch() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        let side = graph.fork_branch(Some(root), "side").unwrap();

        let cp_id = Uuid::new_v4();
        // Node is on primary branch but checkpoint claims side branch
        graph.nodes.insert(
            cp_id,
            test_node(cp_id, NodeKind::Checkpoint, graph.primary_branch, None),
        );
        graph.checkpoints.insert(
            cp_id,
            Checkpoint {
                id: cp_id,
                branch_id: side,
                label: "mismatch".to_string(),
                note: None,
                tags: Vec::new(),
                created_by_principal_id: None,
                provenance: None,
                created_at: Utc::now(),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "checkpoint_branch_mismatch"));
    }

    // ---------------------------------------------------------------
    // checkpoint_node_kind_mismatch
    // ---------------------------------------------------------------

    #[test]
    fn checkpoint_node_kind_mismatch() {
        let mut graph = SessionGraph::default();
        let cp_id = Uuid::new_v4();
        // Node has kind User but checkpoint expects Checkpoint
        graph.nodes.insert(
            cp_id,
            test_node(cp_id, NodeKind::User, graph.primary_branch, None),
        );
        graph.checkpoints.insert(
            cp_id,
            Checkpoint {
                id: cp_id,
                branch_id: graph.primary_branch,
                label: "wrong-kind".to_string(),
                note: None,
                tags: Vec::new(),
                created_by_principal_id: None,
                provenance: None,
                created_at: Utc::now(),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "checkpoint_node_kind_mismatch"));
    }

    // ---------------------------------------------------------------
    // provenance_missing
    // ---------------------------------------------------------------

    #[test]
    fn provenance_missing_when_creator_set() {
        let mut graph = SessionGraph::default();
        let node_id = Uuid::new_v4();
        let mut node = test_node(node_id, NodeKind::User, graph.primary_branch, None);
        node.created_by_principal_id = Some("user-1".to_string());
        node.provenance = None; // creator set but no provenance
        graph.nodes.insert(node_id, node);

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "provenance_missing"));
    }

    #[test]
    fn provenance_present_when_creator_set_is_valid() {
        let mut graph = SessionGraph::default();
        let node_id = Uuid::new_v4();
        let mut node = test_node(node_id, NodeKind::User, graph.primary_branch, None);
        node.created_by_principal_id = Some("user-1".to_string());
        node.provenance = Some(NodeProvenance {
            source_session_id: Uuid::new_v4().to_string(),
            session_type: "main".to_string(),
            task_id: None,
            subagent_session_id: None,
            subagent_type: None,
            subagent_description: None,
        });
        graph.nodes.insert(node_id, node);

        let report = GraphValidator::validate(&graph);
        assert!(report.is_valid());
    }

    // ---------------------------------------------------------------
    // valid_graph_passes - complex valid graph
    // ---------------------------------------------------------------

    #[test]
    fn valid_complex_graph_passes() {
        let mut graph = SessionGraph::default();
        let primary = graph.primary_branch;

        // Build a linear chain on primary: User -> Assistant -> User -> Assistant
        let n1 = graph
            .append_node(primary, NodeKind::User, serde_json::json!({"text": "q1"}))
            .unwrap();
        let n2 = graph
            .append_node(
                primary,
                NodeKind::Assistant,
                serde_json::json!({"text": "a1"}),
            )
            .unwrap();
        let _n3 = graph
            .append_node(primary, NodeKind::User, serde_json::json!({"text": "q2"}))
            .unwrap();
        let _n4 = graph
            .append_node(
                primary,
                NodeKind::Assistant,
                serde_json::json!({"text": "a2"}),
            )
            .unwrap();

        // Fork from n1 and add a side branch
        let side = graph.fork_branch(Some(n1), "experiment").unwrap();
        graph
            .append_node(
                side,
                NodeKind::Assistant,
                serde_json::json!({"text": "alt"}),
            )
            .unwrap();

        // Fork from n2 and add another side branch
        let side2 = graph.fork_branch(Some(n2), "experiment-2").unwrap();
        let side2_node = graph
            .append_node(
                side2,
                NodeKind::User,
                serde_json::json!({"text": "side2-q"}),
            )
            .unwrap();

        // Bookmark and checkpoint on various branches
        graph.create_bookmark(n1, "root", None, None, None).unwrap();
        graph
            .create_bookmark(
                side2_node,
                "side2-mark",
                Some("a note".to_string()),
                None,
                None,
            )
            .unwrap();
        graph
            .create_checkpoint(primary, "main-cp", None, vec!["v1".to_string()], None, None)
            .unwrap();
        graph
            .create_checkpoint(
                side2,
                "side2-cp",
                Some("note".to_string()),
                vec![],
                None,
                None,
            )
            .unwrap();

        let report = GraphValidator::validate(&graph);
        assert!(
            report.is_valid(),
            "Expected valid graph but got issues: {:?}",
            report.issues
        );
    }

    // ---------------------------------------------------------------
    // multiple_issues_detected
    // ---------------------------------------------------------------

    #[test]
    fn multiple_issues_detected() {
        let mut graph = SessionGraph::default();

        // Issue 1: primary branch missing
        let bad_primary = Uuid::new_v4();
        graph.primary_branch = bad_primary;

        // Issue 2: node references missing branch
        let node_id = Uuid::new_v4();
        graph.nodes.insert(
            node_id,
            test_node(node_id, NodeKind::User, bad_primary, None),
        );

        // Issue 3: bookmark target missing
        let bm_id = Uuid::new_v4();
        let phantom_node = Uuid::new_v4();
        // Use the real existing branch for the bookmark so we isolate just the target issue
        let existing_branch = *graph.branches.keys().next().unwrap();
        graph.bookmarks.insert(
            bm_id,
            Bookmark {
                id: bm_id,
                node_id: phantom_node,
                branch_id: existing_branch,
                label: "broken-bm".to_string(),
                note: None,
                created_by_principal_id: None,
                provenance: None,
                created_at: Utc::now(),
            },
        );

        // Issue 4: checkpoint node missing
        let cp_id = Uuid::new_v4();
        graph.checkpoints.insert(
            cp_id,
            Checkpoint {
                id: cp_id,
                branch_id: existing_branch,
                label: "broken-cp".to_string(),
                note: None,
                tags: Vec::new(),
                created_by_principal_id: None,
                provenance: None,
                created_at: Utc::now(),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "primary_branch_missing"));
        assert!(has_issue(&report, "node_branch_missing"));
        assert!(has_issue(&report, "bookmark_target_missing"));
        assert!(has_issue(&report, "checkpoint_node_missing"));
        assert!(
            report.issues.len() >= 4,
            "Expected at least 4 issues, got {}: {:?}",
            report.issues.len(),
            report.issues
        );
    }

    // ---------------------------------------------------------------
    // validate_restore_pair
    // ---------------------------------------------------------------

    #[test]
    fn validate_restore_pair_identical_graphs() {
        let graph = linear_graph(4);
        let report = GraphValidator::validate_restore_pair(&graph, &graph);
        assert!(
            report.is_valid(),
            "Identical graphs should pass restore validation, got: {:?}",
            report.issues
        );
    }

    #[test]
    fn validate_restore_pair_node_count_mismatch() {
        let original = linear_graph(4);
        let restored = linear_graph(2);

        let report = GraphValidator::validate_restore_pair(&original, &restored);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "restore_node_count_mismatch"));
    }

    #[test]
    fn validate_restore_pair_bookmark_count_mismatch() {
        let mut original = SessionGraph::default();
        let node = original
            .append_node(
                original.primary_branch,
                NodeKind::User,
                serde_json::json!({}),
            )
            .unwrap();
        original
            .create_bookmark(node, "bm", None, None, None)
            .unwrap();

        // Restored has the same node but no bookmarks
        let mut restored = SessionGraph::default();
        restored
            .append_node(
                restored.primary_branch,
                NodeKind::User,
                serde_json::json!({}),
            )
            .unwrap();

        let report = GraphValidator::validate_restore_pair(&original, &restored);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "restore_bookmark_count_mismatch"));
    }

    #[test]
    fn validate_restore_pair_checkpoint_count_mismatch() {
        let mut original = SessionGraph::default();
        original
            .append_node(
                original.primary_branch,
                NodeKind::User,
                serde_json::json!({}),
            )
            .unwrap();
        original
            .create_checkpoint(original.primary_branch, "cp", None, vec![], None, None)
            .unwrap();

        // Restored has one User node but no checkpoint
        let mut restored = SessionGraph::default();
        restored
            .append_node(
                restored.primary_branch,
                NodeKind::User,
                serde_json::json!({}),
            )
            .unwrap();

        let report = GraphValidator::validate_restore_pair(&original, &restored);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "restore_checkpoint_count_mismatch"));
    }

    #[test]
    fn validate_restore_pair_also_validates_restored_graph_structure() {
        let original = linear_graph(2);

        // Build a structurally broken restored graph with the right node count
        let mut restored = SessionGraph::default();
        restored
            .append_node(
                restored.primary_branch,
                NodeKind::User,
                serde_json::json!({}),
            )
            .unwrap();
        // Manually insert a node referencing a missing parent
        let bad_node = Uuid::new_v4();
        let phantom_parent = Uuid::new_v4();
        restored.nodes.insert(
            bad_node,
            test_node(
                bad_node,
                NodeKind::Assistant,
                restored.primary_branch,
                Some(phantom_parent),
            ),
        );

        let report = GraphValidator::validate_restore_pair(&original, &restored);
        assert!(!report.is_valid());
        assert!(has_issue(&report, "missing_parent"));
    }

    // ---------------------------------------------------------------
    // default graph is valid
    // ---------------------------------------------------------------

    #[test]
    fn empty_default_graph_is_valid() {
        let graph = SessionGraph::default();
        let report = GraphValidator::validate(&graph);
        assert!(report.is_valid());
    }

    // ---------------------------------------------------------------
    // linear_graph helper produces valid graphs
    // ---------------------------------------------------------------

    #[test]
    fn linear_graph_helper_is_valid() {
        let graph = linear_graph(6);
        let report = GraphValidator::validate(&graph);
        assert!(
            report.is_valid(),
            "linear_graph helper produced invalid graph: {:?}",
            report.issues
        );
        assert_eq!(graph.nodes.len(), 6);
    }

    // ---------------------------------------------------------------
    // forked_graph helper produces valid graphs
    // ---------------------------------------------------------------

    #[test]
    fn forked_graph_helper_is_valid() {
        let (graph, _root, _side) = forked_graph();
        let report = GraphValidator::validate(&graph);
        assert!(
            report.is_valid(),
            "forked_graph helper produced invalid graph: {:?}",
            report.issues
        );
        assert_eq!(graph.branches.len(), 2);
        assert_eq!(graph.nodes.len(), 3);
    }
}
