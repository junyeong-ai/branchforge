use serde::{Deserialize, Serialize};

use super::SessionGraph;
use super::types::{BranchId, NodeId, NodeKind, NodeProvenance};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSummary {
    pub id: NodeId,
    pub branch_id: BranchId,
    pub kind: NodeKind,
    pub parent_id: Option<NodeId>,
    pub created_by_principal_id: Option<String>,
    pub provenance: Option<NodeProvenance>,
    pub provenance_digest: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub tags: Vec<String>,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchSummary {
    pub id: BranchId,
    pub name: String,
    pub forked_from: Option<NodeId>,
    pub head: Option<NodeId>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub node_count: usize,
    pub checkpoint_count: usize,
    pub bookmark_count: usize,
    pub last_activity_at: Option<chrono::DateTime<chrono::Utc>>,
    pub head_preview: Option<String>,
    pub divergence_from_primary: Option<usize>,
    pub summary_count: usize,
    pub tool_activity_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNodeSummary {
    pub node: NodeSummary,
    pub depth: usize,
    pub has_children: bool,
    pub has_checkpoint: bool,
    pub has_bookmark: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TreeRenderMode {
    Compact,
    Verbose,
}

pub struct GraphExplorer;

impl GraphExplorer {
    pub fn list_branches(graph: &SessionGraph) -> Vec<BranchSummary> {
        graph
            .branch_ids()
            .into_iter()
            .filter_map(|branch_id| {
                graph
                    .branches
                    .get(&branch_id)
                    .map(|branch| (branch_id, branch))
            })
            .map(|(branch_id, branch)| {
                let branch_nodes = graph.branch_nodes(branch_id);
                let head_preview = branch
                    .head
                    .and_then(|head| graph.nodes.get(&head))
                    .and_then(|node| preview_payload(&node.payload));
                BranchSummary {
                    id: branch_id,
                    name: branch.name.clone(),
                    forked_from: branch.forked_from,
                    head: branch.head,
                    created_at: branch.created_at,
                    node_count: branch_nodes.len(),
                    checkpoint_count: graph.checkpoints_for_branch(branch_id).len(),
                    bookmark_count: graph.bookmarks_for_branch(branch_id).len(),
                    last_activity_at: branch_nodes.last().map(|node| node.created_at),
                    head_preview,
                    divergence_from_primary: divergence_from_primary(graph, branch_id),
                    summary_count: branch_nodes
                        .iter()
                        .filter(|node| node.kind == NodeKind::Summary)
                        .count(),
                    tool_activity_count: branch_nodes
                        .iter()
                        .filter(|node| {
                            matches!(node.kind, NodeKind::ToolCall | NodeKind::ToolResult)
                        })
                        .count(),
                }
            })
            .collect()
    }

    pub fn tree_view(graph: &SessionGraph, branch_id: BranchId) -> Vec<TreeNodeSummary> {
        graph
            .current_branch_nodes(branch_id)
            .into_iter()
            .map(|node| TreeNodeSummary {
                node: summarize_node(node),
                depth: node_depth(graph, node.id),
                has_children: !graph.children_of(node.id).is_empty(),
                has_checkpoint: graph.checkpoints.contains_key(&node.id),
                has_bookmark: graph
                    .bookmarks
                    .values()
                    .any(|bookmark| bookmark.node_id == node.id),
            })
            .collect()
    }

    pub fn render_tree(graph: &SessionGraph, branch_id: BranchId, mode: TreeRenderMode) -> String {
        let tree = Self::tree_view(graph, branch_id);
        let branch = graph.branches.get(&branch_id);
        let mut lines = Vec::new();
        if let Some(branch) = branch {
            lines.push(format!("branch {} ({})", branch.name, branch.id));
        }
        for item in tree {
            let marker = node_marker(&item);
            let indent = "  ".repeat(item.depth);
            let head = branch.and_then(|branch| branch.head) == Some(item.node.id);
            let head_marker = if head { " *" } else { "" };
            match mode {
                TreeRenderMode::Compact => {
                    let preview = item.node.preview.as_deref().unwrap_or("");
                    lines.push(format!(
                        "{}{} {}{}{}",
                        indent,
                        marker,
                        item.node.kind_label(),
                        head_marker,
                        if preview.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", preview)
                        }
                    ));
                }
                TreeRenderMode::Verbose => {
                    let preview = item.node.preview.as_deref().unwrap_or("no preview");
                    let provenance = item.node.provenance_digest.as_deref().unwrap_or("");
                    lines.push(format!(
                        "{}{} {}{} [{}] {}{}",
                        indent,
                        marker,
                        item.node.kind_label(),
                        head_marker,
                        item.node.id,
                        preview,
                        if provenance.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", provenance)
                        }
                    ));
                }
            }
        }
        lines.join("\n")
    }

    pub fn bookmarks(graph: &SessionGraph, branch_id: Option<BranchId>) -> Vec<super::Bookmark> {
        let mut bookmarks: Vec<_> = graph.bookmarks.values().cloned().collect();
        if let Some(branch_id) = branch_id {
            bookmarks.retain(|bookmark| bookmark.branch_id == branch_id);
        }
        bookmarks.sort_by_key(|bookmark| bookmark.created_at);
        bookmarks
    }

    pub fn checkpoints(
        graph: &SessionGraph,
        branch_id: Option<BranchId>,
    ) -> Vec<super::Checkpoint> {
        let mut checkpoints: Vec<_> = graph.checkpoints.values().cloned().collect();
        if let Some(branch_id) = branch_id {
            checkpoints.retain(|checkpoint| checkpoint.branch_id == branch_id);
        }
        checkpoints.sort_by_key(|checkpoint| checkpoint.created_at);
        checkpoints
    }

    pub fn node_summary(graph: &SessionGraph, node_id: NodeId) -> Option<NodeSummary> {
        graph.nodes.get(&node_id).map(summarize_node)
    }
}

pub(crate) fn node_summary_from_graph_node(node: super::GraphNode) -> NodeSummary {
    summarize_node(&node)
}

fn summarize_node(node: &super::GraphNode) -> NodeSummary {
    NodeSummary {
        id: node.id,
        branch_id: node.branch_id,
        kind: node.kind,
        parent_id: node.parent_id,
        created_by_principal_id: node.created_by_principal_id.clone(),
        provenance: node.provenance.clone(),
        provenance_digest: crate::graph::ProvenanceSummaryService::render_node_digest(node),
        created_at: node.created_at,
        tags: node.tags.clone(),
        preview: preview_payload(&node.payload),
    }
}

impl NodeSummary {
    fn kind_label(&self) -> &'static str {
        match self.kind {
            NodeKind::User => "user",
            NodeKind::Assistant => "assistant",
            NodeKind::ToolCall => "tool_call",
            NodeKind::ToolResult => "tool_result",
            NodeKind::Summary => "summary",
            NodeKind::Plan => "plan",
            NodeKind::Todo => "todo",
            NodeKind::Checkpoint => "checkpoint",
            NodeKind::Branch => "branch",
        }
    }
}

fn node_marker(item: &TreeNodeSummary) -> String {
    let mut marker = match item.node.kind {
        NodeKind::User => "U".to_string(),
        NodeKind::Assistant => "A".to_string(),
        NodeKind::ToolCall => "TC".to_string(),
        NodeKind::ToolResult => "TR".to_string(),
        NodeKind::Summary => "S".to_string(),
        NodeKind::Plan => "P".to_string(),
        NodeKind::Todo => "T".to_string(),
        NodeKind::Checkpoint => "C".to_string(),
        NodeKind::Branch => "B".to_string(),
    };
    if item.has_checkpoint && item.node.kind != NodeKind::Checkpoint {
        marker.push('!');
    }
    if item.has_bookmark {
        marker.push('@');
    }
    marker
}

pub(crate) fn bookmark_digest(bookmark: &super::Bookmark) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(actor) = bookmark.created_by_principal_id.as_ref() {
        parts.push(format!("actor:{}", actor));
    }
    if let Some(task_id) = bookmark
        .provenance
        .as_ref()
        .and_then(|p| p.task_id.as_ref())
    {
        parts.push(format!("task:{}", task_id));
    }
    if let Some(subagent) = bookmark
        .provenance
        .as_ref()
        .and_then(|p| p.subagent_type.as_ref())
    {
        parts.push(format!("subagent:{}", subagent));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub(crate) fn checkpoint_digest(checkpoint: &super::Checkpoint) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(actor) = checkpoint.created_by_principal_id.as_ref() {
        parts.push(format!("actor:{}", actor));
    }
    if let Some(task_id) = checkpoint
        .provenance
        .as_ref()
        .and_then(|p| p.task_id.as_ref())
    {
        parts.push(format!("task:{}", task_id));
    }
    if let Some(subagent) = checkpoint
        .provenance
        .as_ref()
        .and_then(|p| p.subagent_type.as_ref())
    {
        parts.push(format!("subagent:{}", subagent));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn preview_payload(payload: &serde_json::Value) -> Option<String> {
    if let Some(content) = payload.get("content")
        && let Some(blocks) = content.as_array()
    {
        for block in blocks {
            if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                return Some(truncate_preview(text));
            }
        }
    }

    if let Some(summary) = payload.get("summary").and_then(serde_json::Value::as_str) {
        return Some(truncate_preview(summary));
    }

    if let Some(label) = payload.get("label").and_then(serde_json::Value::as_str) {
        return Some(truncate_preview(label));
    }

    None
}

fn divergence_from_primary(graph: &SessionGraph, branch_id: BranchId) -> Option<usize> {
    if branch_id == graph.primary_branch {
        return Some(0);
    }

    let primary = graph.current_branch_nodes(graph.primary_branch);
    let branch = graph.current_branch_nodes(branch_id);
    let shared = primary
        .iter()
        .zip(branch.iter())
        .take_while(|(left, right)| left.id == right.id)
        .count();
    Some(branch.len().saturating_sub(shared))
}

fn truncate_preview(text: &str) -> String {
    let mut chars = text.chars();
    let preview: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        format!("{}...", preview)
    } else {
        preview
    }
}

fn node_depth(graph: &SessionGraph, node_id: NodeId) -> usize {
    graph.node_depth(node_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{NodeKind, SessionGraph};

    #[test]
    fn explorer_lists_branches_and_tree() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(
                graph.primary_branch,
                NodeKind::User,
                serde_json::json!({"content": [{"type": "text", "text": "hello"}]}),
            )
            .unwrap();
        let branch = graph.fork_branch(Some(root), "experiment").unwrap();
        graph
            .append_node(
                branch,
                NodeKind::Assistant,
                serde_json::json!({"content": [{"type": "text", "text": "world"}]}),
            )
            .unwrap();
        graph
            .create_checkpoint(branch, "milestone", None, vec![], None, None)
            .unwrap();
        graph
            .create_bookmark(root, "start", None, None, None)
            .unwrap();

        let branches = GraphExplorer::list_branches(&graph);
        let tree = GraphExplorer::tree_view(&graph, branch);

        assert_eq!(branches.len(), 2);
        assert_eq!(branches[1].divergence_from_primary, Some(2));
        assert!(!tree.is_empty());
        assert!(tree.iter().any(|node| node.has_checkpoint));
    }

    #[test]
    fn explorer_renders_compact_and_verbose_tree() {
        let mut graph = SessionGraph::default();
        let root = graph
            .append_node(
                graph.primary_branch,
                NodeKind::User,
                serde_json::json!({"content": [{"type": "text", "text": "hello world"}]}),
            )
            .unwrap();
        graph
            .create_bookmark(root, "start", None, None, None)
            .unwrap();

        let compact =
            GraphExplorer::render_tree(&graph, graph.primary_branch, TreeRenderMode::Compact);
        let verbose =
            GraphExplorer::render_tree(&graph, graph.primary_branch, TreeRenderMode::Verbose);

        assert!(compact.contains("U@ user"));
        assert!(verbose.contains("hello world"));
    }
}
