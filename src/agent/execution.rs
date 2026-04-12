//! Agent execution logic with session-based context management.

#![allow(missing_docs)]

use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, info, instrument, warn};

use super::AgentMetrics;
use super::common::{
    self, accumulate_inner_usage, emit_cost_report, emit_tool_executed, handle_compaction,
    maybe_invoke_explicit_skill_command, request_tool_approval, run_post_tool_hooks,
    run_stop_hooks, try_activate_dynamic_rules,
};
use super::events::AgentResult;
use super::executor::Agent;
use super::request::RequestBuilder;
use super::request_pipeline::RequestPipeline;
use super::run_config::RunConfig;
use crate::authorization::{AuthorizationDenied, ToolApprovalResponse};
use crate::graph::ReplayInput;
use crate::hooks::{HookContext, HookEvent, HookInput};
use crate::ir::FinishReason;
use crate::ir::Message;
use crate::session::{MessageMetadata, ToolExecution};
use crate::types::context_window;

/// Outcome of a single model request within the agentic loop.
/// Replaces implicit `continue`/`break`/`return Err` with explicit
/// variants that the orchestrator matches on.
enum ModelRequestOutcome {
    /// Iteration gate stopped the loop (max iterations, shutdown, etc.).
    GateStopped,
    /// Retry this iteration (structured output or recovery applied).
    Retry,
    /// Unrecoverable error — propagate to caller.
    Fatal(crate::Error),
    /// Model finished — stop reason says no continuation needed.
    Finished,
    /// Model responded with potential tool calls — proceed to execution.
    ToolCallsReady {
        response: Box<crate::ir::ModelResponse>,
    },
}

impl Agent {
    fn check_budget(&self) -> crate::Result<()> {
        self.runtime.budget_context().check()
    }

    pub async fn execute(&self, prompt: &str) -> crate::Result<AgentResult> {
        self.execute_with_optional_config(prompt, None).await
    }

    /// Execute with per-run configuration overrides.
    pub async fn execute_with(
        &self,
        prompt: impl Into<String>,
        run_config: RunConfig,
    ) -> crate::Result<AgentResult> {
        self.execute_with_optional_config(&prompt.into(), Some(run_config))
            .await
    }

    async fn execute_with_optional_config(
        &self,
        prompt: &str,
        run_config: Option<RunConfig>,
    ) -> crate::Result<AgentResult> {
        let default_timeout = self.runtime.config.execution.timeout;
        let timeout = run_config
            .as_ref()
            .and_then(|rc| rc.timeout_override())
            .unwrap_or(default_timeout);

        if self.state.is_executing() {
            self.state.enqueue(prompt).await.map_err(|e| {
                crate::Error::Session(crate::session::SessionError::QueueFull {
                    message: e.to_string(),
                })
            })?;
            return self.wait_for_execution(timeout).await;
        }

        tokio::time::timeout(timeout, self.execute_inner(prompt, run_config.as_ref()))
            .await
            .map_err(|_| crate::Error::Timeout(timeout))?
    }

    async fn wait_for_execution(&self, timeout: std::time::Duration) -> crate::Result<AgentResult> {
        tokio::time::timeout(timeout, async {
            loop {
                self.state.wait_for_queue_signal().await;
                if !self.state.is_executing()
                    && let Some(merged) = self.state.dequeue_or_merge().await
                {
                    return self.execute_inner(&merged.content, None).await;
                }
            }
        })
        .await
        .map_err(|_| crate::Error::Timeout(timeout))?
    }

    pub async fn execute_with_messages(
        &self,
        previous_messages: Vec<Message>,
        prompt: &str,
    ) -> crate::Result<AgentResult> {
        let context_summary = previous_messages
            .iter()
            .filter_map(|m| m.content.iter().filter_map(|b| b.as_text()).next())
            .collect::<Vec<_>>()
            .join("\n---\n");

        let enriched_prompt = if context_summary.is_empty() {
            prompt.to_string()
        } else {
            format!(
                "Previous conversation context:\n{}\n\nContinue with: {}",
                context_summary, prompt
            )
        };

        self.execute(&enriched_prompt).await
    }

    pub async fn execute_with_replay(
        &self,
        replay: ReplayInput,
        prompt: &str,
    ) -> crate::Result<AgentResult> {
        self.execute_with_messages(replay.messages, prompt).await
    }

    // ---------------------------------------------------------------
    // execute_inner: orchestrator (delegates to staged methods below)
    // ---------------------------------------------------------------

    #[instrument(skip(self, prompt, run_config), fields(session_id = %self.session_id))]
    async fn execute_inner(
        &self,
        prompt: &str,
        run_config: Option<&RunConfig>,
    ) -> crate::Result<AgentResult> {
        let _guard = self.state.acquire_execution().await?;

        if let Some(ref bus) = self.runtime.event_bus {
            self.state.with_event_bus(Arc::clone(bus)).await;
        }

        let execution_start = Instant::now();

        let (mut request_builder, effective_max_iterations, max_tokens, hook_ctx, mut metrics) =
            self.prepare_execution(prompt, run_config).await?;

        let mut final_text = String::new();
        let mut final_stop_reason = FinishReason::Stop;
        let mut total_usage = crate::ir::Usage::default();
        let mut recovery_attempts = 0u32;
        let mut structured_output_attempts = 0u32;
        let mut cache_break_baseline: Option<crate::observability::CacheBreakBaseline> = None;
        let mut dynamic_rules_context = String::new();

        info!(prompt_len = prompt.len(), "Starting agent execution");

        loop {
            match self
                .send_model_request(
                    &mut request_builder,
                    &mut metrics,
                    &mut total_usage,
                    &mut recovery_attempts,
                    &mut structured_output_attempts,
                    &mut cache_break_baseline,
                    &dynamic_rules_context,
                    &mut final_text,
                    &mut final_stop_reason,
                    effective_max_iterations,
                    &hook_ctx,
                )
                .await?
            {
                ModelRequestOutcome::GateStopped => {
                    self.persist_session_state().await?;
                    break;
                }
                ModelRequestOutcome::Retry => continue,
                ModelRequestOutcome::Fatal(e) => return Err(e),
                ModelRequestOutcome::Finished => break,
                ModelRequestOutcome::ToolCallsReady { response } => {
                    let all_failed = self
                        .execute_tool_calls(
                            &response,
                            &mut metrics,
                            &mut total_usage,
                            &mut dynamic_rules_context,
                            max_tokens,
                        )
                        .await?;
                    if all_failed {
                        warn!("All tool calls failed with non-retryable errors, ending execution");
                        break;
                    }
                }
            }
        }

        self.finalize_execution(
            metrics,
            total_usage,
            final_text,
            final_stop_reason,
            execution_start,
        )
        .await
    }

    // ---------------------------------------------------------------
    // Stage 1: prepare_execution — session setup, hooks, request builder
    // ---------------------------------------------------------------

    async fn prepare_execution(
        &self,
        prompt: &str,
        run_config: Option<&RunConfig>,
    ) -> crate::Result<(RequestBuilder, usize, u64, HookContext, AgentMetrics)> {
        let hook_ctx = self.hook_context();

        let session_start_input = HookInput::session_start(&*self.session_id);
        if let Err(e) = self
            .runtime
            .hooks
            .execute(HookEvent::SessionStart, session_start_input, &hook_ctx)
            .await
        {
            warn!(error = %e, "SessionStart hook failed");
        }

        let final_prompt = if let Some(merged) = self.state.dequeue_or_merge().await {
            format!("{}\n{}", prompt, merged.content)
        } else {
            prompt.to_string()
        };

        let prompt_input = HookInput::user_prompt_submit(&*self.session_id, &final_prompt);
        let prompt_output = self
            .runtime
            .hooks
            .execute(HookEvent::UserPromptSubmit, prompt_input, &hook_ctx)
            .await?;

        if !prompt_output.continue_execution {
            let session_end_input = HookInput::session_end(&*self.session_id);
            if let Err(e) = self
                .runtime
                .hooks
                .execute(HookEvent::SessionEnd, session_end_input, &hook_ctx)
                .await
            {
                warn!(error = %e, "SessionEnd hook failed");
            }
            return Err(crate::Error::Authorization(
                prompt_output
                    .stop_reason
                    .unwrap_or_else(|| "Blocked by hook".into()),
            ));
        }

        self.state
            .with_session_mut(|session| session.add_user_message(&final_prompt))
            .await?;
        self.persist_session_state().await?;

        let mut metrics = AgentMetrics::default();

        if maybe_invoke_explicit_skill_command(
            &self.runtime.tools,
            &self.state,
            &self.runtime.hooks,
            &hook_ctx,
            &self.session_id,
            &final_prompt,
            &mut metrics,
        )
        .await?
        {
            self.persist_session_state().await?;
        }

        let mut request_builder = {
            let static_context = match &self.runtime.orchestrator {
                Some(orchestrator) => orchestrator.read().await.static_context().clone(),
                None => crate::context::StaticContext::new(),
            };
            let metadata = self
                .state
                .with_session(|session| {
                    crate::agent::types::RequestMetadata::from_identity(
                        session.tenant_id.as_deref(),
                        session.principal_id.as_deref(),
                        Some(&session.id.to_string()),
                    )
                })
                .await;
            let builder = RequestBuilder::new(
                &self.runtime.config,
                Arc::clone(&self.runtime.tools),
                static_context,
            )
            .metadata(metadata);

            if let Some(ref tsm) = self.runtime.tool_search_manager {
                let prepared = tsm
                    .prepare_tools_for_access(&self.runtime.config.security.tool_surface)
                    .await;
                if prepared.use_search {
                    info!(
                        immediate = prepared.immediate.len(),
                        deferred = prepared.deferred.len(),
                        tokens_saved = prepared.token_savings().get(),
                        "MCP Progressive Disclosure active"
                    );
                }
                builder.prepared_tools(prepared)
            } else {
                builder
            }
        };

        if let Some(rc) = run_config {
            if let Some(model) = rc.model_override() {
                request_builder.set_model(model);
            }
            if let Some(max_tokens) = rc.max_tokens_override() {
                request_builder.set_max_tokens(max_tokens);
            }
            if let Some(prompt) = rc.system_prompt_override() {
                request_builder.set_system_prompt_override(prompt);
            }
        }

        let effective_max_iterations = run_config
            .map(|rc| rc.effective_max_iterations(self.runtime.config.execution.max_iterations))
            .unwrap_or(self.runtime.config.execution.max_iterations);

        let max_tokens = context_window::for_model(&self.runtime.config.model.primary);

        Ok((
            request_builder,
            effective_max_iterations,
            max_tokens,
            hook_ctx,
            metrics,
        ))
    }

    // ---------------------------------------------------------------
    // Stage 2: send_model_request — gate, hooks, LLM call, response recording
    // ---------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    async fn send_model_request(
        &self,
        request_builder: &mut RequestBuilder,
        metrics: &mut AgentMetrics,
        total_usage: &mut crate::ir::Usage,
        recovery_attempts: &mut u32,
        structured_output_attempts: &mut u32,
        cache_break_baseline: &mut Option<crate::observability::CacheBreakBaseline>,
        dynamic_rules_context: &str,
        final_text: &mut String,
        final_stop_reason: &mut FinishReason,
        effective_max_iterations: usize,
        hook_ctx: &HookContext,
    ) -> crate::Result<ModelRequestOutcome> {
        metrics.iterations += 1;

        let gate_ctx = super::policy::IterationContext {
            iteration: metrics.iterations,
            max_iterations: effective_max_iterations,
            structured_output_attempts: *structured_output_attempts,
            max_structured_output_retries: self
                .runtime
                .config
                .execution
                .max_structured_output_retries,
            recovery_attempts: *recovery_attempts,
            total_usage,
            is_shutdown_requested: self.runtime.shutdown.is_cancelled(),
        };
        match self.runtime.iteration_gate.should_continue(&gate_ctx) {
            super::policy::GateDecision::Continue => {}
            super::policy::GateDecision::Stop { reason } => {
                info!(reason = %reason, "Iteration gate stopped the loop");
                return Ok(ModelRequestOutcome::GateStopped);
            }
        }

        self.check_budget()?;

        let pipeline = RequestPipeline::new(&self.runtime);
        pipeline.apply_budget_fallback(request_builder);

        debug!(iteration = metrics.iterations, "Starting iteration");

        let messages = self
            .state
            .with_session(|session| session.to_api_messages())
            .await;

        let model_input = HookInput::model_selection(
            &*self.session_id,
            request_builder.current_model(),
            messages.len(),
            request_builder.has_tools(),
        );
        match self
            .runtime
            .hooks
            .execute(HookEvent::ModelSelection, model_input, hook_ctx)
            .await
        {
            Ok(output) => {
                if let Some(ref updated) = output.updated_input
                    && let Some(model_override) = updated.get("model").and_then(|v| v.as_str())
                {
                    request_builder.set_model(model_override);
                }
            }
            Err(e) => {
                warn!(error = %e, "ModelSelection hook failed, blocking request");
                return Ok(ModelRequestOutcome::Fatal(e));
            }
        }

        let messages_json: Vec<serde_json::Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        let pre_msg_input = HookInput::pre_message(
            &*self.session_id,
            messages_json,
            request_builder.current_model(),
        );
        let pre_msg_output = self
            .runtime
            .hooks
            .execute(HookEvent::PreMessage, pre_msg_input, hook_ctx)
            .await?;

        if !pre_msg_output.continue_execution {
            return Ok(ModelRequestOutcome::Fatal(crate::Error::Authorization(
                pre_msg_output
                    .stop_reason
                    .unwrap_or_else(|| "Blocked by PreMessage hook".into()),
            )));
        }

        let api_start = Instant::now();
        let ir_request = request_builder.build(messages, dynamic_rules_context);
        let prepared = pipeline.prepare(&ir_request)?;

        let response = match self.runtime.llm.send(&ir_request).await {
            Ok(resp) => resp,
            Err(e) => {
                if let crate::Error::StructuredOutputInvalid { reason, .. } = &e {
                    *structured_output_attempts += 1;
                    if *structured_output_attempts
                        >= self.runtime.config.execution.max_structured_output_retries
                    {
                        return Ok(ModelRequestOutcome::Fatal(
                            crate::Error::StructuredOutputExhausted {
                                attempts: *structured_output_attempts,
                                last_reason: reason.clone(),
                            },
                        ));
                    }
                    *recovery_attempts = 0;
                    return Ok(ModelRequestOutcome::Retry);
                }
                let executor = super::recovery_executor::RecoveryExecutor {
                    registry: &self.runtime.recovery_recipes,
                    session_handle: &self.state,
                    llm: Some(self.runtime.llm.as_ref()),
                    event_bus: self.runtime.event_bus.as_deref(),
                };
                return match executor.apply(&e, recovery_attempts).await {
                    super::recovery_executor::RecoveryOutcome::Retry => {
                        Ok(ModelRequestOutcome::Retry)
                    }
                    super::recovery_executor::RecoveryOutcome::Abort => {
                        Ok(ModelRequestOutcome::Fatal(e))
                    }
                };
            }
        };
        *recovery_attempts = 0;
        let api_duration_ms = api_start.elapsed().as_millis() as u64;
        metrics.record_api_call_with_timing(api_duration_ms);
        debug!(api_time_ms = api_duration_ms, "API call completed");

        let stop_reason_str = Some(format!("{:?}", response.finish_reason));
        let post_msg_input = HookInput::post_message(
            &*self.session_id,
            &response.model,
            stop_reason_str,
            response.usage.input_tokens,
            response.usage.output_tokens,
        );
        let _ = self
            .runtime
            .hooks
            .execute(HookEvent::PostMessage, post_msg_input, hook_ctx)
            .await;

        pipeline.record_usage(
            total_usage,
            metrics,
            &self.runtime.config.model.primary,
            &response.usage,
            Some(prepared.estimate),
        )?;

        // Cache-break classification
        {
            let prev_cache_read = cache_break_baseline
                .as_ref()
                .map(|b| b.last_cache_read_tokens)
                .unwrap_or(0);
            let current = crate::observability::CacheBreakBaseline::from_request(
                &ir_request,
                prev_cache_read,
            );
            let had_markers = ir_request.has_cache_markers();
            if let Some(cause) = crate::observability::classify_cache_break(
                cache_break_baseline.as_ref(),
                &current,
                &response,
                had_markers,
            ) {
                tracing::warn!(
                    target: "branchforge::cache::break",
                    category = cause.category(),
                    model = %ir_request.model,
                    "prompt cache break classified"
                );
                if let Some(bus) = self.runtime.event_bus.as_ref() {
                    use crate::decision::DecisionReason;
                    bus.emit_typed(crate::events::CacheBreakObservedPayload {
                        category: cause.category().to_string(),
                        summary: cause.summary(),
                        model: ir_request.model.clone(),
                    });
                }
            }
            let new_read = response.usage.cached_input_tokens.unwrap_or(0);
            let mut next = current;
            next.last_cache_read_tokens = new_read;
            *cache_break_baseline = Some(next);
        }

        // Rate-limit snapshot
        if let (Some(bus), Some(snap)) = (
            self.runtime.event_bus.as_ref(),
            response.rate_limit.as_ref(),
        ) {
            bus.emit_typed(crate::events::RateLimitObservedPayload {
                snapshot: snap.clone(),
            });
            if snap.is_approaching_limit(crate::ir::APPROACHING_THRESHOLD) {
                bus.emit_typed(crate::events::RateLimitApproachingPayload {
                    snapshot: snap.clone(),
                });
            }
        }

        *final_text = response.text();
        *final_stop_reason = response.finish_reason.clone();
        let assistant_metadata = MessageMetadata {
            model: Some(response.model.clone()),
            request_id: Some(response.id.clone()),
            structured_output: self.extract_structured_output(final_text),
            ..Default::default()
        };

        self.state
            .with_session_mut(|session| {
                session.add_assistant_message_with_metadata(
                    response.content.clone(),
                    Some(response.usage.clone()),
                    assistant_metadata,
                )
            })
            .await?;
        self.persist_session_state_detached();

        if !response.finish_reason.should_continue() {
            debug!("Model finished, ending loop");
            return Ok(ModelRequestOutcome::Finished);
        }

        Ok(ModelRequestOutcome::ToolCallsReady {
            response: Box::new(response),
        })
    }

    // ---------------------------------------------------------------
    // Stage 3: execute_tool_calls — extract, approve, validate, run, record
    // ---------------------------------------------------------------

    async fn execute_tool_calls(
        &self,
        response: &crate::ir::ModelResponse,
        metrics: &mut AgentMetrics,
        total_usage: &mut crate::ir::Usage,
        dynamic_rules_context: &mut String,
        max_tokens: u64,
    ) -> crate::Result<bool> {
        let raw_calls: Vec<_> = response
            .content
            .iter()
            .filter_map(|part| match part {
                crate::ir::ContentPart::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } => Some(super::policy::ToolCallProposal {
                    id: id.clone(),
                    name: name.clone(),
                    input: arguments.clone(),
                }),
                _ => None,
            })
            .collect();

        let selection_ctx = super::policy::ToolSelectionContext {
            iteration: metrics.iterations,
            total_usage,
            model: &response.model,
        };
        let plan = self
            .runtime
            .tool_selection_strategy
            .plan(raw_calls, &selection_ctx);

        let hook_ctx = self.hook_context();

        let mut prepared = Vec::with_capacity(plan.execute.len());
        let mut blocked: Vec<crate::ir::ContentPart> = plan
            .skip
            .into_iter()
            .map(|(call, reason)| {
                crate::ir::ContentPart::tool_error(&call.id, reason).with_tool_name(&call.name)
            })
            .collect();

        let tool_calls: Vec<_> = plan
            .execute
            .into_iter()
            .map(|c| (c.id, c.name, c.input))
            .collect();

        // PreToolUse hooks + HITL approval + graph recording
        for (tool_id, tool_name, tool_input) in &tool_calls {
            let pre_input =
                HookInput::pre_tool_use(&*self.session_id, tool_name, tool_input.clone());
            let pre_output = self
                .runtime
                .hooks
                .execute(HookEvent::PreToolUse, pre_input, &hook_ctx)
                .await?;

            if !pre_output.continue_execution {
                debug!(tool = %tool_name, "Tool blocked by hook");
                let reason = pre_output
                    .stop_reason
                    .clone()
                    .unwrap_or_else(|| "Blocked by hook".into());
                blocked.push(
                    crate::ir::ContentPart::tool_error(tool_id, reason.clone())
                        .with_tool_name(tool_name),
                );
                metrics.record_authorization_denial(
                    AuthorizationDenied::new(tool_name, tool_id, tool_input.clone()).reason(reason),
                );
            } else {
                let original_input_for_audit = pre_output
                    .updated_input
                    .as_ref()
                    .map(|_| tool_input.clone());
                let input = pre_output.updated_input.unwrap_or(tool_input.clone());

                if self.runtime.execution_mode.requires_review(tool_name) {
                    let approval_result = request_tool_approval(
                        self.runtime.human.as_deref(),
                        tool_name,
                        tool_id,
                        &input,
                        &self.runtime.execution_mode.to_string(),
                    )
                    .await;

                    match approval_result {
                        ToolApprovalResponse::Approve => {
                            debug!(tool = %tool_name, "Tool approved by human");
                        }
                        ToolApprovalResponse::Deny { reason } => {
                            debug!(tool = %tool_name, %reason, "Tool denied by human");
                            blocked.push(
                                crate::ir::ContentPart::tool_error(tool_id, reason.clone())
                                    .with_tool_name(tool_name),
                            );
                            metrics.record_authorization_denial(
                                AuthorizationDenied::new(tool_name, tool_id, input.clone())
                                    .reason(reason),
                            );
                            continue;
                        }
                    }
                }

                let mut node_data = serde_json::json!({
                    "tool_call_id": tool_id.clone(),
                    "tool_name": tool_name.clone(),
                    "tool_input": input.clone(),
                });
                if let Some(original) = original_input_for_audit
                    && let Some(obj) = node_data.as_object_mut()
                {
                    obj.insert("original_input".to_string(), original);
                }
                self.state
                    .append_graph_node(crate::graph::NodeKind::ToolCall, node_data)
                    .await?;
                prepared.push((tool_id.clone(), tool_name.clone(), input));
            }
        }

        // Parallel preflight validation
        {
            let ctx = self.runtime.tools.context().clone();
            let validation_futures = prepared.iter().map(|(_, name, input)| {
                let tools = Arc::clone(&self.runtime.tools);
                let name = name.clone();
                let input = input.clone();
                let ctx = ctx.clone();
                async move {
                    let Some(tool) = tools.get(&name) else {
                        return Ok(());
                    };
                    tool.validate_input(&input, &ctx).await
                }
            });
            let results: Vec<_> = futures::future::join_all(validation_futures).await;

            let mut still_valid = Vec::with_capacity(prepared.len());
            for ((id, name, input), result) in
                std::mem::take(&mut prepared).into_iter().zip(results)
            {
                match result {
                    Ok(()) => still_valid.push((id, name, input)),
                    Err(err) => {
                        debug!(
                            tool = %name,
                            code = ?err.code,
                            message = %err.message,
                            "Tool input rejected by preflight validation"
                        );
                        blocked.push(
                            crate::ir::ContentPart::tool_error(&id, err.message.clone())
                                .with_tool_name(&name),
                        );
                        metrics.record_authorization_denial(
                            AuthorizationDenied::new(&name, &id, input).reason(err.message),
                        );
                    }
                }
            }
            prepared = still_valid;
        }

        // Parallel tool execution
        let context_scope = self.runtime.context_scope.clone();
        let shutdown = self.runtime.shutdown.clone();
        let tool_futures = prepared.into_iter().map(|(id, name, input)| {
            let tools = &self.runtime.tools;
            let context_scope = context_scope.clone();
            let cancel = shutdown.child_token();
            async move {
                let start = Instant::now();
                let result = if let Some(ref scope) = context_scope {
                    let fut = tools.execute_with_cancel(&name, input.clone(), cancel);
                    scope.wrap_tool_future(Box::pin(fut)).await
                } else {
                    tools
                        .execute_with_cancel(&name, input.clone(), cancel)
                        .await
                };
                let duration_ms = start.elapsed().as_millis() as u64;
                (id, name, input, result, duration_ms)
            }
        });

        let parallel_results: Vec<_> = futures::future::join_all(tool_futures).await;

        let all_non_retryable = !parallel_results.is_empty()
            && parallel_results
                .iter()
                .all(|(_, _, _, result, _)| result.is_non_retryable());

        // Process results
        let mut results = blocked;
        for (id, name, input, result, duration_ms) in parallel_results {
            let is_error = result.is_error();
            debug!(tool = %name, duration_ms, is_error, "Tool execution completed");
            metrics.record_tool(&id, &name, duration_ms, is_error);

            accumulate_inner_usage(
                &self.state,
                total_usage,
                metrics,
                &self.runtime.budget_tracker,
                &result,
                &name,
            )
            .await?;

            try_activate_dynamic_rules(
                &name,
                &input,
                &self.runtime.orchestrator,
                dynamic_rules_context,
            )
            .await;

            run_post_tool_hooks(
                &self.runtime.hooks,
                &hook_ctx,
                &self.session_id,
                &name,
                is_error,
                &result,
            )
            .await;

            emit_tool_executed(
                self.runtime.event_bus.as_deref(),
                &name,
                duration_ms,
                is_error,
            );

            self.state
                .record_tool_execution(
                    ToolExecution::new(self.state.session_id(), &name, input.clone())
                        .message(id.clone())
                        .output(result.output.text(), is_error)
                        .duration(duration_ms),
                )
                .await?;

            results
                .push(crate::ir::ContentPart::from_tool_result(&id, &result).with_tool_name(&name));
        }

        self.state
            .with_session_mut(|session| session.add_tool_results(results))
            .await?;
        self.persist_session_state_detached();

        if !all_non_retryable {
            handle_compaction(
                &self.state,
                &self.runtime,
                &hook_ctx,
                &self.session_id,
                max_tokens,
                metrics,
            )
            .await;
            self.persist_session_state().await?;
        }

        Ok(all_non_retryable)
    }

    // ---------------------------------------------------------------
    // Stage 4: finalize_execution — cost report, hooks, persist, result
    // ---------------------------------------------------------------

    async fn finalize_execution(
        &self,
        mut metrics: AgentMetrics,
        total_usage: crate::ir::Usage,
        final_text: String,
        final_stop_reason: FinishReason,
        execution_start: Instant,
    ) -> crate::Result<AgentResult> {
        metrics.execution_time_ms = execution_start.elapsed().as_millis() as u64;

        emit_cost_report(
            self.runtime.event_bus.as_deref(),
            &metrics,
            &self.session_id,
        );

        let hook_ctx = self.hook_context();
        run_stop_hooks(&self.runtime.hooks, &hook_ctx, &self.session_id).await;

        self.persist_session_state().await?;

        info!(
            iterations = metrics.iterations,
            tool_calls = metrics.tool_calls,
            api_calls = metrics.api_calls,
            total_tokens = metrics.total_tokens(),
            execution_time_ms = metrics.execution_time_ms,
            "Agent execution completed"
        );

        let messages = self
            .state
            .with_session(|session| session.to_api_messages())
            .await;

        let structured_output = self.extract_structured_output(&final_text);
        Ok(AgentResult::new(
            final_text,
            total_usage,
            metrics.iterations,
            final_stop_reason,
            metrics,
            self.session_id.to_string(),
            structured_output,
            messages,
        ))
    }

    pub(crate) fn hook_context(&self) -> HookContext {
        let ctx = HookContext::new(&*self.session_id)
            .cwd(self.runtime.config.workspace_root_buf().unwrap_or_default())
            .env(self.runtime.config.security.env.clone());
        match self.runtime.context_scope {
            Some(ref scope) => ctx.context_scope(Arc::clone(scope)),
            None => ctx,
        }
    }

    fn extract_structured_output(&self, text: &str) -> Option<serde_json::Value> {
        common::extract_structured_output(self.runtime.config.prompt.output_schema.as_ref(), text)
    }
}

#[cfg(test)]
mod tests {
    use super::common::extract_file_path;

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
}
