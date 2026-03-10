use serde::{Deserialize, Serialize};

use crate::graph::{BranchExport, SessionGraph};
use crate::session::Session;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPolicy {
    pub include_identity: bool,
    pub include_provenance: bool,
    pub include_tool_payloads: bool,
}

impl Default for ExportPolicy {
    fn default() -> Self {
        Self {
            include_identity: true,
            include_provenance: true,
            include_tool_payloads: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditBundle {
    pub session_id: String,
    pub tenant_id: Option<String>,
    pub principal_id: Option<String>,
    pub branch_id: uuid::Uuid,
    pub branch_name: String,
    pub stats: crate::graph::GraphSessionStats,
    pub provenance_digest: Vec<String>,
    pub export: BranchExport,
}

pub struct SessionExporter;

impl SessionExporter {
    pub fn export_branch(
        graph: &SessionGraph,
        branch_id: crate::graph::BranchId,
    ) -> Option<BranchExport> {
        graph.export_branch(branch_id)
    }

    pub fn export_branch_with_policy(
        graph: &SessionGraph,
        branch_id: crate::graph::BranchId,
        policy: &ExportPolicy,
    ) -> Option<BranchExport> {
        let mut export = graph.export_branch(branch_id)?;
        apply_policy(&mut export, policy);
        Some(export)
    }

    pub fn branch_to_json(export: &BranchExport) -> crate::Result<String> {
        serde_json::to_string_pretty(export).map_err(crate::Error::from)
    }

    pub fn branch_to_html(export: &BranchExport) -> String {
        let mut html = String::from(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>Session Export</title><style>body{font-family:ui-monospace,monospace;max-width:1080px;margin:40px auto;padding:0 16px;background:#fafafa;color:#111}h1,h2{margin:0 0 16px}ul{padding-left:18px}.summary{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:12px;margin:20px 0}.card{background:#fff;border:1px solid #ddd;border-radius:10px;padding:12px}.node{background:#fff;border:1px solid #ddd;border-radius:10px;padding:12px;margin:12px 0}.meta{color:#666;font-size:12px;margin-bottom:8px}.tags{margin:8px 0 0;color:#444;font-size:12px}pre{white-space:pre-wrap;word-break:break-word}</style></head><body>",
        );
        html.push_str(&format!("<h1>Branch: {}</h1>", export.branch_name));
        html.push_str("<div class=\"summary\">");
        html.push_str(&format!(
            "<div class=\"card\"><strong>Branch ID</strong><br>{}</div>",
            export.branch_id
        ));
        html.push_str(&format!(
            "<div class=\"card\"><strong>Head</strong><br>{}</div>",
            export
                .head
                .map(|id| id.to_string())
                .unwrap_or_else(|| "none".to_string())
        ));
        html.push_str(&format!(
            "<div class=\"card\"><strong>Nodes</strong><br>{}</div>",
            export.nodes.len()
        ));
        html.push_str("</div>");
        if !export.checkpoints.is_empty() {
            html.push_str("<h2>Checkpoints</h2><ul>");
            for checkpoint in &export.checkpoints {
                html.push_str(&format!(
                    "<li><strong>{}</strong> - {}{}</li>",
                    html_escape(&checkpoint.label),
                    checkpoint.created_at,
                    checkpoint
                        .note
                        .as_ref()
                        .map(|note| format!(" - {}", html_escape(note)))
                        .unwrap_or_default()
                ));
            }
            html.push_str("</ul>");
        }
        if !export.bookmarks.is_empty() {
            html.push_str("<h2>Bookmarks</h2><ul>");
            for bookmark in &export.bookmarks {
                html.push_str(&format!(
                    "<li><strong>{}</strong> - node {}{}</li>",
                    html_escape(&bookmark.label),
                    bookmark.node_id,
                    bookmark
                        .note
                        .as_ref()
                        .map(|note| format!(" - {}", html_escape(note)))
                        .unwrap_or_default()
                ));
            }
            html.push_str("</ul>");
        }
        if !export.tree.is_empty() {
            html.push_str("<h2>Tree</h2><pre>");
            for node in &export.tree {
                html.push_str(&html_escape(&format!(
                    "{}- {:?} {}\n",
                    "  ".repeat(node.depth),
                    node.kind,
                    node.id
                )));
            }
            html.push_str("</pre>");
        }
        html.push_str("<h2>Timeline</h2>");
        for node in &export.nodes {
            html.push_str("<div class=\"node\">");
            html.push_str(&format!(
                "<div class=\"meta\">{} | {:?} | {}</div>",
                node.id, node.kind, node.created_at
            ));
            html.push_str("<pre>");
            html.push_str(&html_escape(
                &serde_json::to_string_pretty(&node.payload).unwrap_or_default(),
            ));
            html.push_str("</pre>");
            if !node.tags.is_empty() {
                html.push_str(&format!(
                    "<div class=\"tags\">tags: {}</div>",
                    html_escape(&node.tags.join(", "))
                ));
            }
            html.push_str("</div>");
        }
        html.push_str("</body></html>");
        html
    }

    pub fn audit_bundle(session: &Session, policy: &ExportPolicy) -> Option<AuditBundle> {
        let export =
            Self::export_branch_with_policy(&session.graph, session.graph.primary_branch, policy)?;
        let stats = crate::graph::GraphSearchService::stats(&session.graph);
        Some(AuditBundle {
            session_id: session.id.to_string(),
            tenant_id: policy
                .include_identity
                .then(|| session.tenant_id.clone())
                .flatten(),
            principal_id: policy
                .include_identity
                .then(|| session.principal_id.clone())
                .flatten(),
            branch_id: export.branch_id,
            branch_name: export.branch_name.clone(),
            stats,
            provenance_digest: build_provenance_digest(&export),
            export,
        })
    }
}

fn apply_policy(export: &mut BranchExport, policy: &ExportPolicy) {
    if !policy.include_identity {
        for node in &mut export.nodes {
            node.created_by_principal_id = None;
        }
        for checkpoint in &mut export.checkpoints {
            checkpoint.created_by_principal_id = None;
        }
        for bookmark in &mut export.bookmarks {
            bookmark.created_by_principal_id = None;
        }
    }

    if !policy.include_provenance {
        for node in &mut export.nodes {
            node.provenance = None;
            node.provenance_digest = None;
        }
        for checkpoint in &mut export.checkpoints {
            checkpoint.provenance = None;
            checkpoint.provenance_digest = None;
        }
        for bookmark in &mut export.bookmarks {
            bookmark.provenance = None;
            bookmark.provenance_digest = None;
        }
    }

    if !policy.include_tool_payloads {
        for node in &mut export.nodes {
            if matches!(
                node.kind,
                crate::graph::NodeKind::ToolCall | crate::graph::NodeKind::ToolResult
            ) {
                node.payload = serde_json::json!({"redacted": true});
            }
        }
    }
}

fn build_provenance_digest(export: &BranchExport) -> Vec<String> {
    let mut lines = Vec::new();
    let principal_count = export
        .nodes
        .iter()
        .filter_map(|node| node.created_by_principal_id.as_ref())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if principal_count > 0 {
        lines.push(format!("principals:{}", principal_count));
    }

    let subagent_count = export
        .nodes
        .iter()
        .filter_map(|node| node.provenance.as_ref())
        .filter_map(|provenance| provenance.subagent_type.as_ref())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if subagent_count > 0 {
        lines.push(format!("subagent_types:{}", subagent_count));
    }

    let task_count = export
        .nodes
        .iter()
        .filter_map(|node| node.provenance.as_ref())
        .filter_map(|provenance| provenance.task_id.as_ref())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if task_count > 0 {
        lines.push(format!("tasks:{}", task_count));
    }

    lines
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
