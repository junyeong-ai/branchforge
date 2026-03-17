//! Agent streaming execution with session-based context management.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use futures::{Stream, StreamExt, future::join_all, stream};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use super::common::{
    BudgetContext, accumulate_inner_usage, accumulate_response_usage, emit_tokens_consumed,
    emit_tool_executed, handle_compaction, maybe_emit_budget_alert,
    maybe_invoke_explicit_skill_command, run_post_tool_hooks, run_stop_hooks,
    try_activate_dynamic_rules,
};
use super::events::{AgentEvent, AgentResult};
use super::executor::Agent;
use super::request::RequestBuilder;
use super::{AgentConfig, AgentMetrics};
use crate::authorization::ExecutionMode;
use crate::budget::{BudgetTracker, TenantBudget};
use crate::client::StreamItem;
use crate::context::PromptOrchestrator;
use crate::hooks::{HookContext, HookEvent, HookInput, HookManager};
use crate::session::ToolExecution;
use crate::session::{MessageMetadata, SessionAccessScope, SessionManager, ToolState};
use crate::types::{
    AuthorizationDenied, ContentBlock, ContentDelta, StopReason, StreamEvent, ToolResultBlock,
    ToolUseBlock, Usage, context_window,
};
use crate::{Client, ToolRegistry};

type BoxedItemStream = Pin<Box<dyn Stream<Item = crate::Result<StreamItem>> + Send>>;

impl Agent {
    pub async fn execute_stream(
        &self,
        prompt: &str,
    ) -> crate::Result<impl Stream<Item = crate::Result<AgentEvent>> + Send> {
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
        }
        let static_context = match &self.orchestrator {
            Some(orchestrator) => orchestrator.read().await.static_context().clone(),
            None => crate::context::StaticContext::new(),
        };
        let metadata = self
            .state
            .with_session(|session| {
                crate::client::messages::RequestMetadata::from_identity(
                    session.tenant_id.as_deref(),
                    session.principal_id.as_deref(),
                    Some(&session.id.to_string()),
                )
            })
            .await;

        let state = StreamState::new(
            StreamStateConfig {
                tool_state: self.state.clone(),
                client: Arc::clone(&self.client),
                config: Arc::clone(&self.config),
                tools: Arc::clone(&self.tools),
                hooks: Arc::clone(&self.hooks),
                hook_context: self.hook_context(),
                request_builder: RequestBuilder::new(
                    &self.config,
                    Arc::clone(&self.tools),
                    static_context,
                )
                .metadata(metadata),
                orchestrator: self.orchestrator.clone(),
                session_id: Arc::clone(&self.session_id),
                budget_tracker: Arc::clone(&self.budget_tracker),
                tenant_budget: self.tenant_budget.clone(),
                session_manager: self.session_manager.clone(),
                session_scope: self.session_scope.clone(),
                event_bus: self.event_bus.clone(),
                execution_mode: self.execution_mode.clone(),
            },
            timeout,
            prompt.to_string(),
        );

        Ok(stream::unfold(state, |mut state| async move {
            state.next_event().await.map(|event| (event, state))
        }))
    }
}

struct StreamStateConfig {
    tool_state: ToolState,
    client: Arc<Client>,
    config: Arc<AgentConfig>,
    tools: Arc<ToolRegistry>,
    hooks: Arc<HookManager>,
    hook_context: HookContext,
    request_builder: RequestBuilder,
    orchestrator: Option<Arc<RwLock<PromptOrchestrator>>>,
    session_id: Arc<str>,
    budget_tracker: Arc<BudgetTracker>,
    tenant_budget: Option<Arc<TenantBudget>>,
    session_manager: Option<SessionManager>,
    session_scope: Option<SessionAccessScope>,
    event_bus: Option<Arc<crate::events::EventBus>>,
    execution_mode: ExecutionMode,
}

enum StreamPollResult {
    Event(crate::Result<AgentEvent>),
    Continue,
    StreamEnded,
}

enum Phase {
    StartRequest,
    Streaming(Box<StreamingPhase>),
    StreamEnded { accumulated_usage: Usage },
    EmittingToolResults { events: VecDeque<AgentEvent> },
    Done,
}

struct StreamingPhase {
    stream: BoxedItemStream,
    accumulated_usage: Usage,
}

struct StreamState {
    cfg: StreamStateConfig,
    timeout: std::time::Duration,
    chunk_timeout: std::time::Duration,
    dynamic_rules: String,
    metrics: AgentMetrics,
    start_time: Instant,
    last_chunk_time: Instant,
    pending_tool_results: Vec<ToolResultBlock>,
    pending_tool_uses: Vec<ToolUseBlock>,
    /// Accumulator for tool_use content blocks being streamed.
    /// Bedrock (and direct API with BoxedItemStream) sends ContentBlockStart,
    /// then ContentBlockDelta(InputJsonDelta), then ContentBlockStop.
    /// We accumulate the partial JSON here and emit ToolUseComplete on stop.
    accumulating_tool_use: Option<(ToolUseBlock, String)>,
    final_text: String,
    total_usage: Usage,
    phase: Phase,
    all_non_retryable: bool,
    session_started: bool,
    prompt_submitted: bool,
    initial_prompt: Option<String>,
}

impl StreamState {
    fn new(cfg: StreamStateConfig, timeout: std::time::Duration, prompt: String) -> Self {
        let chunk_timeout = cfg.config.execution.chunk_timeout;
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
            accumulating_tool_use: None,
            final_text: String::new(),
            total_usage: Usage::default(),
            phase: Phase::StartRequest,
            all_non_retryable: false,
            session_started: false,
            prompt_submitted: false,
            initial_prompt: Some(prompt),
        }
    }

    fn extract_structured_output(&self, text: &str) -> Option<serde_json::Value> {
        super::common::extract_structured_output(
            self.cfg.config.prompt.output_schema.as_ref(),
            text,
        )
    }

    fn build_result(
        &self,
        iterations: usize,
        stop_reason: StopReason,
        messages: Vec<crate::types::Message>,
    ) -> AgentResult {
        let structured_output = self.extract_structured_output(&self.final_text);
        AgentResult::new(
            self.final_text.clone(),
            self.total_usage,
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
                            &self.cfg.hooks,
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
                            StopReason::EndTurn,
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
            tracker: &self.cfg.budget_tracker,
            tenant: self.cfg.tenant_budget.as_deref(),
            config: &self.cfg.config.budget,
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
            if let Some(ref bus) = self.cfg.event_bus {
                self.cfg.tool_state.with_event_bus(Arc::clone(bus)).await;
            }

            let session_start_input = HookInput::session_start(&*self.cfg.session_id);
            if let Err(e) = self
                .cfg
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

                self.cfg
                    .tool_state
                    .with_session_mut(|session| {
                        session.add_user_message(&prompt);
                    })
                    .await;
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
                    &self.cfg.tools,
                    &self.cfg.tool_state,
                    &self.cfg.hooks,
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

        self.metrics.iterations += 1;
        if self.metrics.iterations > self.cfg.config.execution.max_iterations {
            self.phase = Phase::Done;
            self.metrics.execution_time_ms = self.start_time.elapsed().as_millis() as u64;

            run_stop_hooks(
                &self.cfg.hooks,
                &self.cfg.hook_context,
                &self.cfg.session_id,
            )
            .await;

            let messages = self
                .cfg
                .tool_state
                .with_session(|session| session.to_api_messages())
                .await;
            let result =
                self.build_result(self.metrics.iterations - 1, StopReason::MaxTokens, messages);
            return Some(Ok(AgentEvent::Complete(Box::new(result))));
        }

        let budget_ctx = BudgetContext {
            tracker: &self.cfg.budget_tracker,
            tenant: self.cfg.tenant_budget.as_deref(),
            config: &self.cfg.config.budget,
        };
        if let Some(fallback) = budget_ctx.fallback_model() {
            self.cfg.request_builder.set_model(fallback);
        }

        let messages = self
            .cfg
            .tool_state
            .with_session(|session| {
                session.to_api_messages_with_cache(self.cfg.config.cache.conversation_ttl_option())
            })
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
                    && let Some(model_override) = updated.get("model").and_then(|v| v.as_str())
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

        let stream_request = self
            .cfg
            .request_builder
            .build(messages, &self.dynamic_rules)
            .stream();

        let response = match self
            .cfg
            .client
            .send_stream_with_auth_retry(stream_request)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                self.phase = Phase::Done;
                return Some(Err(e));
            }
        };

        self.metrics.record_api_call();

        // Select the appropriate stream parser based on the provider.
        // Providers that use a binary format (e.g. Bedrock's AWS EventStream)
        // return a custom stream from create_binary_stream; others use SSE.
        let item_stream: BoxedItemStream = if self.cfg.client.adapter().stream_format()
            != crate::client::adapter::StreamFormat::Sse
        {
            self.cfg
                .client
                .adapter()
                .create_binary_stream(Box::pin(response.bytes_stream()))
                .expect("non-SSE provider must implement create_binary_stream")
        } else {
            Box::pin(crate::client::StreamParser::new(response.bytes_stream()))
        };

        self.phase = Phase::Streaming(Box::new(StreamingPhase {
            stream: item_stream,
            accumulated_usage: Usage::default(),
        }));

        None
    }

    async fn do_poll_stream(
        &mut self,
        stream: &mut BoxedItemStream,
        accumulated_usage: &mut Usage,
    ) -> StreamPollResult {
        let chunk_result = tokio::time::timeout(self.chunk_timeout, stream.next()).await;

        match chunk_result {
            Ok(Some(Ok(item))) => {
                self.last_chunk_time = Instant::now();
                self.handle_stream_item(item, accumulated_usage)
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

    fn handle_stream_item(
        &mut self,
        item: StreamItem,
        accumulated_usage: &mut Usage,
    ) -> StreamPollResult {
        match item {
            StreamItem::Text(text) => {
                self.final_text.push_str(&text);
                self.fire_post_stream_chunk_sync(&text, "text");
                if let Some(ref bus) = self.cfg.event_bus {
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
            StreamItem::Thinking(thinking) => {
                self.fire_post_stream_chunk_sync(&thinking, "thinking");
                if let Some(ref bus) = self.cfg.event_bus {
                    bus.emit_simple(
                        crate::events::EventKind::StreamChunk,
                        serde_json::json!({
                            "chunk_type": "thinking",
                            "length": thinking.len(),
                        }),
                    );
                }
                StreamPollResult::Event(Ok(AgentEvent::Thinking { content: thinking }))
            }
            StreamItem::Citation(_) => StreamPollResult::Continue,
            StreamItem::ToolUseComplete(tool_use) => {
                self.fire_post_stream_chunk_sync(&tool_use.name, "tool_use");
                if let Some(ref bus) = self.cfg.event_bus {
                    bus.emit_simple(
                        crate::events::EventKind::StreamChunk,
                        serde_json::json!({
                            "chunk_type": "tool_use",
                            "tool_name": &tool_use.name,
                        }),
                    );
                }
                self.pending_tool_uses.push(tool_use);
                StreamPollResult::Continue
            }
            StreamItem::Event(event) => self.handle_stream_event(event, accumulated_usage),
        }
    }

    /// Fire PostStreamChunk hook in a non-blocking, fail-open manner.
    fn fire_post_stream_chunk_sync(&self, chunk_text: &str, chunk_type: &str) {
        let hooks = Arc::clone(&self.cfg.hooks);
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

    fn handle_stream_event(
        &mut self,
        event: StreamEvent,
        accumulated_usage: &mut Usage,
    ) -> StreamPollResult {
        match event {
            StreamEvent::MessageStart { message } => {
                accumulated_usage.input_tokens = message.usage.input_tokens;
                accumulated_usage.output_tokens = message.usage.output_tokens;
                accumulated_usage.cache_creation_input_tokens =
                    message.usage.cache_creation_input_tokens;
                accumulated_usage.cache_read_input_tokens = message.usage.cache_read_input_tokens;
                StreamPollResult::Continue
            }
            StreamEvent::ContentBlockStart {
                content_block: ContentBlock::ToolUse(tu),
                ..
            } => {
                // Start accumulating a tool_use content block.
                self.accumulating_tool_use = Some((tu, String::new()));
                StreamPollResult::Continue
            }
            StreamEvent::ContentBlockStart { .. } => StreamPollResult::Continue,
            StreamEvent::ContentBlockDelta {
                delta: ContentDelta::InputJsonDelta { partial_json },
                ..
            } => {
                // Accumulate partial JSON for the in-progress tool_use.
                if let Some((_, ref mut json_buf)) = self.accumulating_tool_use {
                    json_buf.push_str(&partial_json);
                }
                StreamPollResult::Continue
            }
            StreamEvent::ContentBlockDelta { .. } => StreamPollResult::Continue,
            StreamEvent::ContentBlockStop { .. } => {
                // Finalize accumulated tool_use and push to pending_tool_uses.
                if let Some((mut tool_use, json_buf)) = self.accumulating_tool_use.take() {
                    let input: serde_json::Value =
                        serde_json::from_str(&json_buf).unwrap_or(serde_json::json!({}));
                    tool_use.input = input;
                    self.pending_tool_uses.push(tool_use);
                }
                StreamPollResult::Continue
            }
            StreamEvent::MessageDelta { usage, .. } => {
                accumulated_usage.output_tokens = usage.output_tokens;
                StreamPollResult::Continue
            }
            StreamEvent::MessageStop => StreamPollResult::StreamEnded,
            StreamEvent::Ping => StreamPollResult::Continue,
            StreamEvent::Error { error } => {
                self.phase = Phase::Done;
                StreamPollResult::Event(Err(crate::Error::Stream(error.message)))
            }
        }
    }

    async fn do_handle_stream_end(
        &mut self,
        accumulated_usage: Usage,
    ) -> Option<crate::Result<AgentEvent>> {
        // Fire PostMessage hook (observation only, fail-open)
        let post_msg_input = HookInput::post_message(
            &*self.cfg.session_id,
            self.cfg.request_builder.current_model(),
            None, // stop_reason is not directly available from stream end
            accumulated_usage.input_tokens,
            accumulated_usage.output_tokens,
        );
        let _ = self
            .cfg
            .hooks
            .execute(
                HookEvent::PostMessage,
                post_msg_input,
                &self.cfg.hook_context,
            )
            .await;

        accumulate_response_usage(
            &mut self.total_usage,
            &mut self.metrics,
            &self.cfg.budget_tracker,
            self.cfg.tenant_budget.as_deref(),
            &self.cfg.config.model.primary,
            &accumulated_usage,
        );

        emit_tokens_consumed(
            self.cfg.event_bus.as_deref(),
            &accumulated_usage,
            &self.cfg.config.model.primary,
        );

        maybe_emit_budget_alert(
            &self.cfg.budget_tracker,
            self.cfg.event_bus.as_deref(),
            self.cfg.config.budget.alert_threshold_pct,
        );

        let structured_output = self.extract_structured_output(&self.final_text);

        self.cfg
            .tool_state
            .with_session_mut(|session| {
                let text_count = if self.final_text.is_empty() { 0 } else { 1 };
                let mut content = Vec::with_capacity(text_count + self.pending_tool_uses.len());
                if !self.final_text.is_empty() {
                    content.push(ContentBlock::Text {
                        text: self.final_text.clone(),
                        citations: None,
                        cache_control: None,
                    });
                }
                for tool_use in &self.pending_tool_uses {
                    content.push(ContentBlock::ToolUse(tool_use.clone()));
                }
                if !content.is_empty() {
                    session.add_assistant_message_with_metadata(
                        content,
                        Some(accumulated_usage),
                        MessageMetadata {
                            structured_output: structured_output.clone(),
                            ..Default::default()
                        },
                    );
                }
            })
            .await;
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
                &self.cfg.hooks,
                &self.cfg.hook_context,
                &self.cfg.session_id,
            )
            .await;

            let messages = self
                .cfg
                .tool_state
                .with_session(|session| session.to_api_messages())
                .await;
            let result = self.build_result(self.metrics.iterations, StopReason::EndTurn, messages);
            return Some(Ok(AgentEvent::Complete(Box::new(result))));
        }

        match self.execute_tools_parallel().await {
            Ok(events) => {
                if events.is_empty() {
                    self.phase = Phase::StartRequest;
                } else {
                    self.phase = Phase::EmittingToolResults {
                        events: events.into(),
                    };
                }
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
    async fn execute_tools_parallel(&mut self) -> crate::Result<Vec<AgentEvent>> {
        let tool_uses = std::mem::take(&mut self.pending_tool_uses);
        let mut events = Vec::new();
        let mut all_tool_results = Vec::new();

        // Phase 1: Pre-hooks (serial — hooks may depend on ordering)
        let mut prepared = Vec::new();

        for tool_use in &tool_uses {
            let pre_input = HookInput::pre_tool_use(
                &*self.cfg.session_id,
                &tool_use.name,
                tool_use.input.clone(),
            );
            let pre_output = self
                .cfg
                .hooks
                .execute(HookEvent::PreToolUse, pre_input, &self.cfg.hook_context)
                .await?;

            if !pre_output.continue_execution {
                let reason = pre_output
                    .stop_reason
                    .clone()
                    .unwrap_or_else(|| "Blocked by hook".into());
                debug!(tool = %tool_use.name, "Tool blocked by hook");

                all_tool_results.push(ToolResultBlock::error(&tool_use.id, reason.clone()));
                self.metrics.record_authorization_denial(
                    AuthorizationDenied::new(&tool_use.name, &tool_use.id, tool_use.input.clone())
                        .reason(reason.clone()),
                );
                events.push(AgentEvent::ToolBlocked {
                    id: tool_use.id.clone(),
                    name: tool_use.name.clone(),
                    reason,
                });
            } else {
                let actual_input = pre_output.updated_input.unwrap_or(tool_use.input.clone());

                // ExecutionMode: Plan mode blocks non-plan tools
                if self.cfg.execution_mode.is_plan()
                    && !self.cfg.execution_mode.allows_tool(&tool_use.name)
                {
                    let reason = format!(
                        "Tool '{}' is not available in plan mode. Only read/navigation tools are allowed.",
                        tool_use.name
                    );
                    all_tool_results.push(ToolResultBlock::error(&tool_use.id, reason.clone()));
                    events.push(AgentEvent::ToolBlocked {
                        id: tool_use.id.clone(),
                        name: tool_use.name.clone(),
                        reason,
                    });
                    continue;
                }

                // ExecutionMode: Supervised mode requires review
                if self.cfg.execution_mode.requires_review(&tool_use.name) {
                    events.push(AgentEvent::ToolReview {
                        id: tool_use.id.clone(),
                        name: tool_use.name.clone(),
                        input: actual_input.clone(),
                    });
                    all_tool_results.push(ToolResultBlock::error(
                        &tool_use.id,
                        format!(
                            "Tool '{}' requires user review. Use execute_stream() to handle ToolReview events.",
                            tool_use.name
                        ),
                    ));
                    continue;
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
                            "tool_use_id": tool_use.id.clone(),
                            "tool_name": tool_use.name.clone(),
                            "tool_input": actual_input.clone(),
                        }),
                    )
                    .await?;
                prepared.push((tool_use.id.clone(), tool_use.name.clone(), actual_input));
            }
        }

        // Phase 2: Parallel tool execution
        let tools = Arc::clone(&self.cfg.tools);
        let tool_futures = prepared.into_iter().map(|(id, name, input)| {
            let tools = Arc::clone(&tools);
            async move {
                let start = Instant::now();
                let result = tools.execute(&name, input.clone()).await;
                let duration_ms = start.elapsed().as_millis() as u64;
                (id, name, input, result, duration_ms)
            }
        });
        let parallel_results: Vec<_> = join_all(tool_futures).await;

        self.all_non_retryable = !parallel_results.is_empty()
            && parallel_results
                .iter()
                .all(|(_, _, _, result, _)| result.is_non_retryable());

        // Phase 3: Post-processing (serial — metrics, hooks, graph recording)
        for (id, name, input, result, duration_ms) in parallel_results {
            let output = result.text();
            let is_error = result.is_error();

            self.metrics.record_tool(&id, &name, duration_ms, is_error);

            accumulate_inner_usage(
                &self.cfg.tool_state,
                &mut self.total_usage,
                &mut self.metrics,
                &self.cfg.budget_tracker,
                &result,
                &name,
            )
            .await;

            try_activate_dynamic_rules(
                &name,
                &input,
                &self.cfg.orchestrator,
                &mut self.dynamic_rules,
            )
            .await;

            run_post_tool_hooks(
                &self.cfg.hooks,
                &self.cfg.hook_context,
                &self.cfg.session_id,
                &name,
                is_error,
                &result,
            )
            .await;

            emit_tool_executed(self.cfg.event_bus.as_deref(), &name, duration_ms, is_error);

            self.cfg
                .tool_state
                .record_tool_execution(
                    ToolExecution::new(self.cfg.tool_state.session_id(), &name, input.clone())
                        .message(id.clone())
                        .output(result.output.text(), is_error)
                        .duration(duration_ms),
                )
                .await?;

            all_tool_results.push(ToolResultBlock::from_tool_result(&id, &result));
            events.push(AgentEvent::ToolComplete {
                id,
                name,
                output,
                is_error,
                duration_ms,
            });
        }

        // Phase 4: Finalize — add results to session, persist, compact
        self.pending_tool_results = all_tool_results;
        if !self.pending_tool_results.is_empty() {
            self.finalize_tool_results().await?;
        }
        self.final_text.clear();

        Ok(events)
    }

    async fn finalize_tool_results(&mut self) -> crate::Result<()> {
        let results = std::mem::take(&mut self.pending_tool_results);
        let max_tokens = context_window::for_model(&self.cfg.config.model.primary);

        self.cfg
            .tool_state
            .with_session_mut(|session| {
                session.add_tool_results(results);
            })
            .await;
        persist_stream_session_state(
            self.cfg.session_manager.clone(),
            self.cfg.session_scope.clone(),
            self.cfg.tool_state.clone(),
        )
        .await?;

        handle_compaction(
            &self.cfg.tool_state,
            &self.cfg.client,
            &self.cfg.tools,
            &self.cfg.hooks,
            &self.cfg.hook_context,
            &self.cfg.session_id,
            &self.cfg.config.execution,
            max_tokens,
            &mut self.metrics,
            self.cfg.event_bus.as_deref(),
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
        .map_err(|e| crate::Error::Session(e.to_string()))
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
