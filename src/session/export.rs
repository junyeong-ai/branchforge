use crate::graph::BranchExport;

pub fn branch_export_to_json(export: &BranchExport) -> crate::Result<String> {
    serde_json::to_string_pretty(export).map_err(crate::Error::from)
}

pub fn branch_export_to_html(export: &BranchExport) -> String {
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

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
