//! Agent streaming execution with session-based context management.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use futures::{Stream, StreamExt, stream};
use tracing::{debug, warn};

use super::AgentMetrics;
use super::common::{
    BudgetContext, accumulate_inner_usage, accumulate_response_usage, emit_cost_report,
    emit_tokens_consumed, emit_tool_executed, emit_tool_progress, handle_compaction,
    maybe_emit_budget_alert, maybe_invoke_explicit_skill_command, run_post_tool_hooks,
    run_stop_hooks, try_activate_dynamic_rules,
};
use super::events::{AgentEvent, AgentResult};
use super::executor::Agent;
use super::request::RequestBuilder;
use super::run_config::RunConfig;
use super::runtime::AgentRuntime;
use crate::authorization::approval::DEFAULT_APPROVAL_TIMEOUT_SECS;
use crate::authorization::{ApprovalRequest, ApprovalResponse, AuthorizationDenied};
use crate::client::provider_client::ChunkStream;
use crate::hooks::{HookContext, HookEvent, HookInput};
use crate::ir::ContentPart;
use crate::ir::ModelStreamChunk;
use crate::session::ToolExecution;
use crate::session::{MessageMetadata, SessionAccessScope, SessionManager, ToolState};
use crate::types::context_window;

impl Agent {
    pub async fn execute_stream(
        &self,
        prompt: &str,
    ) -> crate::Result<impl Stream<Item = crate::Result<AgentEvent>> + Send> {
        self.execute_stream_inner(prompt.to_string(), None).await
    }

    /// Stream execution with per-run configuration overrides.
    pub async fn execute_stream_with(
        &self,
        prompt: impl Into<String>,
        run_config: RunConfig,
    ) -> crate::Result<impl Stream<Item = crate::Result<AgentEvent>> + Send> {
        self.execute_stream_inner(prompt.into(), Some(run_config))
            .await
    }

    async fn execute_stream_inner(
        &self,
        prompt: String,
        run_config: Option<RunConfig>,
    ) -> crate::Result<impl Stream<Item = crate::Result<AgentEvent>> + Send> {
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
            self.state.enqueue(&*prompt).await.map_err(|e| {
                crate::Error::Session(crate::session::SessionError::QueueFull {
                    message: e.to_string(),
                })
            })?;
        }
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

        let mut request_builder = RequestBuilder::new(
            &self.runtime.config,
            Arc::clone(&self.runtime.tools),
            static_context,
        )
        .metadata(metadata);

        // Apply RunConfig overrides.
        if let Some(ref rc) = run_config {
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

        let max_iterations_override = run_config
            .as_ref()
            .map(|rc| rc.effective_max_iterations(self.runtime.config.execution.max_iterations));

        let state = StreamState::new(
            StreamStateConfig {
                tool_state: self.state.clone(),
                runtime: Arc::clone(&self.runtime),
                hook_context: self.hook_context(),
                request_builder,
                session_id: Arc::clone(&self.session_id),
                session_manager: self.session_manager.clone(),
                session_scope: self.session_scope.clone(),
            },
            timeout,
            prompt,
            max_iterations_override,
        );

        Ok(stream::unfold(state, |mut state| async move {
            state.next_event().await.map(|event| (event, state))
        }))
    }
}

struct StreamStateConfig {
    tool_state: ToolState,
    runtime: Arc<AgentRuntime>,
    hook_context: HookContext,
    request_builder: RequestBuilder,
    session_id: Arc<str>,
    session_manager: Option<SessionManager>,
    session_scope: Option<SessionAccessScope>,
}

enum StreamPollResult {
    Event(crate::Result<AgentEvent>),
    Continue,
    StreamEnded,
}

enum Phase {
    StartRequest,
    Streaming(Box<StreamingPhase>),
    StreamEnded {
        accumulated_usage: crate::ir::Usage,
    },
    /// Tools are running — yields ToolProgress events in real-time via tokio::select!.
    ExecutingTools(Box<ToolExecutionPhase>),
    EmittingToolResults {
        events: VecDeque<AgentEvent>,
    },
    Done,
}

/// Tool execution result: (id, name, input, result, duration_ms).
type ToolExecResult = (
    String,
    String,
    serde_json::Value,
    crate::types::ToolResult,
    u64,
);

/// Annotated progress event: (tool_id, tool_name, event).
type AnnotatedProgress = (String, String, crate::tools::ProgressEvent);

/// Real-time tool execution state.
///
/// Preserves safety partitioning: each batch runs to completion before the next
/// batch is spawned. Within a batch, tools run concurrently. Progress events
/// are yielded to the stream consumer in real-time via `tokio::select!`.
///
/// Pre-processing events (ToolStart, ToolBlocked) are stored in `pre_events`
/// and yielded first, before progress events from tool execution.
struct ToolExecutionPhase {
    /// Pre-processing events to yield before tool progress (ToolStart, ToolBlocked).
    pre_events: VecDeque<AgentEvent>,
    /// Progress channel shared across all relay tasks in the current batch.
    progress_rx: tokio::sync::mpsc::Receiver<AnnotatedProgress>,
    /// Active tool handles for the current batch.
    tool_handles: futures::stream::FuturesUnordered<tokio::task::JoinHandle<ToolExecResult>>,
    /// Results accumulated across all completed batches.
    completed: Vec<ToolExecResult>,
    /// Remaining batches to execute after current batch completes.
    remaining_batches: Vec<Vec<(String, String, serde_json::Value)>>,
    /// Context needed to spawn the next batch.
    tools_ref: Arc<crate::tools::ToolRegistry>,
    context_scope: Option<crate::SharedContextScope>,
    /// Cancellation token propagated to tool execution.
    cancel_token: tokio_util::sync::CancellationToken,
    /// Blocked tool results from pre-processing (passed to finalize).
    blocked_results: Vec<ContentPart>,
}

struct StreamingPhase {
    stream: ChunkStream,
    accumulated_usage: crate::ir::Usage,
}

/// Accumulated tool call from streaming chunks, replacing `ToolUseBlock`.
struct PendingToolCall {
    id: String,
    name: String,
    arguments: serde_json::Value,
}

struct StreamState {
    cfg: StreamStateConfig,
    timeout: std::time::Duration,
    chunk_timeout: std::time::Duration,
    dynamic_rules: String,
    metrics: AgentMetrics,
    start_time: Instant,
    last_chunk_time: Instant,
    pending_tool_results: Vec<ContentPart>,
    pending_tool_uses: Vec<PendingToolCall>,
    recovery_attempts: u32,
    /// Accumulators for tool_use content blocks being streamed, keyed by
    /// the codec's `index`. Codecs (esp. OpenAI Chat Completions) can
    /// interleave deltas for multiple tool calls in a single stream, so
    /// we MUST key on `index` rather than tracking a single in-flight call.
    /// Each entry is `(tool_call_id, tool_name, json_buffer)`.
    accumulating_tool_calls: std::collections::HashMap<usize, (String, String, String)>,
    final_text: String,
    final_thinking: String,
    thinking_signature: Option<crate::ir::ReasoningSignature>,
    finish_reason: Option<crate::ir::FinishReason>,
    total_usage: crate::ir::Usage,
    /// Token estimate from the most recent preflight. Stashed here so
    /// the finish-chunk path can reconcile it against
    /// `accumulated_usage` when the response completes. Reset per
    /// iteration in `do_start_request`.
    last_request_estimate: Option<crate::budget::RequestTokenEstimate>,
    phase: Phase,
    all_non_retryable: bool,
    session_started: bool,
    prompt_submitted: bool,
    initial_prompt: Option<String>,
    max_iterations_override: Option<usize>,
}

impl StreamState {
    fn new(
        cfg: StreamStateConfig,
        timeout: std::time::Duration,
        prompt: String,
        max_iterations_override: Option<usize>,
    ) -> Self {
        let chunk_timeout = cfg.runtime.config.execution.chunk_timeout;
        let now = Instant::now();
        Self {
            cfg,
            timeout,
            chunk_timeout,
            dynamic_rules: String::new(),
            metrics: AgentMetrics::default(),
            start_time: now,
            last_chunk_time: now,
            pending_tool_results: Vec::new(),
            pending_tool_uses: Vec::new(),
            recovery_attempts: 0,
            accumulating_tool_calls: std::collections::HashMap::new(),
            final_text: String::new(),
            final_thinking: String::new(),
            thinking_signature: None,
            finish_reason: None,
            total_usage: crate::ir::Usage::default(),
            last_request_estimate: None,
            phase: Phase::StartRequest,
            all_non_retryable: false,
            session_started: false,
            prompt_submitted: false,
            initial_prompt: Some(prompt),
            max_iterations_override,
        }
    }

    fn extract_structured_output(&self, text: &str) -> Option<serde_json::Value> {
        super::common::extract_structured_output(
            self.cfg.runtime.config.prompt.output_schema.as_ref(),
            text,
        )
    }

    fn build_result(
        &self,
        iterations: usize,
        stop_reason: crate::ir::FinishReason,
        messages: Vec<crate::ir::Message>,
    ) -> AgentResult {
        emit_cost_report(
            self.cfg.runtime.event_bus.as_deref(),
            &self.metrics,
            &self.cfg.session_id,
        );
        let structured_output = self.extract_structured_output(&self.final_text);
        AgentResult::new(
            self.final_text.clone(),
            self.total_usage.clone(),
            iterations,
            stop_reason,
            self.metrics.clone(),
            self.cfg.session_id.to_string(),
            structured_output,
            messages,
        )
    }

    async fn next_event(&mut self) -> Option<crate::Result<AgentEvent>> {
        loop {
            if matches!(self.phase, Phase::Done) {
                return None;
            }

            if self.start_time.elapsed() > self.timeout {
                self.phase = Phase::Done;
                return Some(Err(crate::Error::Timeout(self.timeout)));
            }

            if let Some(event) = self.check_budget_exceeded() {
                return Some(event);
            }

            match std::mem::replace(&mut self.phase, Phase::Done) {
                Phase::StartRequest => {
                    if let Some(result) = self.do_start_request().await {
                        return Some(result);
                    }
                }
                Phase::Streaming(mut streaming) => {
                    match self
                        .do_poll_stream(&mut streaming.stream, &mut streaming.accumulated_usage)
                        .await
                    {
                        StreamPollResult::Event(event) => {
                            self.phase = Phase::Streaming(streaming);
                            return Some(event);
                        }
                        StreamPollResult::Continue => {
                            self.phase = Phase::Streaming(streaming);
                        }
                        StreamPollResult::StreamEnded => {
                            self.phase = Phase::StreamEnded {
                                accumulated_usage: streaming.accumulated_usage,
                            };
                        }
                    }
                }
                Phase::StreamEnded { accumulated_usage } => {
                    if let Some(event) = self.do_handle_stream_end(accumulated_usage).await {
                        return Some(event);
                    }
                }
                Phase::ExecutingTools(mut exec) => {
                    // Yield pre-processing events first (ToolStart, ToolBlocked)
                    if let Some(event) = exec.pre_events.pop_front() {
                        self.phase = Phase::ExecutingTools(exec);
                        return Some(Ok(event));
                    }

                    tokio::select! {
                        biased;
                        // Progress events have priority — yield immediately
                        recv = exec.progress_rx.recv() => {
                            match recv {
                                Some((id, name, p)) => {
                                    emit_tool_progress(
                                        self.cfg.runtime.event_bus.as_deref(),
                                        &id, &name, &p.step, &p.status,
                                    );
                                    self.phase = Phase::ExecutingTools(exec);
                                    return Some(Ok(AgentEvent::ToolProgress {
                                        id, name,
                                        step: p.step, status: p.status,
                                        timestamp: Some(p.timestamp),
                                        duration_ms: p.duration_ms,
                                        metadata: p.metadata,
                                    }));
                                }
                                None => {
                                    // All relay senders dropped — drain remaining tool handles
                                    while let Some(join_result) = exec.tool_handles.next().await {
                                        match join_result {
                                            Ok(result) => exec.completed.push(result),
                                            Err(e) => warn!(error = %e, "Tool task panicked"),
                                        }
                                    }
                                    if let Err(e) = self.advance_to_next_batch_or_finalize(exec).await {
                                        self.phase = Phase::Done;
                                        return Some(Err(e));
                                    }
                                }
                            }
                        }
                        // Tool completed
                        join_result = exec.tool_handles.next(), if !exec.tool_handles.is_empty() => {
                            match join_result {
                                Some(Ok(result)) => exec.completed.push(result),
                                Some(Err(e)) => warn!(error = %e, "Tool task panicked"),
                                None => {}
                            }
                            if exec.tool_handles.is_empty() {
                                // Current batch done — drain remaining progress from relay tasks
                                while let Some((id, name, p)) = exec.progress_rx.recv().await {
                                    emit_tool_progress(
                                        self.cfg.runtime.event_bus.as_deref(),
                                        &id, &name, &p.step, &p.status,
                                    );
                                    exec.pre_events.push_back(AgentEvent::ToolProgress {
                                        id, name,
                                        step: p.step, status: p.status,
                                        timestamp: Some(p.timestamp),
                                        duration_ms: p.duration_ms,
                                        metadata: p.metadata,
                                    });
                                }
                                if let Err(e) = self.advance_to_next_batch_or_finalize(exec).await {
                                    self.phase = Phase::Done;
                                    return Some(Err(e));
                                }
                            } else {
                                self.phase = Phase::ExecutingTools(exec);
                            }
                        }
                    }
                }
                Phase::EmittingToolResults { mut events } => {
                    if let Some(event) = events.pop_front() {
                        self.phase = Phase::EmittingToolResults { events };
                        return Some(Ok(event));
                    }
                    if self.all_non_retryable {
                        self.phase = Phase::Done;
                        self.metrics.execution_time_ms =
                            self.start_time.elapsed().as_millis() as u64;
                        run_stop_hooks(
                            &self.cfg.runtime.hooks,
                            &self.cfg.hook_context,
                            &self.cfg.session_id,
                        )
                        .await;
                        let messages = self
                            .cfg
                            .tool_state
                            .with_session(|session| session.to_api_messages())
                            .await;
                        let result = self.build_result(
                            self.metrics.iterations,
                            crate::ir::FinishReason::Stop,
                            messages,
                        );
                        return Some(Ok(AgentEvent::Complete(Box::new(result))));
                    }
                    self.phase = Phase::StartRequest;
                }
                Phase::Done => return None,
            }
        }
    }

    fn check_budget_exceeded(&mut self) -> Option<crate::Result<AgentEvent>> {
        let result = BudgetContext {
            tracker: &self.cfg.runtime.budget_tracker,
            tenant: self.cfg.runtime.tenant_budget.as_deref(),
            config: &self.cfg.runtime.config.budget,
        }
        .check();

        if let Err(e) = result {
            self.phase = Phase::Done;
            return Some(Err(e));
        }

        None
    }

    async fn do_start_request(&mut self) -> Option<crate::Result<AgentEvent>> {
        if !self.session_started {
            self.session_started = true;

            // Propagate EventBus to Session/Graph for SessionChanged,
            // BranchForked, and CheckpointCreated events.
            if let Some(ref bus) = self.cfg.runtime.event_bus {
                self.cfg.tool_state.with_event_bus(Arc::clone(bus)).await;
            }

            let session_start_input = HookInput::session_start(&*self.cfg.session_id);
            if let Err(e) = self
                .cfg
                .runtime
                .hooks
                .execute(
                    HookEvent::SessionStart,
                    session_start_input,
                    &self.cfg.hook_context,
                )
                .await
            {
                warn!(error = %e, "SessionStart hook failed");
            }
        }

        if !self.prompt_submitted {
            if let Some(prompt) = self.initial_prompt.take() {
                let prompt_input = HookInput::user_prompt_submit(&*self.cfg.session_id, &prompt);
                let prompt_output = match self
                    .cfg
                    .runtime
                    .hooks
                    .execute(
                        HookEvent::UserPromptSubmit,
                        prompt_input,
                        &self.cfg.hook_context,
                    )
                    .await
                {
                    Ok(output) => output,
                    Err(e) => {
                        self.phase = Phase::Done;
                        return Some(Err(e));
                    }
                };

                if !prompt_output.continue_execution {
                    self.phase = Phase::Done;
                    return Some(Err(crate::Error::Authorization(
                        prompt_output
                            .stop_reason
                            .unwrap_or_else(|| "Blocked by hook".into()),
                    )));
                }

                if let Err(e) = self
                    .cfg
                    .tool_state
                    .with_session_mut(|session| session.add_user_message(&prompt))
                    .await
                {
                    self.phase = Phase::Done;
                    return Some(Err(e.into()));
                }
                if let Err(e) = persist_stream_session_state(
                    self.cfg.session_manager.clone(),
                    self.cfg.session_scope.clone(),
                    self.cfg.tool_state.clone(),
                )
                .await
                {
                    self.phase = Phase::Done;
                    return Some(Err(e));
                }

                match maybe_invoke_explicit_skill_command(
                    &self.cfg.runtime.tools,
                    &self.cfg.tool_state,
                    &self.cfg.runtime.hooks,
                    &self.cfg.hook_context,
                    &self.cfg.session_id,
                    &prompt,
                    &mut self.metrics,
                )
                .await
                {
                    Ok(true) => {
                        if let Err(e) = persist_stream_session_state(
                            self.cfg.session_manager.clone(),
                            self.cfg.session_scope.clone(),
                            self.cfg.tool_state.clone(),
                        )
                        .await
                        {
                            self.phase = Phase::Done;
                            return Some(Err(e));
                        }
                    }
                    Ok(false) => {}
                    Err(e) => {
                        self.phase = Phase::Done;
                        return Some(Err(e));
                    }
                }
            }
            self.prompt_submitted = true;
        }

        if self.cfg.runtime.shutdown.is_cancelled() {
            if let Err(e) = persist_stream_session_state(
                self.cfg.session_manager.clone(),
                self.cfg.session_scope.clone(),
                self.cfg.tool_state.clone(),
            )
            .await
            {
                self.phase = Phase::Done;
                return Some(Err(e));
            }
            self.phase = Phase::Done;
            self.metrics.execution_time_ms = self.start_time.elapsed().as_millis() as u64;

            run_stop_hooks(
                &self.cfg.runtime.hooks,
                &self.cfg.hook_context,
                &self.cfg.session_id,
            )
            .await;

            let messages = self
                .cfg
                .tool_state
                .with_session(|session| session.to_api_messages())
                .await;
            let result = self.build_result(
                self.metrics.iterations,
                crate::ir::FinishReason::Stop,
                messages,
            );
            return Some(Ok(AgentEvent::Complete(Box::new(result))));
        }

        self.metrics.iterations += 1;
        let effective_max_iterations = self
            .max_iterations_override
            .unwrap_or(self.cfg.runtime.config.execution.max_iterations);
        if self.metrics.iterations > effective_max_iterations {
            self.phase = Phase::Done;
            self.metrics.execution_time_ms = self.start_time.elapsed().as_millis() as u64;

            run_stop_hooks(
                &self.cfg.runtime.hooks,
                &self.cfg.hook_context,
                &self.cfg.session_id,
            )
            .await;

            let messages = self
                .cfg
                .tool_state
                .with_session(|session| session.to_api_messages())
                .await;
            let result = self.build_result(
                self.metrics.iterations - 1,
                crate::ir::FinishReason::Length,
                messages,
            );
            return Some(Ok(AgentEvent::Complete(Box::new(result))));
        }

        let budget_ctx = BudgetContext {
            tracker: &self.cfg.runtime.budget_tracker,
            tenant: self.cfg.runtime.tenant_budget.as_deref(),
            config: &self.cfg.runtime.config.budget,
        };
        if let Some(fallback) = budget_ctx.fallback_model() {
            self.cfg.request_builder.set_model(fallback);
        }

        let messages = self
            .cfg
            .tool_state
            .with_session(|session| session.to_api_messages())
            .await;

        // Fire ModelSelection hook - allows overriding the model
        let model_input = HookInput::model_selection(
            &*self.cfg.session_id,
            self.cfg.request_builder.current_model(),
            messages.len(),
            self.cfg.request_builder.has_tools(),
        );
        match self
            .cfg
            .runtime
            .hooks
            .execute(
                HookEvent::ModelSelection,
                model_input,
                &self.cfg.hook_context,
            )
            .await
        {
            Ok(output) => {
                if let Some(ref updated) = output.updated_input
                    && let Some(model_override) =
                        updated.get("model").and_then(serde_json::Value::as_str)
                {
                    self.cfg.request_builder.set_model(model_override);
                }
            }
            Err(e) => {
                warn!(error = %e, "ModelSelection hook failed, blocking request");
                self.phase = Phase::Done;
                return Some(Err(e));
            }
        }

        // Fire PreMessage hook - can block the message
        let messages_json: Vec<serde_json::Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        let pre_msg_input = HookInput::pre_message(
            &*self.cfg.session_id,
            messages_json,
            self.cfg.request_builder.current_model(),
        );
        match self
            .cfg
            .runtime
            .hooks
            .execute(HookEvent::PreMessage, pre_msg_input, &self.cfg.hook_context)
            .await
        {
            Ok(output) => {
                if !output.continue_execution {
                    self.phase = Phase::Done;
                    return Some(Err(crate::Error::Authorization(
                        output
                            .stop_reason
                            .unwrap_or_else(|| "Blocked by PreMessage hook".into()),
                    )));
                }
            }
            Err(e) => {
                self.phase = Phase::Done;
                return Some(Err(e));
            }
        }

        let ir_request = self
            .cfg
            .request_builder
            .build(messages, &self.dynamic_rules);

        // Preflight budget check: estimate the cost of the pending
        // request against the session / tenant budgets and bail out
        // before hitting the wire if it would overrun. Prevents
        // bursting where a single large call blows past a limit that
        // historical spend alone had not yet tripped.
        let preflight_ctx = BudgetContext {
            tracker: &self.cfg.runtime.budget_tracker,
            tenant: self.cfg.runtime.tenant_budget.as_deref(),
            config: &self.cfg.runtime.config.budget,
        };
        if let Err(e) = preflight_ctx.preflight(&ir_request) {
            self.phase = Phase::Done;
            return Some(Err(e));
        }

        // Stash the token estimate so the finish-chunk path can
        // reconcile it against the provider's reported usage. See
        // [`crate::budget::EstimateReconciler::observe`].
        self.last_request_estimate = Some(crate::budget::estimate_request_tokens(&ir_request));

        let chunk_stream = match self
            .cfg
            .runtime
            .llm
            .send_stream(&ir_request, self.cfg.runtime.shutdown.child_token())
            .await
        {
            Ok(s) => s,
            Err(e) => {
                let executor = super::recovery_executor::RecoveryExecutor {
                    registry: &self.cfg.runtime.recovery_recipes,
                    tool_state: &self.cfg.tool_state,
                    llm: Some(self.cfg.runtime.llm.as_ref()),
                    event_bus: self.cfg.runtime.event_bus.as_deref(),
                };
                match executor.apply(&e, &mut self.recovery_attempts).await {
                    super::recovery_executor::RecoveryOutcome::Retry => {
                        self.phase = Phase::StartRequest;
                        return None;
                    }
                    super::recovery_executor::RecoveryOutcome::Abort => {
                        self.phase = Phase::Done;
                        return Some(Err(e));
                    }
                }
            }
        };
        self.recovery_attempts = 0;

        self.metrics.record_api_call();

        self.phase = Phase::Streaming(Box::new(StreamingPhase {
            stream: chunk_stream,
            accumulated_usage: crate::ir::Usage::default(),
        }));

        None
    }

    async fn do_poll_stream(
        &mut self,
        stream: &mut ChunkStream,
        accumulated_usage: &mut crate::ir::Usage,
    ) -> StreamPollResult {
        // Race chunk read against runtime shutdown so cancel takes effect
        // mid-stream instead of only between iterations. The inner
        // `ChunkStream` is also wired to the same shutdown token at
        // `send_stream` time, so dropping the stream here will release
        // the underlying HTTP body as well.
        let shutdown = self.cfg.runtime.shutdown.clone();
        let chunk_result = tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                self.phase = Phase::Done;
                return StreamPollResult::Event(Err(crate::Error::Stream(
                    "Streaming cancelled by runtime shutdown".into(),
                )));
            }
            res = tokio::time::timeout(self.chunk_timeout, stream.next()) => res,
        };

        match chunk_result {
            Ok(Some(Ok(chunk))) => {
                self.last_chunk_time = Instant::now();
                self.handle_stream_chunk(chunk, accumulated_usage)
            }
            Ok(Some(Err(e))) => {
                self.phase = Phase::Done;
                StreamPollResult::Event(Err(e))
            }
            Ok(None) => StreamPollResult::StreamEnded,
            Err(_) => {
                self.phase = Phase::Done;
                StreamPollResult::Event(Err(crate::Error::Stream(format!(
                    "Chunk timeout after {:?} (no data received)",
                    self.chunk_timeout
                ))))
            }
        }
    }

    fn handle_stream_chunk(
        &mut self,
        chunk: ModelStreamChunk,
        accumulated_usage: &mut crate::ir::Usage,
    ) -> StreamPollResult {
        match chunk {
            ModelStreamChunk::MessageStart { .. } => StreamPollResult::Continue,
            ModelStreamChunk::TextDelta { text, .. } => {
                self.final_text.push_str(&text);
                self.fire_post_stream_chunk_sync(&text, "text");
                if let Some(ref bus) = self.cfg.runtime.event_bus {
                    bus.emit_simple(
                        crate::events::EventKind::StreamChunk,
                        serde_json::json!({
                            "chunk_type": "text",
                            "length": text.len(),
                        }),
                    );
                }
                StreamPollResult::Event(Ok(AgentEvent::Text { delta: text }))
            }
            ModelStreamChunk::ReasoningDelta { text, .. } => {
                self.final_thinking.push_str(&text);
                self.fire_post_stream_chunk_sync(&text, "thinking");
                if let Some(ref bus) = self.cfg.runtime.event_bus {
                    bus.emit_simple(
                        crate::events::EventKind::StreamChunk,
                        serde_json::json!({
                            "chunk_type": "thinking",
                            "length": text.len(),
                        }),
                    );
                }
                StreamPollResult::Event(Ok(AgentEvent::Thinking { content: text }))
            }
            ModelStreamChunk::ToolCallStart {
                index, id, name, ..
            } => {
                self.accumulating_tool_calls
                    .insert(index, (id, name, String::new()));
                StreamPollResult::Continue
            }
            ModelStreamChunk::ToolCallArgsDelta {
                index,
                partial_json,
            } => {
                if let Some((_, _, json_buf)) = self.accumulating_tool_calls.get_mut(&index) {
                    json_buf.push_str(&partial_json);
                } else {
                    tracing::warn!(
                        index,
                        "ToolCallArgsDelta received without matching ToolCallStart — dropped"
                    );
                }
                StreamPollResult::Continue
            }
            ModelStreamChunk::ToolCallEnd { index } => {
                if let Some((id, name, json_buf)) = self.accumulating_tool_calls.remove(&index) {
                    let input: serde_json::Value = match serde_json::from_str(&json_buf) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                tool_name = %name,
                                error = %e,
                                json_len = json_buf.len(),
                                "Malformed tool call arguments JSON, defaulting to {{}}"
                            );
                            serde_json::json!({})
                        }
                    };
                    let tool_call = PendingToolCall {
                        id,
                        name: name.clone(),
                        arguments: input,
                    };
                    self.fire_post_stream_chunk_sync(&name, "tool_use");
                    if let Some(ref bus) = self.cfg.runtime.event_bus {
                        bus.emit_simple(
                            crate::events::EventKind::StreamChunk,
                            serde_json::json!({
                                "chunk_type": "tool_use",
                                "tool_name": &tool_call.name,
                            }),
                        );
                    }
                    self.pending_tool_uses.push(tool_call);
                } else {
                    tracing::warn!(
                        index,
                        "ToolCallEnd received without matching ToolCallStart — dropped"
                    );
                }
                StreamPollResult::Continue
            }
            ModelStreamChunk::UsageDelta(partial) => {
                partial.apply(accumulated_usage);
                StreamPollResult::Continue
            }
            ModelStreamChunk::Finish { reason, usage } => {
                self.finish_reason = Some(reason);
                *accumulated_usage = usage;
                StreamPollResult::StreamEnded
            }
            ModelStreamChunk::Error { message, .. } => {
                self.phase = Phase::Done;
                StreamPollResult::Event(Err(crate::Error::Stream(message)))
            }
            ModelStreamChunk::ReasoningSignature { signature, .. } => {
                self.thinking_signature = Some(signature);
                StreamPollResult::Continue
            }
            ModelStreamChunk::Heartbeat
            | ModelStreamChunk::Warning(_)
            | ModelStreamChunk::Source(_)
            | ModelStreamChunk::BuiltinToolEvent { .. } => StreamPollResult::Continue,
        }
    }

    /// Fire PostStreamChunk hook in a non-blocking, fail-open manner.
    fn fire_post_stream_chunk_sync(&self, chunk_text: &str, chunk_type: &str) {
        let hooks = Arc::clone(&self.cfg.runtime.hooks);
        let hook_context = self.cfg.hook_context.clone();
        let input = HookInput::post_stream_chunk(
            &*self.cfg.session_id,
            chunk_text,
            chunk_type,
            Some(self.final_text.clone()),
        );
        tokio::spawn(async move {
            let _ = hooks
                .execute(HookEvent::PostStreamChunk, input, &hook_context)
                .await;
        });
    }

    async fn do_handle_stream_end(
        &mut self,
        accumulated_usage: crate::ir::Usage,
    ) -> Option<crate::Result<AgentEvent>> {
        // Fire PostMessage hook (observation only, fail-open)
        let stop_reason_str = self.finish_reason.as_ref().map(|r| format!("{:?}", r));
        let post_msg_input = HookInput::post_message(
            &*self.cfg.session_id,
            self.cfg.request_builder.current_model(),
            stop_reason_str,
            accumulated_usage.input_tokens,
            accumulated_usage.output_tokens,
        );
        let _ = self
            .cfg
            .runtime
            .hooks
            .execute(
                HookEvent::PostMessage,
                post_msg_input,
                &self.cfg.hook_context,
            )
            .await;

        if let Err(e) = accumulate_response_usage(
            &mut self.total_usage,
            &mut self.metrics,
            &self.cfg.runtime.budget_tracker,
            self.cfg.runtime.tenant_budget.as_deref(),
            &self.cfg.runtime.config.model.primary,
            &accumulated_usage,
        ) {
            return Some(Err(e));
        }

        // Reconcile preflight estimate against actual usage. The
        // estimate was stashed in `last_request_estimate` before the
        // request was sent; comparing here lets operators calibrate
        // the 4-chars-per-token heuristic over time via structured
        // tracing events.
        if let Some(estimate) = self.last_request_estimate.take() {
            let drift = crate::budget::EstimateReconciler::compute(estimate, &accumulated_usage);
            tracing::debug!(
                target: "branchforge::budget::estimate_drift",
                model = %self.cfg.runtime.config.model.primary,
                estimated_input = drift.estimated_input,
                actual_input = drift.actual_input,
                estimated_output = drift.estimated_output,
                actual_output = drift.actual_output,
                input_ratio = drift.input_ratio.unwrap_or(f64::NAN),
                output_ratio = drift.output_ratio.unwrap_or(f64::NAN),
                is_close = drift.is_close(),
                "Token estimate drift recorded"
            );
        }

        emit_tokens_consumed(
            self.cfg.runtime.event_bus.as_deref(),
            &accumulated_usage,
            &self.cfg.runtime.config.model.primary,
        );

        maybe_emit_budget_alert(
            &self.cfg.runtime.budget_tracker,
            self.cfg.runtime.event_bus.as_deref(),
            self.cfg.runtime.config.budget.alert_threshold_pct,
        );

        let structured_output = self.extract_structured_output(&self.final_text);

        if let Err(e) = self
            .cfg
            .tool_state
            .with_session_mut(|session| -> crate::session::SessionResult<()> {
                let has_thinking = !self.final_thinking.is_empty();
                let text_count = if self.final_text.is_empty() { 0 } else { 1 };
                let thinking_count = if has_thinking { 1 } else { 0 };
                let mut content =
                    Vec::with_capacity(thinking_count + text_count + self.pending_tool_uses.len());
                if has_thinking {
                    content.push(crate::ir::ContentPart::Reasoning {
                        content: crate::ir::ReasoningContent::Visible {
                            text: self.final_thinking.clone(),
                        },
                        kind: crate::ir::ReasoningKind::FullTrace,
                        signature: self.thinking_signature.take(),
                    });
                }
                if !self.final_text.is_empty() {
                    content.push(crate::ir::ContentPart::text(self.final_text.clone()));
                }
                for tool_use in &self.pending_tool_uses {
                    content.push(crate::ir::ContentPart::ToolCall {
                        id: tool_use.id.clone(),
                        name: tool_use.name.clone(),
                        arguments: tool_use.arguments.clone(),
                        origin: crate::ir::ToolOrigin::Local,
                    });
                }
                if !content.is_empty() {
                    session.add_assistant_message_with_metadata(
                        content,
                        Some(accumulated_usage.clone()),
                        MessageMetadata {
                            structured_output: structured_output.clone(),
                            ..Default::default()
                        },
                    )?;
                }
                Ok(())
            })
            .await
        {
            self.phase = Phase::Done;
            return Some(Err(e.into()));
        }
        if let Err(e) = persist_stream_session_state(
            self.cfg.session_manager.clone(),
            self.cfg.session_scope.clone(),
            self.cfg.tool_state.clone(),
        )
        .await
        {
            self.phase = Phase::Done;
            return Some(Err(e));
        }

        if self.pending_tool_uses.is_empty() {
            self.phase = Phase::Done;
            self.metrics.execution_time_ms = self.start_time.elapsed().as_millis() as u64;

            run_stop_hooks(
                &self.cfg.runtime.hooks,
                &self.cfg.hook_context,
                &self.cfg.session_id,
            )
            .await;

            let messages = self
                .cfg
                .tool_state
                .with_session(|session| session.to_api_messages())
                .await;
            let result = self.build_result(
                self.metrics.iterations,
                crate::ir::FinishReason::Stop,
                messages,
            );
            return Some(Ok(AgentEvent::Complete(Box::new(result))));
        }

        match self.execute_tools_parallel().await {
            Ok(()) => {
                // execute_tools_parallel sets self.phase directly:
                // - Phase::ExecutingTools if tools were spawned
                // - Phase::EmittingToolResults or Phase::StartRequest otherwise
                None
            }
            Err(e) => {
                self.phase = Phase::Done;
                Some(Err(e))
            }
        }
    }

    /// Execute all pending tools in parallel (matching batch execution behavior),
    /// then collect results and events for sequential emission.
    async fn execute_tools_parallel(&mut self) -> crate::Result<()> {
        let tool_uses = std::mem::take(&mut self.pending_tool_uses);
        let mut events = Vec::new();
        let mut all_tool_results = Vec::new();

        // Phase 1: Pre-hooks (serial — hooks may depend on ordering)
        let mut prepared = Vec::new();

        for tool_use in &tool_uses {
            let pre_input = HookInput::pre_tool_use(
                &*self.cfg.session_id,
                &tool_use.name,
                tool_use.arguments.clone(),
            );
            let pre_output = self
                .cfg
                .runtime
                .hooks
                .execute(HookEvent::PreToolUse, pre_input, &self.cfg.hook_context)
                .await?;

            if !pre_output.continue_execution {
                let reason = pre_output
                    .stop_reason
                    .clone()
                    .unwrap_or_else(|| "Blocked by hook".into());
                debug!(tool = %tool_use.name, "Tool blocked by hook");

                all_tool_results.push(
                    ContentPart::tool_error(&tool_use.id, reason.clone())
                        .with_tool_name(&tool_use.name),
                );
                self.metrics.record_authorization_denial(
                    AuthorizationDenied::new(
                        &tool_use.name,
                        &tool_use.id,
                        tool_use.arguments.clone(),
                    )
                    .reason(reason.clone()),
                );
                events.push(AgentEvent::ToolBlocked {
                    id: tool_use.id.clone(),
                    name: tool_use.name.clone(),
                    reason,
                });
            } else {
                let actual_input = pre_output
                    .updated_input
                    .unwrap_or(tool_use.arguments.clone());

                // ExecutionMode: Plan mode blocks non-plan tools
                if self.cfg.runtime.execution_mode.is_plan()
                    && !self.cfg.runtime.execution_mode.allows_tool(&tool_use.name)
                {
                    let reason = format!(
                        "Tool '{}' is not available in plan mode. Only read/navigation tools are allowed.",
                        tool_use.name
                    );
                    all_tool_results.push(
                        ContentPart::tool_error(&tool_use.id, reason.clone())
                            .with_tool_name(&tool_use.name),
                    );
                    events.push(AgentEvent::ToolBlocked {
                        id: tool_use.id.clone(),
                        name: tool_use.name.clone(),
                        reason,
                    });
                    continue;
                }

                // ExecutionMode: Supervised mode requires review via approval channel
                if self
                    .cfg
                    .runtime
                    .execution_mode
                    .requires_review(&tool_use.name)
                {
                    events.push(AgentEvent::ToolReview {
                        id: tool_use.id.clone(),
                        name: tool_use.name.clone(),
                        input: actual_input.clone(),
                    });

                    let approval_result = if let Some(ref sender) = self.cfg.runtime.approval_sender
                    {
                        let request = ApprovalRequest {
                            tool_name: tool_use.name.clone(),
                            tool_call_id: tool_use.id.clone(),
                            tool_input: actual_input.clone(),
                            reason: format!(
                                "Tool '{}' requires approval in {} mode",
                                tool_use.name, self.cfg.runtime.execution_mode
                            ),
                        };
                        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
                        if sender.send((request, resp_tx)).await.is_err() {
                            ApprovalResponse::Deny {
                                reason: "Approval channel closed".into(),
                            }
                        } else {
                            match tokio::time::timeout(
                                std::time::Duration::from_secs(DEFAULT_APPROVAL_TIMEOUT_SECS),
                                resp_rx,
                            )
                            .await
                            {
                                Ok(Ok(response)) => response,
                                Ok(Err(_)) => ApprovalResponse::Deny {
                                    reason: "Approval responder dropped".into(),
                                },
                                Err(_) => ApprovalResponse::Deny {
                                    reason: "Approval timed out".into(),
                                },
                            }
                        }
                    } else {
                        ApprovalResponse::Deny {
                            reason: format!(
                                "Tool '{}' requires review but no approval channel is configured.                                  Use AgentBuilder::approval_channel() to enable human-in-the-loop.",
                                tool_use.name
                            ),
                        }
                    };

                    match approval_result {
                        ApprovalResponse::Approve => {
                            debug!(tool = %tool_use.name, "Tool approved by human");
                        }
                        ApprovalResponse::Deny { reason } => {
                            debug!(tool = %tool_use.name, %reason, "Tool denied by human");
                            all_tool_results.push(
                                ContentPart::tool_error(&tool_use.id, reason.clone())
                                    .with_tool_name(&tool_use.name),
                            );
                            self.metrics.record_authorization_denial(
                                AuthorizationDenied::new(
                                    &tool_use.name,
                                    &tool_use.id,
                                    actual_input.clone(),
                                )
                                .reason(reason.clone()),
                            );
                            events.push(AgentEvent::ToolBlocked {
                                id: tool_use.id.clone(),
                                name: tool_use.name.clone(),
                                reason,
                            });
                            continue;
                        }
                    }
                }

                events.push(AgentEvent::ToolStart {
                    id: tool_use.id.clone(),
                    name: tool_use.name.clone(),
                    input: actual_input.clone(),
                });
                self.cfg
                    .tool_state
                    .append_graph_node(
                        crate::graph::NodeKind::ToolCall,
                        serde_json::json!({
                            "tool_call_id": tool_use.id.clone(),
                            "tool_name": tool_use.name.clone(),
                            "tool_input": actual_input.clone(),
                        }),
                    )
                    .await?;
                prepared.push((tool_use.id.clone(), tool_use.name.clone(), actual_input));
            }
        }

        // Phase 2: Spawn first batch and transition to ExecutingTools phase.
        // Safety partitioning is preserved: each batch runs to completion before
        // the next batch is spawned. Within a batch, tools run concurrently.
        // The Phase machine in next_event() handles real-time progress delivery.
        let tools_ref = Arc::clone(&self.cfg.runtime.tools);
        let context_scope = self.cfg.runtime.context_scope.clone();
        // Use child_token() for symmetry with execution.rs and to allow
        // future per-batch cancellation without affecting the parent
        // runtime.shutdown signal.
        let cancel_token = self.cfg.runtime.shutdown.child_token();

        let mut batches = partition_tools_by_safety(&tools_ref, &prepared);

        if batches.is_empty() {
            // All tools blocked by hooks — skip ExecutingTools, go straight to finalize
            let finalize_events = self
                .finalize_tool_execution(Vec::new(), all_tool_results)
                .await?;
            let mut combined: VecDeque<AgentEvent> = events.into();
            combined.extend(finalize_events);
            if combined.is_empty() {
                self.phase = Phase::StartRequest;
            } else {
                self.phase = Phase::EmittingToolResults { events: combined };
            }
            return Ok(());
        }

        let first_batch = batches.remove(0);
        let (progress_rx, tool_handles) =
            spawn_tool_batch(&first_batch, &tools_ref, &context_scope, &cancel_token);

        self.phase = Phase::ExecutingTools(Box::new(ToolExecutionPhase {
            pre_events: events.into(),
            progress_rx,
            tool_handles,
            completed: Vec::new(),
            remaining_batches: batches,
            tools_ref,
            context_scope,
            cancel_token,
            blocked_results: all_tool_results,
        }));

        Ok(())
    }
}

/// Spawn a single batch of tools with a shared progress relay channel.
/// Returns the progress receiver and the tool join handles.
fn spawn_tool_batch(
    batch: &[(String, String, serde_json::Value)],
    tools_ref: &Arc<crate::tools::ToolRegistry>,
    context_scope: &Option<crate::SharedContextScope>,
    cancel_token: &tokio_util::sync::CancellationToken,
) -> (
    tokio::sync::mpsc::Receiver<AnnotatedProgress>,
    futures::stream::FuturesUnordered<tokio::task::JoinHandle<ToolExecResult>>,
) {
    let (shared_ptx, shared_prx) =
        tokio::sync::mpsc::channel::<AnnotatedProgress>(crate::tools::PROGRESS_CHANNEL_CAPACITY);

    let tool_handles = futures::stream::FuturesUnordered::new();

    for (id, name, input) in batch {
        let tools = Arc::clone(tools_ref);
        let scope = context_scope.clone();
        let token = cancel_token.clone();
        let relay_tx = shared_ptx.clone();
        let relay_id = id.clone();
        let relay_name = name.clone();
        let id = id.clone();
        let name = name.clone();
        let input = input.clone();

        let (tool_ptx, mut tool_prx) = tokio::sync::mpsc::channel::<crate::tools::ProgressEvent>(
            crate::tools::PROGRESS_CHANNEL_CAPACITY,
        );

        // Relay: annotate per-tool progress with (id, name)
        tokio::spawn(async move {
            while let Some(p) = tool_prx.recv().await {
                if relay_tx
                    .send((relay_id.clone(), relay_name.clone(), p))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        tool_handles.push(tokio::spawn(async move {
            let start = Instant::now();
            let result = if let Some(ref s) = scope {
                let fut =
                    tools.execute_with_progress(&name, input.clone(), Some(tool_ptx), Some(token));
                s.wrap_tool_future(Box::pin(fut)).await
            } else {
                tools
                    .execute_with_progress(&name, input.clone(), Some(tool_ptx), Some(token))
                    .await
            };
            let duration_ms = start.elapsed().as_millis() as u64;
            (id, name, input, result, duration_ms)
        }));
    }
    drop(shared_ptx); // Only relay tasks hold senders

    (shared_prx, tool_handles)
}

impl StreamState {
    /// Advance to the next safety-partitioned batch, or finalize if all batches complete.
    ///
    /// Returns `Err` if finalization fails — the caller should propagate this
    /// to the stream consumer via `return Some(Err(e))`.
    async fn advance_to_next_batch_or_finalize(
        &mut self,
        mut exec: Box<ToolExecutionPhase>,
    ) -> crate::Result<()> {
        if let Some(next_batch) = exec.remaining_batches.first() {
            let (rx, handles) = spawn_tool_batch(
                next_batch,
                &exec.tools_ref,
                &exec.context_scope,
                &exec.cancel_token,
            );
            exec.remaining_batches.remove(0);
            exec.progress_rx = rx;
            exec.tool_handles = handles;
            // pre_events may contain trailing progress from the previous batch
            self.phase = Phase::ExecutingTools(exec);
        } else {
            let mut events = self
                .finalize_tool_execution(exec.completed, exec.blocked_results)
                .await?;
            // Prepend any trailing progress from the last batch drain
            for event in exec.pre_events.into_iter().rev() {
                events.push_front(event);
            }
            self.phase = Phase::EmittingToolResults { events };
        }
        Ok(())
    }

    /// Post-process completed tools: metrics, hooks, audit, ToolComplete events.
    async fn finalize_tool_execution(
        &mut self,
        completed: Vec<ToolExecResult>,
        blocked: Vec<ContentPart>,
    ) -> crate::Result<VecDeque<AgentEvent>> {
        let mut events = VecDeque::new();
        let mut all_tool_results = blocked;

        self.all_non_retryable = !completed.is_empty()
            && completed
                .iter()
                .all(|(_, _, _, result, _)| result.is_non_retryable());

        for (id, name, input, result, duration_ms) in completed {
            let output = result.text();
            let is_error = result.is_error();

            self.metrics.record_tool(&id, &name, duration_ms, is_error);

            accumulate_inner_usage(
                &self.cfg.tool_state,
                &mut self.total_usage,
                &mut self.metrics,
                &self.cfg.runtime.budget_tracker,
                &result,
                &name,
            )
            .await?;

            try_activate_dynamic_rules(
                &name,
                &input,
                &self.cfg.runtime.orchestrator,
                &mut self.dynamic_rules,
            )
            .await;

            run_post_tool_hooks(
                &self.cfg.runtime.hooks,
                &self.cfg.hook_context,
                &self.cfg.session_id,
                &name,
                is_error,
                &result,
            )
            .await;

            emit_tool_executed(
                self.cfg.runtime.event_bus.as_deref(),
                &name,
                duration_ms,
                is_error,
            );

            self.cfg
                .tool_state
                .record_tool_execution(
                    ToolExecution::new(self.cfg.tool_state.session_id(), &name, input.clone())
                        .message(id.clone())
                        .output(result.output.text(), is_error)
                        .duration(duration_ms),
                )
                .await?;

            all_tool_results
                .push(ContentPart::from_tool_result(&id, &result).with_tool_name(&name));
            events.push_back(AgentEvent::ToolComplete {
                id,
                name,
                output,
                is_error,
                duration_ms,
            });
        }

        self.pending_tool_results = all_tool_results;
        if !self.pending_tool_results.is_empty() {
            self.finalize_tool_results().await?;
        }
        self.final_text.clear();
        self.final_thinking.clear();
        self.thinking_signature = None;
        self.finish_reason = None;

        Ok(events)
    }

    async fn finalize_tool_results(&mut self) -> crate::Result<()> {
        let results = std::mem::take(&mut self.pending_tool_results);
        let max_tokens = context_window::for_model(&self.cfg.runtime.config.model.primary);

        self.cfg
            .tool_state
            .with_session_mut(|session| session.add_tool_results(results))
            .await?;
        persist_stream_session_state(
            self.cfg.session_manager.clone(),
            self.cfg.session_scope.clone(),
            self.cfg.tool_state.clone(),
        )
        .await?;

        handle_compaction(
            &self.cfg.tool_state,
            &self.cfg.runtime,
            &self.cfg.hook_context,
            &self.cfg.session_id,
            max_tokens,
            &mut self.metrics,
        )
        .await;
        persist_stream_session_state(
            self.cfg.session_manager.clone(),
            self.cfg.session_scope.clone(),
            self.cfg.tool_state.clone(),
        )
        .await?;
        Ok(())
    }
}

/// Partition tools into batches for safe concurrent execution.
///
/// Consecutive read-only tools are grouped into a single parallel batch.
/// Mutating tools get their own sequential batch (size 1).
fn partition_tools_by_safety(
    registry: &crate::tools::ToolRegistry,
    prepared: &[(String, String, serde_json::Value)],
) -> Vec<Vec<(String, String, serde_json::Value)>> {
    let mut batches: Vec<Vec<(String, String, serde_json::Value)>> = Vec::new();
    let mut current_batch: Vec<(String, String, serde_json::Value)> = Vec::new();
    let mut current_is_read_only = true;

    for (id, name, input) in prepared {
        let tool_read_only = registry
            .get(name)
            .map(|t| t.is_read_only())
            .unwrap_or(false);

        if tool_read_only && current_is_read_only {
            // Accumulate consecutive read-only tools
            current_batch.push((id.clone(), name.clone(), input.clone()));
        } else {
            // Flush current batch if non-empty
            if !current_batch.is_empty() {
                batches.push(std::mem::take(&mut current_batch));
            }
            // Start new batch with this tool
            current_batch.push((id.clone(), name.clone(), input.clone()));
            current_is_read_only = tool_read_only;

            // If mutating tool, flush immediately (run alone)
            if !tool_read_only {
                batches.push(std::mem::take(&mut current_batch));
                current_is_read_only = true; // reset for next
            }
        }
    }

    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    batches
}

async fn persist_stream_session_state(
    manager: Option<SessionManager>,
    scope: Option<SessionAccessScope>,
    tool_state: ToolState,
) -> crate::Result<()> {
    let Some(manager) = manager else {
        return Ok(());
    };
    let session = tool_state.session().await;
    manager
        .persist_snapshot(&session, scope.as_ref())
        .await
        .map_err(crate::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_phase_transitions() {
        assert!(matches!(Phase::StartRequest, Phase::StartRequest));
        assert!(matches!(Phase::Done, Phase::Done));
    }

    #[test]
    fn test_stream_poll_result_variants() {
        let event = StreamPollResult::Event(Ok(AgentEvent::Text {
            delta: "test".into(),
        }));
        assert!(matches!(event, StreamPollResult::Event(_)));

        let cont = StreamPollResult::Continue;
        assert!(matches!(cont, StreamPollResult::Continue));

        let ended = StreamPollResult::StreamEnded;
        assert!(matches!(ended, StreamPollResult::StreamEnded));
    }
}
