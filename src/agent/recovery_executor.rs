//! Side-effecting executor for [`RecoveryAction`]s.
//!
//! The recipe layer ([`super::recovery_recipes`]) decides **what**
//! to do; this module decides **how** to do it. The split is
//! deliberate so recipes stay pure (testable, composable) while
//! the executor owns all the agent-loop coupling.
//!
//! # Pipeline
//!
//! ```text
//! Error → categorise → RecipeRegistry::decide → RecoveryAction → RecoveryExecutor::apply
//! ```
//!
//! [`RecoveryExecutor::apply`] is the only function the agent loop
//! calls; it interprets the action against the live `SessionHandle`
//! (for collapse/compact), `LlmCall` (for compaction), and
//! `EventBus` (for telemetry).

#![allow(missing_docs)]

use std::time::Duration;

use tracing::{info, warn};

use crate::session::SessionHandle;

use super::recovery_recipes::{RecipeRegistry, RecoveryAction, RecoveryDecisionInput};

/// Result of running [`RecoveryExecutor::apply`]. Tells the agent
/// loop whether to continue, retry, or stop.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// The action was applied successfully; the agent loop should
    /// retry the current request.
    Retry,
    /// The recipe set decided to abort. The agent loop should
    /// surface the original error to the caller.
    Abort,
}

/// Side-effecting executor that translates [`RecoveryAction`]s
/// into mutations on the agent's runtime state.
pub struct RecoveryExecutor<'a> {
    pub registry: &'a RecipeRegistry,
    pub session_handle: &'a SessionHandle,
    pub llm: Option<&'a dyn crate::client::LlmCall>,
    pub event_bus: Option<&'a crate::events::EventBus>,
}

impl RecoveryExecutor<'_> {
    /// Decide and apply a recovery action for the given error,
    /// then increment the per-iteration `attempt` counter.
    ///
    /// Returns [`RecoveryOutcome::Retry`] if the agent loop should
    /// re-issue the request, or [`RecoveryOutcome::Abort`] if the
    /// recipe set determined the error is non-recoverable.
    pub async fn apply(&self, error: &crate::Error, attempt: &mut u32) -> RecoveryOutcome {
        let category = error.category();
        // Phase D E-2: if the failing error carries a rate-limit
        // snapshot (Anthropic/OpenAI/Gemini 429 with headers), pass
        // it through to the recipe so it can return a data-driven
        // `RetryAfter { delay }` based on the provider's own
        // `seconds_until_reset` instead of exponential backoff.
        let rate_limit = error.rate_limit_snapshot().cloned();
        let decision = self.registry.decide(&RecoveryDecisionInput {
            category,
            attempt: *attempt,
            rate_limit,
        });

        info!(
            attempt = *attempt,
            category = category.as_str(),
            recipe = decision.recipe,
            action = ?decision.action,
            "Recovery executor applying action"
        );

        if let Some(bus) = self.event_bus {
            bus.emit_simple(
                crate::events::EventKind::Custom("context_recovery"),
                serde_json::json!({
                    "attempt": *attempt,
                    "category": category.as_str(),
                    "recipe": decision.recipe,
                    "action": format!("{:?}", decision.action),
                }),
            );
        }

        let outcome = match decision.action {
            RecoveryAction::Abort => RecoveryOutcome::Abort,
            RecoveryAction::Retry => RecoveryOutcome::Retry,
            RecoveryAction::RetryAfter { delay } => {
                self.sleep(delay).await;
                RecoveryOutcome::Retry
            }
            RecoveryAction::CollapseToolResultsAndRetry { max_chars } => {
                self.collapse_tool_results(max_chars).await;
                RecoveryOutcome::Retry
            }
            RecoveryAction::CompactAndRetry => match self.llm {
                Some(llm) => match self.session_handle.compact(llm).await {
                    Ok(_) => RecoveryOutcome::Retry,
                    Err(crate::Error::ContextWindowExceeded { .. }) => {
                        // Phase D B-2 PTL fallback: the compaction
                        // prompt itself overflowed the model window.
                        // Drop the oldest visible round and retry so
                        // the next recovery pass sees a smaller
                        // projection.
                        warn!(
                            "Compaction overflowed context window; draining oldest round and retrying"
                        );
                        self.drain_oldest_rounds(1).await;
                        RecoveryOutcome::Retry
                    }
                    Err(e) => {
                        warn!(error = %e, "Recovery compaction failed");
                        RecoveryOutcome::Abort
                    }
                },
                None => {
                    warn!("CompactAndRetry requested but no LlmCall available");
                    RecoveryOutcome::Abort
                }
            },
            RecoveryAction::DrainOldestRounds { rounds } => {
                let drained = self.drain_oldest_rounds(rounds).await;
                if drained == 0 {
                    warn!(
                        rounds,
                        "DrainOldestRounds requested but nothing to drain; aborting"
                    );
                    RecoveryOutcome::Abort
                } else {
                    RecoveryOutcome::Retry
                }
            }
            RecoveryAction::FallbackModel => {
                // The actual model swap is the responsibility of the
                // budget layer (see `BudgetExceedPolicy::Fallback`).
                // The executor just signals "retry with whatever
                // model the next iteration's request_builder picks".
                RecoveryOutcome::Retry
            }
        };

        *attempt += 1;
        outcome
    }

    /// Best-effort cancellable sleep. Used for `RetryAfter` actions.
    async fn sleep(&self, delay: Duration) {
        tokio::time::sleep(delay).await;
    }

    /// Phase D B-2 "prompt too long" fallback: archive the oldest
    /// `rounds` visible user→assistant turns from the current
    /// branch via [`crate::graph::SessionGraph::archive_before`].
    /// Graph events are retained so replay/branching remains
    /// correct; only the projection shrinks.
    ///
    /// Returns the number of rounds actually drained (may be less
    /// than `rounds` if the visible projection has fewer user
    /// turns — in which case the caller should surface `Abort`).
    async fn drain_oldest_rounds(&self, rounds: usize) -> usize {
        use crate::ir::Role;
        if rounds == 0 {
            return 0;
        }

        self.session_handle
            .with_session_mut(|session| {
                let messages = session.current_branch_messages();
                // Walk visible user turns in order and collect the
                // watermark candidates: the message immediately
                // AFTER each user turn is where the "round" ends.
                //
                // To drain `rounds` oldest turns, pick the (rounds+1)-th
                // user message as the archive watermark — everything
                // before it will be archived.
                let mut user_turn_ids = Vec::new();
                for msg in &messages {
                    if msg.role == Role::User
                        && let Ok(uuid) = msg.id.as_str().parse::<uuid::Uuid>()
                    {
                        user_turn_ids.push(crate::graph::NodeId::from_uuid(uuid));
                    }
                }

                if user_turn_ids.len() <= rounds {
                    // Not enough rounds to drain `rounds` without
                    // erasing the entire conversation. Return 0 and
                    // let the executor translate to Abort.
                    return 0usize;
                }

                let watermark = user_turn_ids[rounds];
                match session.archive_graph_before(watermark) {
                    Ok(archived) => {
                        info!(
                            rounds,
                            watermark = %watermark,
                            archived_nodes = archived,
                            "PTL drain archived oldest rounds"
                        );
                        rounds
                    }
                    Err(e) => {
                        warn!(error = %e, "archive_before failed during PTL drain");
                        0
                    }
                }
            })
            .await
    }

    /// Walk the current branch's user/tool messages and truncate any
    /// `ToolResultContent` payload longer than `max_chars` to that
    /// length plus a `...[truncated]` suffix. The mutation lives in
    /// `Session::content_overrides`, so the underlying graph is
    /// untouched and replay/branch operations remain correct.
    async fn collapse_tool_results(&self, max_chars: usize) {
        use crate::ir::{ContentPart, Role, ToolResultContent};

        self.session_handle
            .with_session_mut(|session| {
                let messages = session.current_branch_messages();
                for message in &messages {
                    if message.role != Role::User && message.role != Role::Tool {
                        continue;
                    }
                    let mut needs_override = false;
                    let mut new_content = message.content.clone();

                    for part in &mut new_content {
                        if let ContentPart::ToolResult { content, .. } = part {
                            match content {
                                ToolResultContent::Text(text) => {
                                    if text.len() > max_chars {
                                        text.truncate(max_chars);
                                        text.push_str("...[truncated]");
                                        needs_override = true;
                                    }
                                }
                                ToolResultContent::Json(val) => {
                                    let s = val.to_string();
                                    if s.len() > max_chars {
                                        *content = ToolResultContent::Text(format!(
                                            "{}...[truncated]",
                                            &s[..max_chars]
                                        ));
                                        needs_override = true;
                                    }
                                }
                                ToolResultContent::MultiPart(parts) => {
                                    for inner in parts.iter_mut() {
                                        if let ContentPart::Text { text } = inner
                                            && text.len() > max_chars
                                        {
                                            text.truncate(max_chars);
                                            text.push_str("...[truncated]");
                                            needs_override = true;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if needs_override
                        && let Ok(node_id) = message
                            .id
                            .as_str()
                            .parse::<uuid::Uuid>()
                            .map(crate::graph::NodeId::from_uuid)
                    {
                        session.content_overrides.set(node_id, new_content);
                    }
                }
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FailureCategory;
    use crate::agent::recovery_recipes::{
        RecipeRegistry, RecoveryAction, RecoveryRecipe, builtin_general_recipes,
    };

    #[tokio::test]
    async fn aborts_when_no_recipe_matches() {
        let registry = RecipeRegistry::new();
        let session_handle = SessionHandle::default();
        let executor = RecoveryExecutor {
            registry: &registry,
            session_handle: &session_handle,
            llm: None,
            event_bus: None,
        };
        let mut attempt = 0;
        let err = crate::Error::Authentication {
            message: "expired".into(),
        };
        let outcome = executor.apply(&err, &mut attempt).await;
        assert_eq!(outcome, RecoveryOutcome::Abort);
        assert_eq!(attempt, 1);
    }

    #[tokio::test]
    async fn auth_failure_retries_once_then_aborts() {
        let registry = RecipeRegistry::new().with_boxed_recipes(builtin_general_recipes());
        let session_handle = SessionHandle::default();
        let executor = RecoveryExecutor {
            registry: &registry,
            session_handle: &session_handle,
            llm: None,
            event_bus: None,
        };
        let err = crate::Error::Authentication {
            message: "expired".into(),
        };
        let mut attempt = 0;
        assert_eq!(
            executor.apply(&err, &mut attempt).await,
            RecoveryOutcome::Retry
        );
        assert_eq!(
            executor.apply(&err, &mut attempt).await,
            RecoveryOutcome::Abort
        );
    }

    /// Phase D B-2: `DrainOldestRounds` archives the oldest visible
    /// user turn via the graph's watermark, then returns Retry. The
    /// next iteration sees a shorter projection.
    #[tokio::test]
    async fn drain_oldest_rounds_archives_oldest_user_turn() {
        let registry = RecipeRegistry::new();
        let session_handle = SessionHandle::default();

        // Seed the session with three user→assistant rounds so a
        // single-round drain leaves two visible rounds behind.
        session_handle
            .with_session_mut(|session| {
                for i in 0..3 {
                    let _ = session.add_user_message(format!("user turn {i}"));
                    let _ = session.add_assistant_message_with_metadata(
                        vec![crate::ir::ContentPart::text(format!("assistant {i}"))],
                        Some(crate::ir::Usage::default()),
                        Default::default(),
                    );
                }
            })
            .await;

        let initial_len = session_handle
            .with_session(|s| s.current_branch_messages().len())
            .await;
        assert_eq!(initial_len, 6, "3 user + 3 assistant");

        let executor = RecoveryExecutor {
            registry: &registry,
            session_handle: &session_handle,
            llm: None,
            event_bus: None,
        };
        let drained = executor.drain_oldest_rounds(1).await;
        assert_eq!(drained, 1);

        let after_len = session_handle
            .with_session(|s| s.current_branch_messages().len())
            .await;
        assert!(
            after_len < initial_len,
            "drain must shrink projection: was {initial_len}, now {after_len}"
        );
    }

    /// Cannot drain when there are fewer visible rounds than
    /// requested — returns 0 so the executor can surface Abort.
    #[tokio::test]
    async fn drain_oldest_rounds_refuses_to_erase_everything() {
        let registry = RecipeRegistry::new();
        let session_handle = SessionHandle::default();
        session_handle
            .with_session_mut(|session| {
                let _ = session.add_user_message("only turn".to_string());
            })
            .await;

        let executor = RecoveryExecutor {
            registry: &registry,
            session_handle: &session_handle,
            llm: None,
            event_bus: None,
        };
        // 1 visible user turn, requesting to drop 1 → can't (would
        // leave 0 rounds). Helper returns 0; executor aborts.
        assert_eq!(executor.drain_oldest_rounds(1).await, 0);
    }

    #[tokio::test]
    async fn rate_limit_retry_after_uses_real_sleep() {
        // 50ms backoff so the test stays fast.
        #[derive(Debug)]
        struct FastBackoff;
        impl RecoveryRecipe for FastBackoff {
            fn name(&self) -> &'static str {
                "fast_backoff"
            }
            fn decide(
                &self,
                input: &super::super::recovery_recipes::RecoveryDecisionInput,
            ) -> super::super::recovery_recipes::RecipeDecision {
                if input.category == FailureCategory::RateLimit {
                    super::super::recovery_recipes::RecipeDecision::Act(
                        RecoveryAction::RetryAfter {
                            delay: Duration::from_millis(50),
                        },
                    )
                } else {
                    super::super::recovery_recipes::RecipeDecision::Defer
                }
            }
        }
        let registry = RecipeRegistry::new().with_recipe(FastBackoff);
        let session_handle = SessionHandle::default();
        let executor = RecoveryExecutor {
            registry: &registry,
            session_handle: &session_handle,
            llm: None,
            event_bus: None,
        };
        let err = crate::Error::RateLimit { retry_after: None };
        let mut attempt = 0;
        let start = std::time::Instant::now();
        let outcome = executor.apply(&err, &mut attempt).await;
        let elapsed = start.elapsed();
        assert_eq!(outcome, RecoveryOutcome::Retry);
        assert!(elapsed.as_millis() >= 45, "expected ~50ms sleep");
    }
}
