use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::session::{Session, SessionId};
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
    const DESCRIPTION: &'static str = r#"Explore and navigate session graph history.

Actions:
- branches: List all branches with head nodes
- tree: Visual tree of nodes on a branch (optional tree_mode: compact|full)
- node: Inspect a single node by node_id
- search: Find nodes by query text, kind, tag, or principal_id
- stats: Session analytics (node count, branch count, depth)
- diff: Compare two branches (requires branch_id + other_branch_id)
- bookmarks: List bookmarks (optional branch_id filter)
- checkpoints: List checkpoints (optional branch_id filter)
- replay_bookmark / replay_checkpoint: Rebuild messages from a saved point (requires query)
- fork_bookmark / fork_checkpoint: Create new branch from a saved point (requires query)"#;

    async fn handle(&self, input: Self::Input, context: &ExecutionContext) -> ToolResult {
        let Some(manager) = context.session_manager() else {
            return ToolResult::error("Graph history requires a configured SessionManager");
        };

        let session_id = match resolve_session_id(&input, context) {
            Ok(id) => id,
            Err(error) => return ToolResult::error(error),
        };
        let session = match load_session(manager, &session_id, context).await {
            Ok(session) => session,
            Err(error) => return ToolResult::error(error.to_string()),
        };

        let output = match input.action {
            GraphHistoryAction::Branches => Ok(render_branches(
                &session.graph,
                input.follow_up_action.as_deref(),
            )),
            GraphHistoryAction::Tree => {
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                let mode = match input.tree_mode.unwrap_or(GraphTreeMode::Compact) {
                    GraphTreeMode::Compact => crate::graph::TreeRenderMode::Compact,
                    GraphTreeMode::Verbose => crate::graph::TreeRenderMode::Verbose,
                };
                Ok(crate::graph::GraphExplorer::render_tree(
                    &session.graph,
                    branch_id.unwrap_or(session.graph.primary_branch),
                    mode,
                ))
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
                crate::graph::GraphDiffService::branch_diff(&session.graph, left, right)
                    .map_err(|error| crate::session::SessionError::Storage {
                        message: error.to_string(),
                    })
                    .and_then(to_json)
            }
            GraphHistoryAction::ReplayBookmark => {
                let label = match input.query.as_deref() {
                    Some(label) => label,
                    None => return ToolResult::error("query is required for replay_bookmark"),
                };
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                replay_from_reference(
                    &session.graph,
                    crate::graph::GraphReferenceResolver::bookmark_by_label(
                        &session.graph,
                        label,
                        branch_id,
                    ),
                )
            }
            GraphHistoryAction::ReplayCheckpoint => {
                let label = match input.query.as_deref() {
                    Some(label) => label,
                    None => return ToolResult::error("query is required for replay_checkpoint"),
                };
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                replay_from_reference(
                    &session.graph,
                    crate::graph::GraphReferenceResolver::checkpoint_by_label(
                        &session.graph,
                        label,
                        branch_id,
                    ),
                )
            }
            GraphHistoryAction::ForkBookmark => {
                let label = match input.query.as_deref() {
                    Some(label) => label,
                    None => return ToolResult::error("query is required for fork_bookmark"),
                };
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                fork_from_reference(
                    manager,
                    &session,
                    context,
                    crate::graph::GraphReferenceResolver::bookmark_by_label(
                        &session.graph,
                        label,
                        branch_id,
                    ),
                )
                .await
            }
            GraphHistoryAction::ForkCheckpoint => {
                let label = match input.query.as_deref() {
                    Some(label) => label,
                    None => return ToolResult::error("query is required for fork_checkpoint"),
                };
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                fork_from_reference(
                    manager,
                    &session,
                    context,
                    crate::graph::GraphReferenceResolver::checkpoint_by_label(
                        &session.graph,
                        label,
                        branch_id,
                    ),
                )
                .await
            }
            GraphHistoryAction::Bookmarks => {
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                let bookmarks = crate::graph::GraphExplorer::bookmarks(&session.graph, branch_id);
                Ok(render_bookmarks(&bookmarks))
            }
            GraphHistoryAction::Checkpoints => {
                let branch_id = match parse_optional_uuid(input.branch_id.as_deref()) {
                    Ok(branch_id) => branch_id,
                    Err(error) => return ToolResult::error(error),
                };
                let checkpoints =
                    crate::graph::GraphExplorer::checkpoints(&session.graph, branch_id);
                Ok(render_checkpoints(&checkpoints))
            }
            GraphHistoryAction::Node => {
                let node_id = match parse_optional_uuid(input.node_id.as_deref()) {
                    Ok(node_id) => node_id,
                    Err(error) => return ToolResult::error(error),
                };
                let Some(node_id) = node_id else {
                    return ToolResult::error("node_id is required for action=node");
                };
                crate::graph::GraphExplorer::node_summary(&session.graph, node_id)
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
                    .ok_or_else(|| crate::session::SessionError::Storage {
                        message: format!("Node {node_id} not found"),
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
                let matches = crate::graph::GraphSearchService::search(
                    &session.graph,
                    &crate::graph::GraphSearchQuery {
                        text: input.query.clone(),
                        branch_id,
                        kind,
                        tag: input.tag.clone(),
                        principal_id: input.principal_id.clone(),
                        session_type: input.session_type.clone(),
                        subagent_type: input.subagent_type.clone(),
                    },
                );
                let mut output = serde_json::to_string_pretty(&matches).unwrap_or_default();
                if let Some(action) = input.follow_up_action.as_deref() {
                    output.push_str(&format!("\n\nnext_action_hint: {}", action));
                }
                Ok(output)
            }
            GraphHistoryAction::Stats => {
                to_json(crate::graph::GraphSearchService::stats(&session.graph))
            }
        };

        match output {
            Ok(output) => ToolResult::success(output),
            Err(error) => ToolResult::error(error.to_string()),
        }
    }
}

async fn load_session(
    manager: &crate::session::SessionManager,
    session_id: &SessionId,
    context: &ExecutionContext,
) -> crate::session::SessionResult<Session> {
    match context.session_scope_ref() {
        Some(scope) => manager.get_scoped(session_id, scope).await,
        None => manager.get(session_id).await,
    }
}

fn resolve_session_id(
    input: &GraphHistoryInput,
    context: &ExecutionContext,
) -> Result<SessionId, String> {
    if let Some(session_id) = input.session_id.as_ref() {
        SessionId::parse(session_id)
            .ok_or_else(|| format!("session_id must be a valid UUID, got '{session_id}'"))
    } else if let Some(session_id) = context.session_id() {
        SessionId::parse(session_id)
            .ok_or_else(|| format!("bound session_id must be a valid UUID, got '{session_id}'"))
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

fn replay_from_reference(
    graph: &crate::graph::SessionGraph,
    reference: Result<crate::graph::GraphReference, String>,
) -> crate::session::SessionResult<String> {
    let reference =
        reference.map_err(|message| crate::session::SessionError::Storage { message })?;
    crate::session::ReplayService::replay_input(
        graph,
        Some(crate::graph::GraphReferenceResolver::node_id(&reference)),
    )
    .and_then(to_json)
}

async fn fork_from_reference(
    manager: &crate::session::SessionManager,
    session: &Session,
    context: &ExecutionContext,
    reference: Result<crate::graph::GraphReference, String>,
) -> crate::session::SessionResult<String> {
    let reference =
        reference.map_err(|message| crate::session::SessionError::Storage { message })?;
    let node_id = crate::graph::GraphReferenceResolver::node_id(&reference);
    let forked = match context.session_scope_ref() {
        Some(scope) => {
            manager
                .fork_from_node_scoped(&session.id, scope, node_id)
                .await?
        }
        None => manager.fork_from_node(&session.id, node_id).await?,
    };
    Ok(forked.id.to_string())
}

fn render_branches(graph: &crate::graph::SessionGraph, follow_up_action: Option<&str>) -> String {
    let branches = crate::graph::GraphExplorer::list_branches(graph);
    let enriched: Vec<_> = branches
        .iter()
        .map(|branch| {
            serde_json::json!({
                "summary": branch,
                "digest": crate::graph::ProvenanceSummaryService::branch_digest(branch),
            })
        })
        .collect();
    if let Some(action) = follow_up_action {
        format!(
            "{}\n\nnext_action_hint: {}",
            serde_json::to_string_pretty(&enriched).unwrap_or_default(),
            action
        )
    } else {
        serde_json::to_string_pretty(&enriched).unwrap_or_default()
    }
}

fn render_bookmarks(bookmarks: &[crate::graph::Bookmark]) -> String {
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
}

fn render_checkpoints(checkpoints: &[crate::graph::Checkpoint]) -> String {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionAccessScope, SessionConfig, SessionManager, SessionMessage};
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

        let context = ExecutionContext::permissive().with_session_manager(manager);
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

        let context = ExecutionContext::permissive().with_session_manager(manager);
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

    #[tokio::test]
    async fn graph_history_tool_enforces_scope_when_bound() {
        let manager = SessionManager::in_memory();
        let allowed = manager
            .create_with_identity(SessionConfig::default(), "tenant-a", "user-1")
            .await
            .unwrap();
        let denied = manager
            .create_with_identity(SessionConfig::default(), "tenant-a", "user-2")
            .await
            .unwrap();

        let context = ExecutionContext::permissive()
            .with_session_manager(manager)
            .session_scope(
                SessionAccessScope::default()
                    .tenant("tenant-a")
                    .principal("user-1"),
            );
        let tool = GraphHistoryTool;
        let denied_result = tool
            .execute(
                serde_json::json!({
                    "action": "stats",
                    "session_id": denied.id.to_string()
                }),
                &context,
            )
            .await;
        let allowed_result = tool
            .execute(
                serde_json::json!({
                    "action": "stats",
                    "session_id": allowed.id.to_string()
                }),
                &context,
            )
            .await;

        assert!(denied_result.is_error());
        assert!(!allowed_result.is_error());
    }

    #[tokio::test]
    async fn graph_history_tool_rejects_invalid_session_id() {
        let manager = SessionManager::in_memory();
        let context = ExecutionContext::permissive().with_session_manager(manager);
        let tool = GraphHistoryTool;
        let result = tool
            .execute(
                serde_json::json!({
                    "action": "stats",
                    "session_id": "not-a-uuid"
                }),
                &context,
            )
            .await;

        assert!(result.is_error());
        assert!(
            result
                .error_message()
                .contains("session_id must be a valid UUID")
        );
    }
}
