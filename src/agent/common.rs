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
use crate::hooks::{HookContext, HookEvent, HookInput, HookRegistry};
use crate::session::compact::CompactResult;
use crate::session::{ToolExecution, SessionHandle};
use crate::types::ToolResult;

use super::config::BudgetConfig;
use super::state::AgentMetrics;
use super::state_formatter::collect_compaction_state;

/// Default fallback model used when inner tool usage does not specify a model.
const DEFAULT_FALLBACK_MODEL: &str = "claude-haiku-4-5";

/// Phase D C-1: request tool approval through the unified
/// [`crate::authorization::HumanInteractionHandler`] channel.
///
/// Handles all four fail-closed paths uniformly so both the
/// non-streaming and streaming agent loops share one code path:
///
/// 1. No handler wired → deny with configuration hint.
/// 2. Handler returned `NotSupported` → deny with the same hint.
/// 3. Handler exceeded [`DEFAULT_APPROVAL_TIMEOUT_SECS`] → deny
///    with a timeout reason.
/// 4. Handler returned `Err(Handler(msg))` → deny with the msg.
///
/// All four cases produce a concrete
/// [`crate::authorization::ToolApprovalResponse::Deny`] so the caller
/// never needs to branch on handler presence or error kind.
pub(crate) async fn request_tool_approval(
    handler: Option<&dyn crate::authorization::HumanInteractionHandler>,
    tool_name: &str,
    tool_call_id: &str,
    tool_input: &Value,
    execution_mode_label: &str,
) -> crate::authorization::ToolApprovalResponse {
    use crate::authorization::{
        HumanInteractionError, ToolApprovalRequest, ToolApprovalResponse,
        approval::DEFAULT_APPROVAL_TIMEOUT_SECS,
    };
    use std::time::Duration;

    let Some(handler) = handler else {
        return ToolApprovalResponse::Deny {
            reason: format!(
                "Tool '{tool_name}' requires review but no HumanInteractionHandler is wired. \
                 Use AgentBuilder::human_handler() to enable human-in-the-loop."
            ),
        };
    };

    let request = ToolApprovalRequest {
        tool_name: tool_name.into(),
        tool_call_id: tool_call_id.into(),
        tool_input: tool_input.clone(),
        reason: format!("Tool '{tool_name}' requires approval in {execution_mode_label} mode"),
    };

    match tokio::time::timeout(
        Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
        handler.approve_tool(request),
    )
    .await
    {
        Ok(Ok(resp)) => resp,
        Ok(Err(HumanInteractionError::NotSupported(_))) => ToolApprovalResponse::Deny {
            reason: "HumanInteractionHandler does not support tool approval".into(),
        },
        Ok(Err(HumanInteractionError::Timeout)) => ToolApprovalResponse::Deny {
            reason: "Approval timed out (handler)".into(),
        },
        Ok(Err(HumanInteractionError::Handler(msg))) => ToolApprovalResponse::Deny {
            reason: format!("Approval handler error: {msg}"),
        },
        Err(_) => ToolApprovalResponse::Deny {
            reason: "Approval timed out".into(),
        },
    }
}

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

    /// Preflight budget check: estimate the token/cost footprint of
    /// `request` and reject it with [`crate::Error::BudgetExceeded`]
    /// if sending it would push the session or tenant over its
    /// configured limit.
    ///
    /// Behavior is gated on [`crate::budget::OnExceed`]:
    ///
    /// - `StopBeforeNext` → fail-fast with `BudgetExceeded` before
    ///   sending.
    /// - `WarnAndContinue` → emit a tracing warning but allow the
    ///   call through; the post-hoc [`check`] will still enforce
    ///   eventual termination.
    /// - `FallbackModel` → do nothing here. The caller has already
    ///   switched models via [`fallback_model`] earlier in the
    ///   iteration, so the preflight estimate already reflects the
    ///   cheaper model.
    ///
    /// This is the root-cause fix for budget bursting: prior versions
    /// only checked historical spend, so a single large request could
    /// push spend arbitrarily past the limit in one step. Preflight
    /// catches it before the TCP bytes leave the host.
    pub fn preflight(&self, request: &crate::ir::ModelRequest) -> Result<(), crate::Error> {
        use crate::budget::{BudgetExceedPolicy, estimate_request_tokens};

        let estimate = estimate_request_tokens(request);
        let estimated_cost = self.tracker.estimate_cost(&request.model, estimate);

        // Session tracker.
        if let Some(outcome) = self.tracker.project(estimated_cost) {
            match outcome {
                Ok(_projected) => {}
                Err((used, limit)) => match self.tracker.on_exceed_action() {
                    BudgetExceedPolicy::Stop => {
                        warn!(
                            used = %used,
                            limit = %limit,
                            estimate = %estimated_cost,
                            model = %request.model,
                            "Preflight blocked request — would exceed session budget"
                        );
                        return Err(crate::Error::BudgetExceeded { used, limit });
                    }
                    BudgetExceedPolicy::Warn => {
                        warn!(
                            used = %used,
                            limit = %limit,
                            estimate = %estimated_cost,
                            "Preflight projected over session budget — continuing (WarnAndContinue)"
                        );
                    }
                    BudgetExceedPolicy::Fallback(_) => {}
                },
            }
        }

        // Tenant budget.
        if let Some(tenant) = self.tenant {
            match tenant.project(estimated_cost) {
                Ok(_) => {}
                Err((used, limit)) => match tenant.on_exceed_action() {
                    BudgetExceedPolicy::Stop => {
                        warn!(
                            tenant_id = %tenant.tenant_id,
                            used = %used,
                            limit = %limit,
                            estimate = %estimated_cost,
                            "Preflight blocked request — would exceed tenant budget"
                        );
                        return Err(crate::Error::BudgetExceeded { used, limit });
                    }
                    BudgetExceedPolicy::Warn => {
                        warn!(
                            tenant_id = %tenant.tenant_id,
                            used = %used,
                            limit = %limit,
                            estimate = %estimated_cost,
                            "Preflight projected over tenant budget — continuing (WarnAndContinue)"
                        );
                    }
                    BudgetExceedPolicy::Fallback(_) => {}
                },
            }
        }

        Ok(())
    }
}

/// Accumulate usage from an API response into total_usage, metrics, and budget.
pub(crate) fn accumulate_response_usage(
    total_usage: &mut crate::ir::Usage,
    metrics: &mut AgentMetrics,
    budget_tracker: &BudgetTracker,
    tenant_budget: Option<&TenantBudget>,
    model: &str,
    ir_usage: &crate::ir::Usage,
) -> crate::Result<Decimal> {
    total_usage.add(ir_usage);

    metrics.add_usage_with_cache(ir_usage);
    metrics.record_model_usage(model, ir_usage);

    let cost = budget_tracker.record(model, ir_usage)?;
    metrics.add_cost(cost);

    if let Some(tenant_budget) = tenant_budget {
        tenant_budget.record(model, ir_usage)?;
    }

    Ok(cost)
}

/// Emit a [`TokensConsumed`](crate::events::EventKind::TokensConsumed) event
/// for real-time token tracking. Dispatches via the typed payload
/// path so subscribers can register with
/// [`crate::events::EventBus::subscribe_typed`].
pub(crate) fn emit_tokens_consumed(
    event_bus: Option<&crate::events::EventBus>,
    usage: &crate::ir::Usage,
    model: &str,
) {
    if let Some(bus) = event_bus {
        bus.emit_typed(crate::events::TokensConsumedPayload {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            model: model.to_string(),
        });
    }
}

/// Emit a [`ToolExecuted`](crate::events::EventKind::ToolExecuted) event
/// after a tool completes.
pub(crate) fn emit_tool_executed(
    event_bus: Option<&crate::events::EventBus>,
    tool_name: &str,
    duration_ms: u64,
    is_error: bool,
) {
    if let Some(bus) = event_bus {
        bus.emit_typed(crate::events::ToolExecutedPayload {
            tool_name: tool_name.to_string(),
            duration_ms,
            is_error,
        });
    }
}

/// Emit a [`ToolProgress`](crate::events::EventKind::ToolProgress) event
/// for sub-step visibility during tool execution.
pub(crate) fn emit_tool_progress(
    event_bus: Option<&crate::events::EventBus>,
    tool_id: &str,
    tool_name: &str,
    step: &str,
    status: &crate::tools::ProgressStatus,
) {
    if let Some(bus) = event_bus {
        bus.emit_typed(crate::events::ToolProgressPayload {
            tool_id: tool_id.to_string(),
            tool_name: tool_name.to_string(),
            step: step.to_string(),
            status: *status,
        });
    }
}

/// Emit a [`BudgetAlert`](crate::events::EventKind::BudgetAlert) if the budget
/// usage exceeds the configured warning threshold.
pub(crate) fn maybe_emit_budget_alert(
    budget_tracker: &BudgetTracker,
    event_bus: Option<&crate::events::EventBus>,
    alert_threshold_pct: u32,
) {
    let Some(bus) = event_bus else { return };
    let status = budget_tracker.check();
    let payload = match status {
        crate::budget::BudgetStatus::WithinBudget {
            used,
            limit,
            remaining,
        } => {
            let threshold = limit * Decimal::from(alert_threshold_pct) / Decimal::from(100);
            if used < threshold {
                return;
            }
            let utilization = if limit > Decimal::ZERO {
                use rust_decimal::prelude::ToPrimitive;
                (used / limit).to_f64().unwrap_or(0.0).clamp(0.0, 1.0)
            } else {
                0.0
            };
            crate::events::BudgetAlertPayload {
                used_usd: used,
                limit_usd: limit,
                remaining_usd: remaining,
                utilization,
            }
        }
        crate::budget::BudgetStatus::Exceeded { used, limit, .. } => {
            crate::events::BudgetAlertPayload {
                used_usd: used,
                limit_usd: limit,
                remaining_usd: Decimal::ZERO,
                utilization: 1.0,
            }
        }
        crate::budget::BudgetStatus::Unlimited { .. } => return,
    };
    bus.emit_typed(payload);
}

/// Accumulate inner usage from a tool result (e.g., subagent calls).
pub(crate) async fn accumulate_inner_usage(
    session_handle: &SessionHandle,
    total_usage: &mut crate::ir::Usage,
    metrics: &mut AgentMetrics,
    budget_tracker: &BudgetTracker,
    result: &ToolResult,
    tool_name: &str,
) -> crate::Result<()> {
    if let Some(ref inner_ir_usage) = result.inner_usage {
        session_handle
            .with_session_mut(|session| {
                session.update_usage(inner_ir_usage);
            })
            .await;
        total_usage.add(inner_ir_usage);
        metrics.add_usage_with_cache(inner_ir_usage);
        let inner_model = result
            .inner_model
            .as_deref()
            .unwrap_or(DEFAULT_FALLBACK_MODEL);
        metrics.record_model_usage(inner_model, inner_ir_usage);

        let inner_cost = budget_tracker.record(inner_model, inner_ir_usage)?;
        metrics.add_cost(inner_cost);

        debug!(
            tool = %tool_name,
            model = %inner_model,
            input_tokens = inner_ir_usage.input_tokens,
            output_tokens = inner_ir_usage.output_tokens,
            cost_usd = %inner_cost,
            "Accumulated inner usage from tool"
        );
    }
    Ok(())
}

pub(crate) async fn maybe_invoke_explicit_skill_command(
    tools: &ToolRegistry,
    session_handle: &SessionHandle,
    hooks: &HookRegistry,
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
    #[cfg(feature = "local-fs")]
    {
        // Phase D A-1: ask the Skill tool for its subjects rather
        // than going through a parallel extractor registry.
        let subjects = tools
            .get("Skill")
            .map(|t| t.permission_subjects(&actual_input))
            .unwrap_or_default();
        let permission = tools.context().check_explicit_skill_permission(&subjects);
        if !permission.is_allowed() {
            return Err(crate::Error::Authorization(permission.reason().to_string()));
        }
    }

    let typed_input: crate::skills::SkillInput = serde_json::from_value(actual_input.clone())
        .map_err(|e| crate::Error::Config(format!("Invalid explicit skill input: {e}")))?;

    let start = Instant::now();
    let result = Box::pin(skill_tool.execute_by_name_input(typed_input)).await;
    let duration_ms = start.elapsed().as_millis() as u64;
    let is_error = result.is_error();
    let tool_call_id = format!("skill_{}", uuid::Uuid::new_v4().simple());

    run_post_tool_hooks(hooks, hook_ctx, session_id, "Skill", is_error, &result).await;
    metrics.record_tool(&tool_call_id, "Skill", duration_ms, is_error);

    session_handle
        .record_tool_execution(
            ToolExecution::new(session_handle.session_id(), "Skill", actual_input.clone())
                .message(tool_call_id.clone())
                .output(result.output.text(), is_error)
                .duration(duration_ms),
        )
        .await?;

    session_handle
        .with_session_mut(|session| -> crate::session::SessionResult<()> {
            session.add_assistant_message(
                vec![crate::ir::ContentPart::ToolCall {
                    id: tool_call_id.clone(),
                    name: "Skill".to_string(),
                    arguments: actual_input.clone(),
                    origin: crate::ir::ToolOrigin::Local,
                }],
                None,
            )?;
            session.add_tool_results(vec![
                crate::ir::ContentPart::from_tool_result(&tool_call_id, &result)
                    .with_tool_name("Skill"),
            ])?;
            Ok(())
        })
        .await?;

    Ok(true)
}

/// Run post-tool hooks (PostToolUse on success, PostToolUseFailure on error).
pub(crate) async fn run_post_tool_hooks(
    hooks: &HookRegistry,
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

/// Emit a cost report event at the end of execution.
pub(crate) fn emit_cost_report(
    event_bus: Option<&crate::events::EventBus>,
    metrics: &AgentMetrics,
    session_id: &str,
) {
    if let Some(bus) = event_bus {
        let summary = metrics.cost_summary();
        bus.emit_simple(
            crate::events::EventKind::Custom("cost_report"),
            serde_json::json!({
                "session_id": session_id,
                "total_cost_usd": summary.total_cost_usd.to_string(),
                "per_model": summary.per_model.iter().map(|e| {
                    serde_json::json!({
                        "model": e.model,
                        "cost_usd": e.cost_usd.to_string(),
                        "input_tokens": e.input_tokens,
                        "output_tokens": e.output_tokens,
                    })
                }).collect::<Vec<_>>(),
            }),
        );
    }
}

/// Run Stop and SessionEnd hooks in sequence.
pub(crate) async fn run_stop_hooks(hooks: &HookRegistry, hook_ctx: &HookContext, session_id: &str) {
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
    session_handle: &SessionHandle,
    runtime: &super::runtime::AgentRuntime,
    hook_ctx: &HookContext,
    session_id: &str,
    max_tokens: u64,
    metrics: &mut AgentMetrics,
) {
    let config = &runtime.config.execution;
    let should_compact = session_handle
        .with_session(|session| {
            config.auto_compact && session.should_compact(max_tokens, config.compact_threshold)
        })
        .await;

    if !should_compact {
        return;
    }

    let pre_compact_input = HookInput::pre_compact(session_id);
    if let Err(e) = runtime
        .hooks
        .execute(HookEvent::PreCompact, pre_compact_input, hook_ctx)
        .await
    {
        warn!(error = %e, "PreCompact hook failed");
    }

    debug!("Compacting session context");
    let compact_result = session_handle.compact(runtime.llm.as_ref()).await;

    match compact_result {
        Ok(CompactResult::Compacted {
            saved_tokens,
            ref summary,
            ..
        }) => {
            info!(
                saved_tokens = saved_tokens.get(),
                "Session context compacted"
            );
            metrics.record_compaction();
            if let Some(bus) = runtime.event_bus.as_deref() {
                bus.emit_typed(crate::events::SessionCompactedPayload {
                    session_id: session_id.to_string(),
                    saved_tokens: saved_tokens.get(),
                    summary: summary.clone(),
                });
            }

            let state_sections = collect_compaction_state(&runtime.tools).await;
            if !state_sections.is_empty() {
                let _ = session_handle
                    .with_session_mut(|session| {
                        session.add_user_message(format!(
                            "<system-reminder>\n# State preserved after compaction\n\n{}\n</system-reminder>",
                            state_sections.join("\n\n")
                        ))
                    })
                    .await;
            }

            runtime.invalidate_caches_after_compact().await;
        }
        Ok(CompactResult::Truncated {
            truncation_count,
            estimated_token_savings,
        }) => {
            debug!(
                truncation_count,
                estimated_token_savings = estimated_token_savings.get(),
                "Micro-compaction truncated content blocks"
            );
            runtime.invalidate_caches_after_compact().await;
        }
        Ok(CompactResult::NotNeeded | CompactResult::Skipped { .. }) => {
            debug!("Compaction skipped or not needed");
        }
        Err(e) => {
            warn!(error = %e, "Session compaction failed");
        }
    }
}

// Recovery handling moved to the typed `RecoveryExecutor` in
// `agent::recovery_executor`, which interprets the action returned
// by `RecipeRegistry::decide`. The legacy `RecoveryStrategy` trait
// + `try_recover` shim were removed in favour of that single
// pipeline.

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
    fn preflight_blocks_request_that_would_burst_budget() {
        use crate::budget::BudgetExceedPolicy;
        use crate::ir::{Message, ModelRequest, ModelSettings};

        // $0.01 limit is far below the cost of a 1M-input / 4k-output
        // claude-sonnet-4-5 request (~$3/M input = $3.00 minimum).
        let tracker = BudgetTracker::new(dec!(0.01)).on_exceed(BudgetExceedPolicy::Stop);
        let config = BudgetConfig::default();
        let ctx = BudgetContext {
            tracker: &tracker,
            tenant: None,
            config: &config,
        };

        // Build a large request: ~4 MB of text ≈ 1M tokens.
        let bulk = "x".repeat(4_000_000);
        let mut req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user(bulk.as_str())]);
        req.settings = ModelSettings::default().with_max_output_tokens(4096);

        let err = ctx.preflight(&req).expect_err("preflight must block");
        assert!(matches!(err, crate::Error::BudgetExceeded { .. }));
    }

    #[test]
    fn preflight_passes_small_request_within_budget() {
        use crate::budget::BudgetExceedPolicy;
        use crate::ir::{Message, ModelRequest, ModelSettings};

        let tracker = BudgetTracker::new(dec!(5)).on_exceed(BudgetExceedPolicy::Stop);
        let config = BudgetConfig::default();
        let ctx = BudgetContext {
            tracker: &tracker,
            tenant: None,
            config: &config,
        };

        let mut req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("hello")]);
        req.settings = ModelSettings::default().with_max_output_tokens(64);
        ctx.preflight(&req)
            .expect("small request must fit under $5 budget");
    }

    #[test]
    fn preflight_warn_and_continue_never_blocks() {
        use crate::budget::BudgetExceedPolicy;
        use crate::ir::{Message, ModelRequest};

        let tracker = BudgetTracker::new(dec!(0.000001)).on_exceed(BudgetExceedPolicy::Warn);
        let config = BudgetConfig::default();
        let ctx = BudgetContext {
            tracker: &tracker,
            tenant: None,
            config: &config,
        };

        let req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("hi")]);
        ctx.preflight(&req)
            .expect("WarnAndContinue must never fail preflight");
    }

    #[test]
    fn preflight_unlimited_tracker_never_blocks() {
        use crate::ir::{Message, ModelRequest};

        let tracker = BudgetTracker::unlimited();
        let config = BudgetConfig::default();
        let ctx = BudgetContext {
            tracker: &tracker,
            tenant: None,
            config: &config,
        };

        let bulk = "y".repeat(10_000_000);
        let req = ModelRequest::new("claude-opus-4-6", vec![Message::user(bulk.as_str())]);
        ctx.preflight(&req)
            .expect("unlimited budget must always preflight-pass");
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
