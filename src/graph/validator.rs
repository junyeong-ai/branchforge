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

        for (branch_id, branch) in &graph.branches {
            if let Some(head) = branch.head {
                match graph.nodes.get(&head) {
                    Some(node) if node.branch_id == *branch_id => {}
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
            if let Some(parent_id) = node.parent_id
                && !graph.nodes.contains_key(&parent_id)
            {
                issues.push(issue(
                    "missing_parent",
                    format!("Node {} references missing parent {}", node_id, parent_id),
                ));
            }
        }

        for (bookmark_id, bookmark) in &graph.bookmarks {
            if !graph.nodes.contains_key(&bookmark.node_id) {
                issues.push(issue(
                    "bookmark_target_missing",
                    format!(
                        "Bookmark {} points to missing node {}",
                        bookmark_id, bookmark.node_id
                    ),
                ));
            }
        }

        for checkpoint_id in graph.checkpoints.keys() {
            if !graph.nodes.contains_key(checkpoint_id) {
                issues.push(issue(
                    "checkpoint_node_missing",
                    format!("Checkpoint {} is missing its graph node", checkpoint_id),
                ));
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
        let root = graph.append_node(graph.primary_branch, NodeKind::User, serde_json::json!({}));
        graph.create_bookmark(root, "start", None, None, None);
        graph.create_checkpoint(graph.primary_branch, "mark", None, vec![], None, None);

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
}
