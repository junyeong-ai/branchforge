//! Common agent execution utilities shared between execution and streaming.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rust_decimal::Decimal;
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::ToolRegistry;
use crate::budget::{BudgetTracker, TenantBudget};
use crate::context::PromptOrchestrator;
use crate::hooks::{HookContext, HookEvent, HookInput, HookManager};
use crate::session::{ToolExecution, ToolState};
use crate::types::{CompactResult, ContentBlock, ToolResult, ToolResultBlock, ToolUseBlock, Usage};

use super::config::{BudgetConfig, ExecutionConfig};
use super::state::AgentMetrics;
use super::state_formatter::collect_compaction_state;

/// Extract structured output from text if an output schema is configured.
pub(crate) fn extract_structured_output(schema: Option<&Value>, text: &str) -> Option<Value> {
    schema?;
    serde_json::from_str(text).ok()
}

pub struct BudgetContext<'a> {
    pub tracker: &'a BudgetTracker,
    pub tenant: Option<&'a TenantBudget>,
    pub config: &'a BudgetConfig,
}

impl BudgetContext<'_> {
    pub fn check(&self) -> Result<(), crate::Error> {
        if self.tracker.should_stop() {
            let status = self.tracker.check();
            warn!(used = %status.used(), "Budget exceeded, stopping execution");
            return Err(crate::Error::BudgetExceeded {
                used: status.used(),
                limit: self.config.max_cost_usd.unwrap_or(Decimal::ZERO),
            });
        }

        if let Some(fallback_model) = self.tracker.should_fallback() {
            warn!(
                model = %fallback_model,
                used = %self.tracker.used_cost_usd(),
                "Budget exceeded, should switch to fallback model"
            );
        }

        if let Some(tenant_budget) = self.tenant
            && tenant_budget.should_stop()
        {
            warn!(
                tenant_id = %tenant_budget.tenant_id,
                used = %tenant_budget.used_cost_usd(),
                "Tenant budget exceeded, stopping execution"
            );
            return Err(crate::Error::BudgetExceeded {
                used: tenant_budget.used_cost_usd(),
                limit: tenant_budget.max_cost_usd(),
            });
        }

        Ok(())
    }

    pub fn fallback_model(&self) -> Option<&str> {
        self.tracker.should_fallback()
    }
}

/// Accumulate usage from an API response into total_usage, metrics, and budget.
pub(crate) fn accumulate_response_usage(
    total_usage: &mut Usage,
    metrics: &mut AgentMetrics,
    budget_tracker: &BudgetTracker,
    tenant_budget: Option<&TenantBudget>,
    model: &str,
    usage: &Usage,
) -> Decimal {
    total_usage.input_tokens = total_usage.input_tokens.saturating_add(usage.input_tokens);
    total_usage.output_tokens = total_usage
        .output_tokens
        .saturating_add(usage.output_tokens);
    metrics.add_usage_with_cache(usage);
    metrics.record_model_usage(model, usage);

    if let Some(ref server_usage) = usage.server_tool_use {
        metrics.update_server_tool_use_from_api(server_usage);
    }

    let cost = budget_tracker.record(model, usage);
    metrics.add_cost(cost);

    if let Some(tenant_budget) = tenant_budget {
        tenant_budget.record(model, usage);
    }

    cost
}

/// Accumulate inner usage from a tool result (e.g., subagent calls).
pub(crate) async fn accumulate_inner_usage(
    tool_state: &ToolState,
    total_usage: &mut Usage,
    metrics: &mut AgentMetrics,
    budget_tracker: &BudgetTracker,
    result: &ToolResult,
    tool_name: &str,
) {
    if let Some(ref inner_usage) = result.inner_usage {
        tool_state
            .with_session_mut(|session| {
                session.update_usage(inner_usage);
            })
            .await;
        total_usage.input_tokens = total_usage
            .input_tokens
            .saturating_add(inner_usage.input_tokens);
        total_usage.output_tokens = total_usage
            .output_tokens
            .saturating_add(inner_usage.output_tokens);
        metrics.add_usage_with_cache(inner_usage);
        let inner_model = result.inner_model.as_deref().unwrap_or("claude-haiku-4-5");
        metrics.record_model_usage(inner_model, inner_usage);

        let inner_cost = budget_tracker.record(inner_model, inner_usage);
        metrics.add_cost(inner_cost);

        debug!(
            tool = %tool_name,
            model = %inner_model,
            input_tokens = inner_usage.input_tokens,
            output_tokens = inner_usage.output_tokens,
            cost_usd = %inner_cost,
            "Accumulated inner usage from tool"
        );
    }
}

pub(crate) async fn maybe_invoke_explicit_skill_command(
    tools: &ToolRegistry,
    tool_state: &ToolState,
    hooks: &HookManager,
    hook_ctx: &HookContext,
    session_id: &str,
    prompt: &str,
    metrics: &mut AgentMetrics,
) -> crate::Result<bool> {
    let Some(tool) = tools.get("Skill") else {
        return Ok(false);
    };
    let Some(skill_tool) = tool.as_any().downcast_ref::<crate::skills::SkillTool>() else {
        return Ok(false);
    };
    let Some(input) = skill_tool.resolve_explicit_command(prompt).await else {
        return Ok(false);
    };

    let raw_input = serde_json::to_value(&input).map_err(|e| {
        crate::Error::Config(format!(
            "Failed to serialize explicit skill invocation: {e}"
        ))
    })?;
    let pre_input = HookInput::pre_tool_use(session_id, "Skill", raw_input.clone());
    let pre_output = hooks
        .execute(HookEvent::PreToolUse, pre_input, hook_ctx)
        .await?;

    if !pre_output.continue_execution {
        return Err(crate::Error::Authorization(
            pre_output
                .stop_reason
                .unwrap_or_else(|| "Blocked by hook".into()),
        ));
    }

    let actual_input = pre_output.updated_input.unwrap_or(raw_input);
    let permission = tools
        .get_context()
        .check_explicit_skill_permission(&actual_input);
    if !permission.is_allowed() {
        return Err(crate::Error::Authorization(permission.reason));
    }

    let typed_input: crate::skills::SkillInput = serde_json::from_value(actual_input.clone())
        .map_err(|e| crate::Error::Config(format!("Invalid explicit skill input: {e}")))?;

    let start = Instant::now();
    let result = Box::pin(skill_tool.execute_explicit_input(typed_input)).await;
    let duration_ms = start.elapsed().as_millis() as u64;
    let is_error = result.is_error();
    let tool_use_id = format!("skill_{}", uuid::Uuid::new_v4().simple());

    run_post_tool_hooks(hooks, hook_ctx, session_id, "Skill", is_error, &result).await;
    metrics.record_tool(&tool_use_id, "Skill", duration_ms, is_error);

    tool_state
        .record_tool_execution(
            ToolExecution::new(tool_state.session_id(), "Skill", actual_input.clone())
                .message(tool_use_id.clone())
                .output(result.output.text(), is_error)
                .duration(duration_ms),
        )
        .await?;

    tool_state
        .with_session_mut(|session| {
            session.add_assistant_message(
                vec![ContentBlock::ToolUse(ToolUseBlock {
                    id: tool_use_id.clone(),
                    name: "Skill".to_string(),
                    input: actual_input.clone(),
                })],
                None,
            );
            session.add_tool_results(vec![ToolResultBlock::from_tool_result(
                &tool_use_id,
                &result,
            )]);
        })
        .await;

    Ok(true)
}

/// Run post-tool hooks (PostToolUse on success, PostToolUseFailure on error).
pub(crate) async fn run_post_tool_hooks(
    hooks: &HookManager,
    hook_ctx: &HookContext,
    session_id: &str,
    tool_name: &str,
    is_error: bool,
    result: &ToolResult,
) {
    if is_error {
        let failure_input =
            HookInput::post_tool_use_failure(session_id, tool_name, result.error_message());
        if let Err(e) = hooks
            .execute(HookEvent::PostToolUseFailure, failure_input, hook_ctx)
            .await
        {
            warn!(tool = %tool_name, error = %e, "PostToolUseFailure hook failed");
        }
    } else {
        let post_input = HookInput::post_tool_use(session_id, tool_name, result.output.clone());
        if let Err(e) = hooks
            .execute(HookEvent::PostToolUse, post_input, hook_ctx)
            .await
        {
            warn!(tool = %tool_name, error = %e, "PostToolUse hook failed");
        }
    }
}

/// Activate dynamic rules for file-related tool operations.
pub(crate) async fn try_activate_dynamic_rules(
    tool_name: &str,
    input: &Value,
    orchestrator: &Option<Arc<RwLock<PromptOrchestrator>>>,
    dynamic_rules: &mut String,
) {
    if let Some(file_path) = extract_file_path(tool_name, input)
        && let Some(orchestrator) = orchestrator
    {
        let new_rules = activate_rules_for_file(orchestrator, &file_path).await;
        if !new_rules.is_empty() {
            *dynamic_rules = build_dynamic_rules_context(orchestrator, &file_path).await;
            debug!(rules = ?new_rules, "Activated rules for file");
        }
    }
}

/// Run Stop and SessionEnd hooks in sequence.
pub(crate) async fn run_stop_hooks(hooks: &HookManager, hook_ctx: &HookContext, session_id: &str) {
    let stop_input = HookInput::stop(session_id);
    if let Err(e) = hooks.execute(HookEvent::Stop, stop_input, hook_ctx).await {
        warn!(error = %e, "Stop hook failed");
    }

    let session_end_input = HookInput::session_end(session_id);
    if let Err(e) = hooks
        .execute(HookEvent::SessionEnd, session_end_input, hook_ctx)
        .await
    {
        warn!(error = %e, "SessionEnd hook failed");
    }
}

/// Check whether compaction is needed and perform it if so.
pub(crate) async fn handle_compaction(
    tool_state: &ToolState,
    client: &crate::Client,
    tools: &ToolRegistry,
    hooks: &HookManager,
    hook_ctx: &HookContext,
    session_id: &str,
    config: &ExecutionConfig,
    max_tokens: u64,
    metrics: &mut AgentMetrics,
) {
    let should_compact = tool_state
        .with_session(|session| {
            config.auto_compact && session.should_compact(max_tokens, config.compact_threshold)
        })
        .await;

    if !should_compact {
        return;
    }

    let pre_compact_input = HookInput::pre_compact(session_id);
    if let Err(e) = hooks
        .execute(HookEvent::PreCompact, pre_compact_input, hook_ctx)
        .await
    {
        warn!(error = %e, "PreCompact hook failed");
    }

    debug!("Compacting session context");
    let compact_result = tool_state.compact(client).await;

    match compact_result {
        Ok(CompactResult::Compacted { saved_tokens, .. }) => {
            info!(saved_tokens, "Session context compacted");
            metrics.record_compaction();

            let state_sections = collect_compaction_state(tools).await;
            if !state_sections.is_empty() {
                tool_state
                    .with_session_mut(|session| {
                        session.add_user_message(format!(
                            "<system-reminder>\n# State preserved after compaction\n\n{}\n</system-reminder>",
                            state_sections.join("\n\n")
                        ));
                    })
                    .await;
            }
        }
        Ok(CompactResult::NotNeeded | CompactResult::Skipped { .. }) => {
            debug!("Compaction skipped or not needed");
        }
        Err(e) => {
            warn!(error = %e, "Session compaction failed");
        }
    }
}

/// Extract file path from tool input for rule activation.
pub(crate) fn extract_file_path(tool_name: &str, input: &Value) -> Option<String> {
    match tool_name {
        "Read" | "Write" | "Edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(String::from),
        "Glob" | "Grep" => input.get("path").and_then(|v| v.as_str()).map(String::from),
        _ => None,
    }
}

pub(crate) async fn activate_rules_for_file(
    orchestrator: &Arc<RwLock<PromptOrchestrator>>,
    file_path: &str,
) -> Vec<String> {
    let orch = orchestrator.read().await;
    let path = Path::new(file_path);
    let rules = orch.find_matching_rules(path).await;
    rules.iter().map(|r| r.name.clone()).collect()
}

pub(crate) async fn build_dynamic_rules_context(
    orchestrator: &Arc<RwLock<PromptOrchestrator>>,
    file_path: &str,
) -> String {
    let orch = orchestrator.read().await;
    let path = Path::new(file_path);
    orch.build_dynamic_context(Some(path)).await
}

#[cfg(test)]
mod tests {
    use rust_decimal_macros::dec;

    use super::*;

    #[test]
    fn test_extract_structured_output_with_schema() {
        let schema = serde_json::json!({"type": "object"});
        let text = r#"{"name": "test", "value": 42}"#;
        let result = extract_structured_output(Some(&schema), text);
        assert!(result.is_some());
        assert_eq!(result.unwrap()["name"], "test");
    }

    #[test]
    fn test_extract_structured_output_no_schema() {
        let text = r#"{"name": "test"}"#;
        let result = extract_structured_output(None, text);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_structured_output_invalid_json() {
        let schema = serde_json::json!({"type": "object"});
        let text = "not valid json";
        let result = extract_structured_output(Some(&schema), text);
        assert!(result.is_none());
    }

    #[test]
    fn test_budget_context_check_ok() {
        let tracker = BudgetTracker::new(dec!(10));
        let config = BudgetConfig::default();
        let ctx = BudgetContext {
            tracker: &tracker,
            tenant: None,
            config: &config,
        };
        assert!(ctx.check().is_ok());
    }

    #[test]
    fn test_extract_file_path() {
        let input = serde_json::json!({"file_path": "/src/lib.rs"});
        assert_eq!(
            extract_file_path("Read", &input),
            Some("/src/lib.rs".to_string())
        );

        let input = serde_json::json!({"path": "/src"});
        assert_eq!(extract_file_path("Glob", &input), Some("/src".to_string()));

        let input = serde_json::json!({"command": "ls"});
        assert_eq!(extract_file_path("Bash", &input), None);
    }

    #[test]
    fn test_extract_file_path_all_tools() {
        let file_input = serde_json::json!({"file_path": "/test/file.rs"});
        let path_input = serde_json::json!({"path": "/test/dir"});

        assert_eq!(
            extract_file_path("Read", &file_input),
            Some("/test/file.rs".to_string())
        );
        assert_eq!(
            extract_file_path("Write", &file_input),
            Some("/test/file.rs".to_string())
        );
        assert_eq!(
            extract_file_path("Edit", &file_input),
            Some("/test/file.rs".to_string())
        );

        assert_eq!(
            extract_file_path("Glob", &path_input),
            Some("/test/dir".to_string())
        );
        assert_eq!(
            extract_file_path("Grep", &path_input),
            Some("/test/dir".to_string())
        );

        assert_eq!(extract_file_path("WebFetch", &file_input), None);
        assert_eq!(extract_file_path("Task", &file_input), None);
    }

    #[test]
    fn test_extract_file_path_missing_field() {
        let empty = serde_json::json!({});
        assert_eq!(extract_file_path("Read", &empty), None);
        assert_eq!(extract_file_path("Glob", &empty), None);

        let wrong_field = serde_json::json!({"other": "value"});
        assert_eq!(extract_file_path("Read", &wrong_field), None);
        assert_eq!(extract_file_path("Glob", &wrong_field), None);
    }

    #[test]
    fn test_extract_file_path_non_string() {
        let input = serde_json::json!({"file_path": 123});
        assert_eq!(extract_file_path("Read", &input), None);

        let input = serde_json::json!({"path": null});
        assert_eq!(extract_file_path("Glob", &input), None);
    }
}
