//! Agent execution logic with session-based context management.

use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, info, instrument, warn};

use super::AgentMetrics;
use super::common::{
    self, BudgetContext, accumulate_inner_usage, accumulate_response_usage, handle_compaction,
    run_post_tool_hooks, run_stop_hooks, try_activate_dynamic_rules,
};
use super::events::AgentResult;
use super::executor::Agent;
use super::request::RequestBuilder;
use crate::graph::ReplayInput;
use crate::hooks::{HookContext, HookEvent, HookInput};
use crate::session::ToolExecution;
use crate::types::{
    ContentBlock, Message, PermissionDenial, StopReason, ToolResultBlock, Usage, context_window,
};

impl Agent {
    fn check_budget(&self) -> crate::Result<()> {
        BudgetContext {
            tracker: &self.budget_tracker,
            tenant: self.tenant_budget.as_deref(),
            config: &self.config.budget,
        }
        .check()
    }

    pub async fn execute(&self, prompt: &str) -> crate::Result<AgentResult> {
        let timeout = self
            .config
            .execution
            .timeout
            .unwrap_or(std::time::Duration::from_secs(600));

        if self.state.is_executing() {
            self.state
                .enqueue(prompt)
                .await
                .map_err(|e| crate::Error::Session(format!("Queue full: {}", e)))?;
            return self.wait_for_execution(timeout).await;
        }

        tokio::time::timeout(timeout, self.execute_inner(prompt))
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
                    return self.execute_inner(&merged.content).await;
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
            .filter_map(|m| {
                m.content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .next()
            })
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

    #[instrument(skip(self, prompt), fields(session_id = %self.session_id))]
    async fn execute_inner(&self, prompt: &str) -> crate::Result<AgentResult> {
        let _guard = self.state.acquire_execution().await;
        let execution_start = Instant::now();
        let hook_ctx = self.hook_context();

        let session_start_input = HookInput::session_start(&*self.session_id);
        if let Err(e) = self
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
            .hooks
            .execute(HookEvent::UserPromptSubmit, prompt_input, &hook_ctx)
            .await?;

        if !prompt_output.continue_execution {
            let session_end_input = HookInput::session_end(&*self.session_id);
            if let Err(e) = self
                .hooks
                .execute(HookEvent::SessionEnd, session_end_input, &hook_ctx)
                .await
            {
                warn!(error = %e, "SessionEnd hook failed");
            }
            return Err(crate::Error::Permission(
                prompt_output
                    .stop_reason
                    .unwrap_or_else(|| "Blocked by hook".into()),
            ));
        }

        self.state
            .with_session_mut(|session| {
                session.add_user_message(&final_prompt);
            })
            .await;

        let mut metrics = AgentMetrics::default();
        let mut final_text = String::new();
        let mut final_stop_reason = StopReason::EndTurn;
        let mut dynamic_rules_context = String::new();
        let mut total_usage = Usage::default();

        let mut request_builder = {
            let builder = RequestBuilder::new(&self.config, Arc::clone(&self.tools));

            if let Some(ref tsm) = self.tool_search_manager {
                let prepared = tsm.prepare_tools().await;
                if prepared.use_search {
                    info!(
                        immediate = prepared.immediate.len(),
                        deferred = prepared.deferred.len(),
                        tokens_saved = prepared.token_savings(),
                        "MCP Progressive Disclosure active"
                    );
                }
                builder.prepared_tools(prepared)
            } else {
                builder
            }
        };
        let max_tokens = context_window::for_model(&self.config.model.primary);

        info!(prompt_len = final_prompt.len(), "Starting agent execution");

        loop {
            metrics.iterations += 1;
            if metrics.iterations > self.config.execution.max_iterations {
                warn!(
                    max = self.config.execution.max_iterations,
                    "Max iterations reached"
                );
                break;
            }

            self.check_budget()?;

            let budget_ctx = BudgetContext {
                tracker: &self.budget_tracker,
                tenant: self.tenant_budget.as_deref(),
                config: &self.config.budget,
            };
            if let Some(fallback) = budget_ctx.fallback_model() {
                request_builder.set_model(fallback);
            }

            debug!(iteration = metrics.iterations, "Starting iteration");

            let messages = self
                .state
                .with_session(|session| {
                    session.to_api_messages_with_cache(self.config.cache.message_ttl_option())
                })
                .await;

            let api_start = Instant::now();
            let request = request_builder.build(messages, &dynamic_rules_context);
            let response = self.client.send_with_auth_retry(request).await?;
            let api_duration_ms = api_start.elapsed().as_millis() as u64;
            metrics.record_api_call_with_timing(api_duration_ms);
            debug!(api_time_ms = api_duration_ms, "API call completed");

            self.state
                .with_session_mut(|session| {
                    session.update_usage(&response.usage);
                })
                .await;

            accumulate_response_usage(
                &mut total_usage,
                &mut metrics,
                &self.budget_tracker,
                self.tenant_budget.as_deref(),
                &self.config.model.primary,
                &response.usage,
            );

            final_text = response.text();
            final_stop_reason = response.stop_reason.unwrap_or(StopReason::EndTurn);

            self.state
                .with_session_mut(|session| {
                    session.add_assistant_message(response.content.clone(), Some(response.usage));
                })
                .await;

            if !response.wants_tool_use() {
                debug!("No tool use requested, ending loop");
                break;
            }

            let tool_uses = response.tool_uses();
            let hook_ctx = self.hook_context();

            let mut prepared = Vec::with_capacity(tool_uses.len());
            let mut blocked = Vec::with_capacity(tool_uses.len());

            for tool_use in &tool_uses {
                let pre_input = HookInput::pre_tool_use(
                    &*self.session_id,
                    &tool_use.name,
                    tool_use.input.clone(),
                );
                let pre_output = self
                    .hooks
                    .execute(HookEvent::PreToolUse, pre_input, &hook_ctx)
                    .await?;

                if !pre_output.continue_execution {
                    debug!(tool = %tool_use.name, "Tool blocked by hook");
                    let reason = pre_output
                        .stop_reason
                        .clone()
                        .unwrap_or_else(|| "Blocked by hook".into());
                    blocked.push(ToolResultBlock::error(&tool_use.id, reason.clone()));
                    metrics.record_permission_denial(
                        PermissionDenial::new(&tool_use.name, &tool_use.id, tool_use.input.clone())
                            .reason(reason),
                    );
                } else {
                    let input = pre_output.updated_input.unwrap_or(tool_use.input.clone());
                    self.state
                        .append_graph_node(
                            crate::graph::NodeKind::ToolCall,
                            serde_json::json!({
                                "tool_use_id": tool_use.id.clone(),
                                "tool_name": tool_use.name.clone(),
                                "tool_input": input.clone(),
                            }),
                        )
                        .await;
                    prepared.push((tool_use.id.clone(), tool_use.name.clone(), input));
                }
            }

            let tool_futures = prepared.into_iter().map(|(id, name, input)| {
                let tools = &self.tools;
                async move {
                    let start = Instant::now();
                    let result = tools.execute(&name, input.clone()).await;
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
                    &self.budget_tracker,
                    &result,
                    &name,
                )
                .await;

                try_activate_dynamic_rules(
                    &name,
                    &input,
                    &self.orchestrator,
                    &mut dynamic_rules_context,
                )
                .await;

                run_post_tool_hooks(
                    &self.hooks,
                    &hook_ctx,
                    &self.session_id,
                    &name,
                    is_error,
                    &result,
                )
                .await;

                self.state
                    .record_tool_execution(
                        ToolExecution::new(self.state.session_id(), &name, input.clone())
                            .message(id.clone())
                            .output(result.output.text(), is_error)
                            .duration(duration_ms),
                    )
                    .await;

                results.push(ToolResultBlock::from_tool_result(&id, &result));
            }

            self.state
                .with_session_mut(|session| {
                    session.add_tool_results(results);
                })
                .await;

            if all_non_retryable {
                warn!("All tool calls failed with non-retryable errors, ending execution");
                break;
            }

            handle_compaction(
                &self.state,
                &self.client,
                &self.tools,
                &self.hooks,
                &hook_ctx,
                &self.session_id,
                &self.config.execution,
                max_tokens,
                &mut metrics,
            )
            .await;
        }

        metrics.execution_time_ms = execution_start.elapsed().as_millis() as u64;

        run_stop_hooks(&self.hooks, &hook_ctx, &self.session_id).await;

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
        HookContext::new(&*self.session_id)
            .cwd(self.config.working_dir.clone().unwrap_or_default())
            .env(self.config.security.env.clone())
    }

    fn extract_structured_output(&self, text: &str) -> Option<serde_json::Value> {
        common::extract_structured_output(self.config.prompt.output_schema.as_ref(), text)
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
