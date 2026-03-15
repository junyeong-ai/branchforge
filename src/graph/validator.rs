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
    use crate::graph::{NodeKind, SessionGraph};

    #[test]
    fn validator_accepts_valid_graph() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}))
            .unwrap();
        graph
            .create_bookmark(root, "start", None, None, None)
            .unwrap();
        graph
            .create_checkpoint(graph.primary_branch, "mark", None, vec![], None, None)
            .unwrap();

        let report = GraphValidator::validate(&graph);
        assert!(report.is_valid());
    }

    #[test]
    fn validator_reports_missing_bookmark_target() {
        let mut graph = SessionGraph::default();
        graph.bookmarks.insert(
            uuid::Uuid::new_v4(),
            crate::graph::Bookmark {
                id: uuid::Uuid::new_v4(),
                node_id: uuid::Uuid::new_v4(),
                branch_id: graph.primary_branch,
                label: "broken".to_string(),
                note: None,
                created_by_principal_id: None,
                provenance: None,
                created_at: chrono::Utc::now(),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "bookmark_target_missing")
        );
    }

    #[test]
    fn validator_reports_missing_branch_references() {
        let mut graph = SessionGraph::default();
        let missing_branch = uuid::Uuid::new_v4();
        graph.primary_branch = missing_branch;
        graph.nodes.insert(
            uuid::Uuid::new_v4(),
            crate::graph::GraphNode {
                id: uuid::Uuid::new_v4(),
                branch_id: missing_branch,
                kind: crate::graph::NodeKind::User,
                parent_id: None,
                created_by_principal_id: None,
                provenance: None,
                created_at: chrono::Utc::now(),
                tags: Vec::new(),
                payload: serde_json::json!({}),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "primary_branch_missing")
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "node_branch_missing")
        );
    }

    #[test]
    fn validator_reports_parent_branch_mismatch() {
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
        let side_node = uuid::Uuid::new_v4();
        graph.nodes.insert(
            side_node,
            crate::graph::GraphNode {
                id: side_node,
                branch_id: side,
                kind: NodeKind::Assistant,
                parent_id: Some(main_follow_up),
                created_by_principal_id: None,
                provenance: None,
                created_at: chrono::Utc::now(),
                tags: Vec::new(),
                payload: serde_json::json!({}),
            },
        );

        let report = GraphValidator::validate(&graph);
        assert!(!report.is_valid());
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "parent_branch_mismatch")
        );
    }
}
