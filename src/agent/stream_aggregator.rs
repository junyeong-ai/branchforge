//! Stateful aggregator for [`AgentEvent`] streams.
//!
//! `Agent::execute_stream()` yields an [`AgentEvent`] stream where
//! text and thinking arrive as delta chunks, tool calls have
//! separate start/progress/complete events, and the final
//! [`AgentResult`] is the terminal `Complete` event. Rendering this
//! into a chat UI requires the caller to accumulate all of that
//! state by hand — which every SDK consumer has to re-implement,
//! invariably differently.
//!
//! `StreamAggregator` is the canonical consumer: feed it each
//! event with [`Self::apply`] (or drain a whole stream with
//! [`Self::drain`]) and read a typed snapshot via
//! [`Self::snapshot`] / [`Self::tools`] / [`Self::text`]. UIs
//! re-render after each `apply` and get a stable, immutable view.
//!
//! # Example
//!
//! ```rust,no_run
//! use branchforge::agent::StreamAggregator;
//! use futures::StreamExt;
//!
//! # async fn example(agent: &branchforge::Agent) -> branchforge::Result<()> {
//! let stream = agent.execute_stream("Hello").await?;
//! futures::pin_mut!(stream);
//! let mut agg = StreamAggregator::new();
//! while let Some(event) = stream.next().await {
//!     agg.apply(&event?);
//!     // Render on every update — aggregator owns the state.
//!     println!("{}", agg.text());
//! }
//! if let Some(result) = agg.final_result() {
//!     println!("usage: {:?}", result.usage);
//! }
//! # Ok(())
//! # }
//! ```

#![allow(missing_docs)]

use futures::{Stream, StreamExt};
use std::collections::HashMap;

use crate::ir::{FinishReason, TokenCount};
use crate::tools::ProgressStatus;

use super::events::{AgentEvent, AgentInitTool, AgentResult};

/// Point-in-time snapshot of agent capabilities taken from the
/// stream-prologue [`AgentEvent::Init`] event. Populated by
/// [`StreamAggregator`] on the first `apply` call and surfaced
/// via [`StreamAggregator::initial_state`] so UIs can render
/// capability panels before any model output arrives.
#[derive(Debug, Clone)]
pub struct InitialState {
    pub model: String,
    pub execution_mode: String,
    pub tools: Vec<AgentInitTool>,
    pub subagents: Vec<String>,
    pub skills: Vec<String>,
    pub mcp_servers: Vec<String>,
}

/// Per-tool-call state reconstructed from the stream event sequence.
/// One entry per `tool_use_id` observed during the run; iteration
/// order matches the order in which the tool calls started.
#[derive(Debug, Clone)]
pub struct ToolCallState {
    pub id: String,
    pub name: String,
    /// Arguments passed to the tool, captured from `ToolStart`.
    pub input: serde_json::Value,
    pub status: ToolCallStatus,
    /// Final output text once the tool completes. `None` until a
    /// matching `ToolComplete` event arrives.
    pub output: Option<String>,
    /// `true` if the tool completed with an error.
    pub is_error: bool,
    /// Total duration from start to completion. `None` until the
    /// tool finishes.
    pub duration_ms: Option<u64>,
    /// Reason for blocking, if the tool was blocked by a security
    /// hook. Populated from `ToolBlocked`.
    pub blocked_reason: Option<String>,
    /// Chronological list of progress events emitted between
    /// `ToolStart` and `ToolComplete`. Empty for tools that never
    /// call `ctx.progress()`.
    pub progress: Vec<ToolProgressEntry>,
}

/// Status of a single tool call, derived from the event sequence.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolCallStatus {
    /// `ToolStart` received, no completion yet.
    Running,
    /// `ToolReview` received — awaiting human approval.
    InReview,
    /// `ToolComplete` with `is_error: false`.
    Succeeded,
    /// `ToolComplete` with `is_error: true`.
    Failed,
    /// `ToolBlocked` — never executed.
    Blocked,
}

/// One sub-step progress event inside a running tool call.
#[derive(Debug, Clone)]
pub struct ToolProgressEntry {
    pub step: String,
    pub status: ProgressStatus,
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
    pub duration_ms: Option<u64>,
    pub metadata: Option<serde_json::Value>,
}

/// Running token-usage totals observed across all `TurnUsage` events
/// in the stream. These are the last values the runtime reported —
/// **not** an accumulator of deltas, because `TurnUsage` already
/// carries `total_*` fields for the session.
#[derive(Clone, Copy, Debug, Default)]
pub struct StreamUsage {
    pub input_tokens: TokenCount,
    pub output_tokens: TokenCount,
    pub cache_read_tokens: TokenCount,
    pub cache_creation_tokens: TokenCount,
    pub total_input_tokens: TokenCount,
    pub total_output_tokens: TokenCount,
}

/// Stateful aggregator that turns an `AgentEvent` stream into a
/// typed snapshot UIs can render without hand-rolling accumulators.
#[derive(Debug, Clone, Default)]
pub struct StreamAggregator {
    /// Initial capability snapshot from the first `Init` event.
    /// Populated once; subsequent `Init` events (if any — the
    /// runtime promises exactly one) replace the snapshot.
    initial_state: Option<InitialState>,
    text: String,
    thinking: String,
    /// Tool calls in insertion order.
    tool_order: Vec<String>,
    /// Fast lookup by id.
    tool_map: HashMap<String, ToolCallState>,
    usage: StreamUsage,
    final_result: Option<AgentResult>,
}

impl StreamAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one event to the aggregator, mutating internal state.
    ///
    /// Safe to call after the stream has completed — extra events
    /// are applied idempotently where possible (text/thinking
    /// append, `Complete` replaces the final result).
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Init {
                model,
                execution_mode,
                tools,
                subagents,
                skills,
                mcp_servers,
            } => {
                self.initial_state = Some(InitialState {
                    model: model.clone(),
                    execution_mode: execution_mode.clone(),
                    tools: tools.clone(),
                    subagents: subagents.clone(),
                    skills: skills.clone(),
                    mcp_servers: mcp_servers.clone(),
                });
            }
            AgentEvent::Text { delta } => {
                self.text.push_str(delta);
            }
            AgentEvent::Thinking { content } => {
                self.thinking.push_str(content);
            }
            AgentEvent::ToolStart { id, name, input } => {
                let entry = self.ensure_tool(id, name);
                entry.input = input.clone();
                entry.status = ToolCallStatus::Running;
            }
            AgentEvent::ToolReview { id, name, input } => {
                let entry = self.ensure_tool(id, name);
                entry.input = input.clone();
                entry.status = ToolCallStatus::InReview;
            }
            AgentEvent::ToolComplete {
                id,
                name,
                output,
                is_error,
                duration_ms,
            } => {
                let entry = self.ensure_tool(id, name);
                entry.status = if *is_error {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Succeeded
                };
                entry.output = Some(output.clone());
                entry.is_error = *is_error;
                entry.duration_ms = Some(*duration_ms);
            }
            AgentEvent::ToolProgress {
                id,
                name,
                step,
                status,
                timestamp,
                duration_ms,
                metadata,
            } => {
                let entry = self.ensure_tool(id, name);
                entry.progress.push(ToolProgressEntry {
                    step: step.clone(),
                    status: *status,
                    timestamp: *timestamp,
                    duration_ms: *duration_ms,
                    metadata: metadata.clone(),
                });
            }
            AgentEvent::ToolBlocked { id, name, reason } => {
                let entry = self.ensure_tool(id, name);
                entry.status = ToolCallStatus::Blocked;
                entry.blocked_reason = Some(reason.clone());
            }
            AgentEvent::TurnUsage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                total_input_tokens,
                total_output_tokens,
            } => {
                self.usage = StreamUsage {
                    input_tokens: *input_tokens,
                    output_tokens: *output_tokens,
                    cache_read_tokens: *cache_read_tokens,
                    cache_creation_tokens: *cache_creation_tokens,
                    total_input_tokens: *total_input_tokens,
                    total_output_tokens: *total_output_tokens,
                };
            }
            AgentEvent::Complete(result) => {
                self.final_result = Some((**result).clone());
            }
        }
    }

    fn new_tool(id: String, name: String) -> ToolCallState {
        ToolCallState {
            id,
            name,
            input: serde_json::Value::Null,
            status: ToolCallStatus::Running,
            output: None,
            is_error: false,
            duration_ms: None,
            blocked_reason: None,
            progress: Vec::new(),
        }
    }

    /// Get or create the per-tool-call entry for `id`, registering
    /// a fresh `tool_order` slot on first sight. Returns a mutable
    /// reference so each event branch can directly update the
    /// fields it cares about.
    ///
    /// **Complexity**: O(1) amortised — one `HashMap::contains_key`
    /// probe + one `entry` lookup + at most one `Vec::push`. The
    /// prior implementation used `Vec::contains` which made each
    /// tool event O(n) for n observed tool calls; stream-aggregated
    /// tool counts above a handful now scale linearly.
    fn ensure_tool(&mut self, id: &str, name: &str) -> &mut ToolCallState {
        let is_new = !self.tool_map.contains_key(id);
        if is_new {
            self.tool_order.push(id.to_string());
        }
        let entry = self
            .tool_map
            .entry(id.to_string())
            .or_insert_with(|| Self::new_tool(id.to_string(), name.to_string()));
        // If a later event arrives with a non-empty name and we
        // originally inserted the entry via a progress event that
        // didn't know the name, fill it in.
        if entry.name.is_empty() && !name.is_empty() {
            entry.name = name.to_string();
        }
        entry
    }

    /// Drain `stream` to completion, applying every event. Returns
    /// the aggregator alongside an optional error captured from the
    /// stream. On error, the aggregator still reflects every event
    /// that was successfully applied before the failure.
    pub async fn drain<S>(mut stream: S) -> (Self, Option<crate::Error>)
    where
        S: Stream<Item = crate::Result<AgentEvent>> + Unpin,
    {
        let mut agg = Self::new();
        let mut error = None;
        while let Some(next) = stream.next().await {
            match next {
                Ok(event) => agg.apply(&event),
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        (agg, error)
    }

    /// The capability snapshot from the stream-prologue
    /// [`AgentEvent::Init`] event. `None` until the first event
    /// is applied; after that, this is the agent's point-in-time
    /// declaration of its model, execution mode, and tool /
    /// skill / subagent / MCP catalogues at stream start.
    pub fn initial_state(&self) -> Option<&InitialState> {
        self.initial_state.as_ref()
    }

    /// Accumulated text (concatenation of every `Text` delta).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Accumulated thinking content (concatenation of every
    /// `Thinking` delta).
    pub fn thinking(&self) -> &str {
        &self.thinking
    }

    /// All tool calls observed so far, in the order they first
    /// appeared in the stream.
    pub fn tools(&self) -> Vec<&ToolCallState> {
        self.tool_order
            .iter()
            .filter_map(|id| self.tool_map.get(id))
            .collect()
    }

    /// Look up a specific tool call by id.
    pub fn tool(&self, id: &str) -> Option<&ToolCallState> {
        self.tool_map.get(id)
    }

    /// Latest usage totals as of the most recent `TurnUsage` event.
    pub fn usage(&self) -> StreamUsage {
        self.usage
    }

    /// `true` once a `Complete` event has been applied.
    pub fn is_complete(&self) -> bool {
        self.final_result.is_some()
    }

    /// Final [`AgentResult`] once the stream reaches `Complete`.
    pub fn final_result(&self) -> Option<&AgentResult> {
        self.final_result.as_ref()
    }

    /// Finish reason from the terminal [`AgentResult`], if any.
    pub fn finish_reason(&self) -> Option<&FinishReason> {
        self.final_result.as_ref().map(|r| &r.stop_reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::state::{AgentMetrics, AgentState};
    use crate::ir::{FinishReason, Usage};

    fn fake_result() -> AgentResult {
        AgentResult {
            text: "done".into(),
            usage: Usage::default(),
            tool_calls: 1,
            iterations: 1,
            stop_reason: FinishReason::Stop,
            state: AgentState::Completed,
            metrics: AgentMetrics::default(),
            session_id: "sess".into(),
            structured_output: None,
            messages: Vec::new(),
            uuid: "u".into(),
        }
    }

    #[test]
    fn text_deltas_accumulate() {
        let mut agg = StreamAggregator::new();
        agg.apply(&AgentEvent::Text {
            delta: "hello ".into(),
        });
        agg.apply(&AgentEvent::Text {
            delta: "world".into(),
        });
        assert_eq!(agg.text(), "hello world");
    }

    #[test]
    fn thinking_deltas_accumulate() {
        let mut agg = StreamAggregator::new();
        agg.apply(&AgentEvent::Thinking {
            content: "first ".into(),
        });
        agg.apply(&AgentEvent::Thinking {
            content: "second".into(),
        });
        assert_eq!(agg.thinking(), "first second");
    }

    #[test]
    fn tool_call_lifecycle() {
        let mut agg = StreamAggregator::new();
        agg.apply(&AgentEvent::ToolStart {
            id: "t1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
        });
        let running = agg.tool("t1").unwrap();
        assert_eq!(running.status, ToolCallStatus::Running);
        assert_eq!(running.input["cmd"], "ls");

        agg.apply(&AgentEvent::ToolProgress {
            id: "t1".into(),
            name: "Bash".into(),
            step: "spawn".into(),
            status: ProgressStatus::Started,
            timestamp: None,
            duration_ms: None,
            metadata: None,
        });

        agg.apply(&AgentEvent::ToolComplete {
            id: "t1".into(),
            name: "Bash".into(),
            output: "a b c".into(),
            is_error: false,
            duration_ms: 42,
        });

        let done = agg.tool("t1").unwrap();
        assert_eq!(done.status, ToolCallStatus::Succeeded);
        assert_eq!(done.output.as_deref(), Some("a b c"));
        assert_eq!(done.duration_ms, Some(42));
        assert_eq!(done.progress.len(), 1);
        assert_eq!(done.progress[0].step, "spawn");
    }

    #[test]
    fn tool_error_flag_set() {
        let mut agg = StreamAggregator::new();
        agg.apply(&AgentEvent::ToolStart {
            id: "t1".into(),
            name: "Bash".into(),
            input: serde_json::Value::Null,
        });
        agg.apply(&AgentEvent::ToolComplete {
            id: "t1".into(),
            name: "Bash".into(),
            output: "error".into(),
            is_error: true,
            duration_ms: 10,
        });
        let t = agg.tool("t1").unwrap();
        assert_eq!(t.status, ToolCallStatus::Failed);
        assert!(t.is_error);
    }

    #[test]
    fn tool_blocked_records_reason() {
        let mut agg = StreamAggregator::new();
        agg.apply(&AgentEvent::ToolBlocked {
            id: "t1".into(),
            name: "Bash".into(),
            reason: "policy deny rm:*".into(),
        });
        let t = agg.tool("t1").unwrap();
        assert_eq!(t.status, ToolCallStatus::Blocked);
        assert_eq!(t.blocked_reason.as_deref(), Some("policy deny rm:*"));
    }

    #[test]
    fn tool_review_status() {
        let mut agg = StreamAggregator::new();
        agg.apply(&AgentEvent::ToolReview {
            id: "t1".into(),
            name: "Bash".into(),
            input: serde_json::json!({"cmd": "rm /"}),
        });
        let t = agg.tool("t1").unwrap();
        assert_eq!(t.status, ToolCallStatus::InReview);
    }

    #[test]
    fn tool_order_preserved_across_many_events() {
        let mut agg = StreamAggregator::new();
        for id in ["a", "b", "c"] {
            agg.apply(&AgentEvent::ToolStart {
                id: id.into(),
                name: "Bash".into(),
                input: serde_json::Value::Null,
            });
        }
        let tools = agg.tools();
        let ids: Vec<&str> = tools.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn turn_usage_replaces_not_accumulates() {
        let mut agg = StreamAggregator::new();
        agg.apply(&AgentEvent::TurnUsage {
            input_tokens: TokenCount::new(100),
            output_tokens: TokenCount::new(50),
            cache_read_tokens: TokenCount::new(0),
            cache_creation_tokens: TokenCount::new(0),
            total_input_tokens: TokenCount::new(100),
            total_output_tokens: TokenCount::new(50),
        });
        agg.apply(&AgentEvent::TurnUsage {
            input_tokens: TokenCount::new(120),
            output_tokens: TokenCount::new(60),
            cache_read_tokens: TokenCount::new(0),
            cache_creation_tokens: TokenCount::new(0),
            total_input_tokens: TokenCount::new(220),
            total_output_tokens: TokenCount::new(110),
        });
        assert_eq!(agg.usage().input_tokens.get(), 120);
        assert_eq!(agg.usage().total_input_tokens.get(), 220);
    }

    #[test]
    fn complete_event_finalizes_result() {
        let mut agg = StreamAggregator::new();
        assert!(!agg.is_complete());
        agg.apply(&AgentEvent::Complete(Box::new(fake_result())));
        assert!(agg.is_complete());
        assert!(agg.final_result().is_some());
        assert!(matches!(agg.finish_reason(), Some(FinishReason::Stop)));
    }

    #[test]
    fn init_event_populates_initial_state() {
        let mut agg = StreamAggregator::new();
        assert!(agg.initial_state().is_none());

        agg.apply(&AgentEvent::Init {
            model: "claude-sonnet-4-5".into(),
            execution_mode: "auto".into(),
            tools: vec![super::AgentInitTool {
                name: "Read".into(),
                description: "read a file".into(),
                search_hint: Some("read file contents by path".into()),
                aliases: vec![],
            }],
            subagents: vec!["explore".into()],
            skills: vec!["commit".into()],
            mcp_servers: vec!["db".into()],
        });

        let init = agg.initial_state().expect("initial state populated");
        assert_eq!(init.model, "claude-sonnet-4-5");
        assert_eq!(init.execution_mode, "auto");
        assert_eq!(init.tools.len(), 1);
        assert_eq!(init.tools[0].name, "Read");
        assert_eq!(init.subagents, vec!["explore"]);
        assert_eq!(init.skills, vec!["commit"]);
        assert_eq!(init.mcp_servers, vec!["db"]);
    }

    #[tokio::test]
    async fn drain_accumulates_full_stream() {
        use futures::stream;

        let events: Vec<crate::Result<AgentEvent>> = vec![
            Ok(AgentEvent::Text {
                delta: "hel".into(),
            }),
            Ok(AgentEvent::Text { delta: "lo".into() }),
            Ok(AgentEvent::ToolStart {
                id: "t1".into(),
                name: "Bash".into(),
                input: serde_json::json!({"cmd": "ls"}),
            }),
            Ok(AgentEvent::ToolComplete {
                id: "t1".into(),
                name: "Bash".into(),
                output: "file".into(),
                is_error: false,
                duration_ms: 5,
            }),
            Ok(AgentEvent::Complete(Box::new(fake_result()))),
        ];

        let stream = stream::iter(events);
        let (agg, err) = StreamAggregator::drain(stream).await;
        assert!(err.is_none());
        assert_eq!(agg.text(), "hello");
        assert_eq!(agg.tools().len(), 1);
        assert!(agg.is_complete());
    }

    /// `drain` must preserve partial state when the stream errors.
    #[tokio::test]
    async fn drain_preserves_partial_state_on_error() {
        use futures::stream;

        let events: Vec<crate::Result<AgentEvent>> = vec![
            Ok(AgentEvent::Text {
                delta: "partial".into(),
            }),
            Err(crate::Error::Stream("mid-stream boom".into())),
            // These never reach the aggregator because drain stops on Err.
            Ok(AgentEvent::Text {
                delta: " rest".into(),
            }),
        ];

        let stream = stream::iter(events);
        let (agg, err) = StreamAggregator::drain(stream).await;
        assert!(err.is_some());
        assert_eq!(agg.text(), "partial");
        assert!(!agg.is_complete());
    }
}
