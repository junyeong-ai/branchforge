use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::session::SessionId;
use crate::tools::{ExecutionContext, SchemaTool};
use crate::types::ToolResult;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphTreeMode {
    Compact,
    Verbose,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphHistoryAction {
    Branches,
    Diff,
    ReplayBookmark,
    ReplayCheckpoint,
    ForkBookmark,
    ForkCheckpoint,
    Tree,
    Bookmarks,
    Checkpoints,
    Node,
    Search,
    Stats,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GraphHistoryInput {
    pub action: GraphHistoryAction,
    pub session_id: Option<String>,
    pub branch_id: Option<String>,
    pub other_branch_id: Option<String>,
    pub node_id: Option<String>,
    pub query: Option<String>,
    pub kind: Option<String>,
    pub tag: Option<String>,
    pub principal_id: Option<String>,
    pub session_type: Option<String>,
    pub subagent_type: Option<String>,
    pub tree_mode: Option<GraphTreeMode>,
    pub follow_up_action: Option<String>,
}

pub struct GraphHistoryTool;

#[async_trait]
impl SchemaTool for GraphHistoryTool {
    type Input = GraphHistoryInput;

    const NAME: &'static str = "GraphHistory";
    const DESCRIPTION: &'static str = "Explore graph-first session history: branches, tree views, bookmarks, checkpoints, node summaries, search, diff, jump workflows, and graph analytics.";

    async fn handle(&self, input: Self::Input, context: &ExecutionContext) -> ToolResult {
        let Some(manager) = context.graph_manager() else {
            return ToolResult::error("Graph history requires a configured SessionManager");
        };

        let session_id = match resolve_session_id(&input, context) {
            Ok(id) => id,
            Err(error) => return ToolResult::error(error),
        };

        let output = match input.action {
            GraphHistoryAction::Branches => {
                manager.graph_branches(&session_id).await.map(|branches| {
                    let enriched: Vec<_> = branches
                        .iter()
                        .map(|branch| {
                            serde_json::json!({
                                "summary": branch,
                                "digest": crate::graph::ProvenanceSummaryService::branch_digest(branch),
                            })
                        })
                        .collect();
                    if let Some(action) = input.follow_up_action.as_deref() {
                        format!(
                            "{}\n\nnext_action_hint: {}",
                            serde_json::to_string_pretty(&enriched).unwrap_or_default(),
                            action
                        )
                    } else {
                        serde_json::to_string_pretty(&enriched).unwrap_or_default()
                    }
                })
            }
            GraphHistoryAction::Tree => {
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                let mode = match input.tree_mode.unwrap_or(GraphTreeMode::Compact) {
                    GraphTreeMode::Compact => crate::graph::TreeRenderMode::Compact,
                    GraphTreeMode::Verbose => crate::graph::TreeRenderMode::Verbose,
                };
                manager
                    .graph_tree_rendered(&session_id, branch_id, mode)
                    .await
            }
            GraphHistoryAction::Diff => {
                let left = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(left) => left,
                    Err(error) => return ToolResult::error(error),
                };
                let right = match parse_optional_uuid(input.other_branch_id.as_deref()) {
                    Ok(right) => right,
                    Err(error) => return ToolResult::error(error),
                };
                let Some(left) = left else {
                    return ToolResult::error("branch_id is required for action=diff");
                };
                let Some(right) = right else {
                    return ToolResult::error("other_branch_id is required for action=diff");
                };
                manager
                    .graph_branch_diff(&session_id, left, right)
                    .await
                    .and_then(to_json)
            }
            GraphHistoryAction::ReplayBookmark => {
                let label = input
                    .query
                    .as_deref()
                    .ok_or_else(|| "query is required for replay_bookmark".to_string());
                let Ok(label) = label else {
                    return ToolResult::error(label.err().unwrap_or_default());
                };
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                manager
                    .replay_from_bookmark(&session_id, label, branch_id)
                    .await
                    .and_then(to_json)
            }
            GraphHistoryAction::ReplayCheckpoint => {
                let label = input
                    .query
                    .as_deref()
                    .ok_or_else(|| "query is required for replay_checkpoint".to_string());
                let Ok(label) = label else {
                    return ToolResult::error(label.err().unwrap_or_default());
                };
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                manager
                    .replay_from_checkpoint(&session_id, label, branch_id)
                    .await
                    .and_then(to_json)
            }
            GraphHistoryAction::ForkBookmark => {
                let label = input
                    .query
                    .as_deref()
                    .ok_or_else(|| "query is required for fork_bookmark".to_string());
                let Ok(label) = label else {
                    return ToolResult::error(label.err().unwrap_or_default());
                };
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                manager
                    .fork_from_bookmark(&session_id, label, branch_id)
                    .await
                    .map(|session| session.id.to_string())
            }
            GraphHistoryAction::ForkCheckpoint => {
                let label = input
                    .query
                    .as_deref()
                    .ok_or_else(|| "query is required for fork_checkpoint".to_string());
                let Ok(label) = label else {
                    return ToolResult::error(label.err().unwrap_or_default());
                };
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                manager
                    .fork_from_checkpoint(&session_id, label, branch_id)
                    .await
                    .map(|session| session.id.to_string())
            }
            GraphHistoryAction::Bookmarks => {
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                manager
                    .graph_bookmarks(&session_id, branch_id)
                    .await
                    .map(|bookmarks| {
                        let enriched: Vec<_> = bookmarks
                            .iter()
                            .map(|bookmark| {
                                serde_json::json!({
                                    "bookmark": bookmark,
                                    "digest": crate::graph::explorer::bookmark_digest(bookmark),
                                    "actions": {
                                        "replay": format!("replay_bookmark:{}", bookmark.label),
                                        "fork": format!("fork_bookmark:{}", bookmark.label),
                                    }
                                })
                            })
                            .collect();
                        serde_json::to_string_pretty(&enriched).unwrap_or_default()
                    })
            }
            GraphHistoryAction::Checkpoints => {
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                manager
                    .graph_checkpoints(&session_id, branch_id)
                    .await
                    .map(|checkpoints| {
                        let enriched: Vec<_> = checkpoints
                            .iter()
                            .map(|checkpoint| {
                                serde_json::json!({
                                    "checkpoint": checkpoint,
                                    "digest": crate::graph::explorer::checkpoint_digest(checkpoint),
                                    "actions": {
                                        "replay": format!("replay_checkpoint:{}", checkpoint.label),
                                        "fork": format!("fork_checkpoint:{}", checkpoint.label),
                                    }
                                })
                            })
                            .collect();
                        serde_json::to_string_pretty(&enriched).unwrap_or_default()
                    })
            }
            GraphHistoryAction::Node => {
                let node_id = match parse_optional_uuid(input.node_id.as_deref()) {
                    Ok(node_id) => node_id,
                    Err(error) => return ToolResult::error(error),
                };
                let Some(node_id) = node_id else {
                    return ToolResult::error("node_id is required for action=node");
                };
                manager
                    .graph_node(&session_id, node_id)
                    .await
                    .map(|node| {
                        serde_json::to_string_pretty(&serde_json::json!({
                            "node": node,
                            "actions": {
                                "replay_from_node": node_id.to_string(),
                                "fork_from_node": node_id.to_string(),
                            }
                        }))
                        .unwrap_or_default()
                    })
            }
            GraphHistoryAction::Search => {
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                let kind = match input.kind.as_deref().map(parse_node_kind).transpose() {
                    Ok(kind) => kind,
                    Err(error) => return ToolResult::error(error),
                };
                manager
                    .graph_search(
                        &session_id,
                        &crate::graph::GraphSearchQuery {
                            text: input.query.clone(),
                            branch_id,
                            kind,
                            tag: input.tag.clone(),
                            principal_id: input.principal_id.clone(),
                            session_type: input.session_type.clone(),
                            subagent_type: input.subagent_type.clone(),
                        },
                    )
                    .await
                    .map(|matches| {
                        let mut output = serde_json::to_string_pretty(&matches).unwrap_or_default();
                        if let Some(action) = input.follow_up_action.as_deref() {
                            output.push_str(&format!("\n\nnext_action_hint: {}", action));
                        }
                        output
                    })
            }
            GraphHistoryAction::Stats => manager.graph_stats(&session_id).await.and_then(to_json),
        };

        match output {
            Ok(output) => ToolResult::success(output),
            Err(error) => ToolResult::error(error.to_string()),
        }
    }
}

fn resolve_session_id(
    input: &GraphHistoryInput,
    context: &ExecutionContext,
) -> Result<SessionId, String> {
    if let Some(session_id) = input.session_id.as_ref() {
        Ok(SessionId::from(session_id.clone()))
    } else if let Some(session_id) = context.session_id() {
        Ok(SessionId::from(session_id.to_string()))
    } else {
        Err(
            "session_id is required when no active session is bound to the tool context"
                .to_string(),
        )
    }
}

fn parse_optional_uuid<T>(value: Option<&str>) -> Result<Option<T>, String>
where
    T: From<uuid::Uuid>,
{
    value
        .map(|value| {
            uuid::Uuid::parse_str(value)
                .map(T::from)
                .map_err(|e| e.to_string())
        })
        .transpose()
}

fn parse_node_kind(value: &str) -> Result<crate::graph::NodeKind, String> {
    serde_json::from_str(&format!("\"{}\"", value)).map_err(|e| e.to_string())
}

fn to_json<T: Serialize>(value: T) -> crate::session::SessionResult<String> {
    serde_json::to_string_pretty(&value).map_err(crate::session::SessionError::Serialization)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionConfig, SessionManager, SessionMessage};
    use crate::tools::Tool;
    use crate::types::ContentBlock;

    #[tokio::test]
    async fn graph_history_tool_uses_bound_session_manager() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        manager
            .add_message(
                &session.id,
                SessionMessage::user(vec![ContentBlock::text("alpha")]),
            )
            .await
            .unwrap();

        let context = ExecutionContext::permissive().session_manager(manager);
        let tool = GraphHistoryTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "action": "search",
                    "session_id": session.id.to_string(),
                    "query": "alpha"
                }),
                &context,
            )
            .await;

        assert!(!result.is_error());
    }

    #[tokio::test]
    async fn graph_history_tool_renders_compact_tree() {
        let manager = SessionManager::in_memory();
        let session = manager.create(SessionConfig::default()).await.unwrap();
        manager
            .add_message(
                &session.id,
                SessionMessage::user(vec![ContentBlock::text("alpha")]),
            )
            .await
            .unwrap();

        let context = ExecutionContext::permissive().session_manager(manager);
        let tool = GraphHistoryTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "action": "tree",
                    "session_id": session.id.to_string(),
                    "tree_mode": "compact"
                }),
                &context,
            )
            .await;

        assert!(!result.is_error());
    }
}
