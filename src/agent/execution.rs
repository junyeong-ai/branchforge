//! Agent execution logic with session-based context management.

use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, info, instrument, warn};

use super::AgentMetrics;
use super::common::{
    self, BudgetContext, accumulate_inner_usage, accumulate_response_usage, emit_cost_report,
    emit_tokens_consumed, emit_tool_executed, handle_compaction, is_context_overflow_error,
    maybe_emit_budget_alert, maybe_invoke_explicit_skill_command, run_post_tool_hooks,
    run_stop_hooks, try_activate_dynamic_rules, try_recover,
};
use super::events::AgentResult;
use super::executor::Agent;
use super::request::RequestBuilder;
use super::run_config::RunConfig;
use crate::authorization::approval::DEFAULT_APPROVAL_TIMEOUT_SECS;
use crate::authorization::{ApprovalRequest, ApprovalResponse, AuthorizationDenied};
use crate::graph::ReplayInput;
use crate::hooks::{HookContext, HookEvent, HookInput};
use crate::ir::FinishReason;
use crate::ir::Message;
use crate::session::{MessageMetadata, ToolExecution};
use crate::types::context_window;

impl Agent {
    fn check_budget(&self) -> crate::Result<()> {
        BudgetContext {
            tracker: &self.runtime.budget_tracker,
            tenant: self.runtime.tenant_budget.as_deref(),
            config: &self.runtime.config.budget,
        }
        .check()
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
        let default_timeout = self
            .runtime
            .config
            .execution
            .timeout
            .unwrap_or(std::time::Duration::from_secs(600));
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

    #[instrument(skip(self, prompt, run_config), fields(session_id = %self.session_id))]
    async fn execute_inner(
        &self,
        prompt: &str,
        run_config: Option<&RunConfig>,
    ) -> crate::Result<AgentResult> {
        let _guard = self.state.acquire_execution().await?;

        // Propagate EventBus to Session and its inner Graph so that
        // SessionChanged, BranchForked, and CheckpointCreated events fire.
        if let Some(ref bus) = self.runtime.event_bus {
            self.state.with_event_bus(Arc::clone(bus)).await;
        }

        let execution_start = Instant::now();
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
        let mut final_text = String::new();
        let mut final_stop_reason = FinishReason::Stop;
        let mut dynamic_rules_context = String::new();
        let mut total_usage = crate::ir::Usage::default();

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
        // Apply RunConfig overrides.
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
        let mut recovery_attempts = 0u32;

        info!(prompt_len = final_prompt.len(), "Starting agent execution");

        loop {
            if self.runtime.shutdown.is_cancelled() {
                self.persist_session_state().await?;
                break;
            }

            metrics.iterations += 1;
            if metrics.iterations > effective_max_iterations {
                warn!(max = effective_max_iterations, "Max iterations reached");
                break;
            }

            self.check_budget()?;

            let budget_ctx = BudgetContext {
                tracker: &self.runtime.budget_tracker,
                tenant: self.runtime.tenant_budget.as_deref(),
                config: &self.runtime.config.budget,
            };
            if let Some(fallback) = budget_ctx.fallback_model() {
                request_builder.set_model(fallback);
            }

            debug!(iteration = metrics.iterations, "Starting iteration");

            let messages = self
                .state
                .with_session(|session| session.to_api_messages())
                .await;

            // Fire ModelSelection hook - allows overriding the model
            let model_input = HookInput::model_selection(
                &*self.session_id,
                request_builder.current_model(),
                messages.len(),
                request_builder.has_tools(),
            );
            match self
                .runtime
                .hooks
                .execute(HookEvent::ModelSelection, model_input, &hook_ctx)
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
                    return Err(e);
                }
            }

            // Fire PreMessage hook - can block the message
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
                .execute(HookEvent::PreMessage, pre_msg_input, &hook_ctx)
                .await?;

            if !pre_msg_output.continue_execution {
                return Err(crate::Error::Authorization(
                    pre_msg_output
                        .stop_reason
                        .unwrap_or_else(|| "Blocked by PreMessage hook".into()),
                ));
            }

            let api_start = Instant::now();
            let ir_request = request_builder.build(messages, &dynamic_rules_context);
            let response = match self.runtime.llm.send(&ir_request).await {
                Ok(resp) => resp,
                Err(e) if is_context_overflow_error(&e) => {
                    if let Some(action) = try_recover(
                        &e,
                        &self.state,
                        &self.runtime,
                        max_tokens,
                        &mut recovery_attempts,
                    )
                    .await
                    {
                        use crate::session::compact::recovery::RecoveryAction;
                        match action {
                            RecoveryAction::Retry | RecoveryAction::CompactAndRetry => continue,
                            RecoveryAction::Abort => return Err(e),
                        }
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            };
            recovery_attempts = 0;
            let api_duration_ms = api_start.elapsed().as_millis() as u64;
            metrics.record_api_call_with_timing(api_duration_ms);
            debug!(api_time_ms = api_duration_ms, "API call completed");

            // Fire PostMessage hook (observation only, fail-open)
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
                .execute(HookEvent::PostMessage, post_msg_input, &hook_ctx)
                .await;

            accumulate_response_usage(
                &mut total_usage,
                &mut metrics,
                &self.runtime.budget_tracker,
                self.runtime.tenant_budget.as_deref(),
                &self.runtime.config.model.primary,
                &response.usage,
            )?;

            emit_tokens_consumed(
                self.runtime.event_bus.as_deref(),
                &response.usage,
                &self.runtime.config.model.primary,
            );

            maybe_emit_budget_alert(
                &self.runtime.budget_tracker,
                self.runtime.event_bus.as_deref(),
                self.runtime.config.budget.alert_threshold_pct,
            );

            final_text = response.text();
            final_stop_reason = response.finish_reason.clone();
            let assistant_metadata = MessageMetadata {
                model: Some(response.model.clone()),
                request_id: Some(response.id.clone()),
                structured_output: self.extract_structured_output(&final_text),
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
            self.persist_session_state().await?;

            if !response.finish_reason.should_continue() {
                debug!("Model finished, ending loop");
                break;
            }

            // Extract tool calls from the response content
            let tool_calls: Vec<_> = response
                .content
                .iter()
                .filter_map(|part| match part {
                    crate::ir::ContentPart::ToolCall {
                        id,
                        name,
                        arguments,
                        ..
                    } => Some((id.clone(), name.clone(), arguments.clone())),
                    _ => None,
                })
                .collect();
            let hook_ctx = self.hook_context();

            let mut prepared = Vec::with_capacity(tool_calls.len());
            let mut blocked = Vec::with_capacity(tool_calls.len());

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
                        AuthorizationDenied::new(tool_name, tool_id, tool_input.clone())
                            .reason(reason),
                    );
                } else {
                    let input = pre_output.updated_input.unwrap_or(tool_input.clone());

                    // Human-in-the-loop: check if supervised mode requires approval
                    if self.runtime.execution_mode.requires_review(tool_name) {
                        let approval_result = if let Some(ref sender) = self.runtime.approval_sender
                        {
                            let request = ApprovalRequest {
                                tool_name: tool_name.clone(),
                                tool_call_id: tool_id.clone(),
                                tool_input: input.clone(),
                                reason: format!(
                                    "Tool '{}' requires approval in {} mode",
                                    tool_name, self.runtime.execution_mode
                                ),
                            };
                            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                            if sender.send((request, resp_tx)).await.is_err() {
                                Some(ApprovalResponse::Deny {
                                    reason: "Approval channel closed".into(),
                                })
                            } else {
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
                                    resp_rx,
                                )
                                .await
                                {
                                    Ok(Ok(response)) => Some(response),
                                    Ok(Err(_)) => Some(ApprovalResponse::Deny {
                                        reason: "Approval responder dropped".into(),
                                    }),
                                    Err(_) => Some(ApprovalResponse::Deny {
                                        reason: "Approval timed out".into(),
                                    }),
                                }
                            }
                        } else {
                            // No approval channel configured - deny with guidance
                            Some(ApprovalResponse::Deny {
                                reason: format!(
                                    "Tool '{}' requires review but no approval channel is configured.                                      Use AgentBuilder::approval_channel() to enable human-in-the-loop.",
                                    tool_name
                                ),
                            })
                        };

                        match approval_result {
                            Some(ApprovalResponse::Approve) => {
                                debug!(tool = %tool_name, "Tool approved by human");
                            }
                            Some(ApprovalResponse::Deny { reason }) => {
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
                            None => unreachable!("approval_result is always Some"),
                        }
                    }

                    self.state
                        .append_graph_node(
                            crate::graph::NodeKind::ToolCall,
                            serde_json::json!({
                                "tool_call_id": tool_id.clone(),
                                "tool_name": tool_name.clone(),
                                "tool_input": input.clone(),
                            }),
                        )
                        .await?;
                    prepared.push((tool_id.clone(), tool_name.clone(), input));
                }
            }

            let context_scope = self.runtime.context_scope.clone();
            // Create a child cancellation token that fires when the runtime
            // shuts down. Each tool future races against it via
            // `execute_with_cancel`, so graceful shutdown aborts in-flight
            // tools instead of waiting for their natural completion.
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

            let mut results = blocked;
            for (id, name, input, result, duration_ms) in parallel_results {
                let is_error = result.is_error();
                debug!(tool = %name, duration_ms, is_error, "Tool execution completed");
                metrics.record_tool(&id, &name, duration_ms, is_error);

                accumulate_inner_usage(
                    &self.state,
                    &mut total_usage,
                    &mut metrics,
                    &self.runtime.budget_tracker,
                    &result,
                    &name,
                )
                .await?;

                try_activate_dynamic_rules(
                    &name,
                    &input,
                    &self.runtime.orchestrator,
                    &mut dynamic_rules_context,
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

                results.push(
                    crate::ir::ContentPart::from_tool_result(&id, &result).with_tool_name(&name),
                );
            }

            self.state
                .with_session_mut(|session| session.add_tool_results(results))
                .await?;
            self.persist_session_state().await?;

            if all_non_retryable {
                warn!("All tool calls failed with non-retryable errors, ending execution");
                break;
            }

            handle_compaction(
                &self.state,
                &self.runtime,
                &hook_ctx,
                &self.session_id,
                max_tokens,
                &mut metrics,
            )
            .await;
            self.persist_session_state().await?;
        }

        metrics.execution_time_ms = execution_start.elapsed().as_millis() as u64;

        emit_cost_report(
            self.runtime.event_bus.as_deref(),
            &metrics,
            &self.session_id,
        );

        run_stop_hooks(&self.runtime.hooks, &hook_ctx, &self.session_id).await;

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
            .cwd(self.runtime.config.working_dir.clone().unwrap_or_default())
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
