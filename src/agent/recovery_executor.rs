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
//! calls; it interprets the action against the live `ToolState`
//! (for collapse/compact), `LlmCall` (for compaction), and
//! `EventBus` (for telemetry).

use std::time::Duration;

use tracing::{info, warn};

use crate::session::ToolState;

use super::recovery_recipes::{RecipeRegistry, RecoveryAction, RecoveryDecisionInput};

/// Result of running [`RecoveryExecutor::apply`]. Tells the agent
/// loop whether to continue, retry, or stop.
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
    pub tool_state: &'a ToolState,
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
        let action = self.registry.decide(&RecoveryDecisionInput {
            category,
            attempt: *attempt,
        });

        info!(
            attempt = *attempt,
            category = category.as_str(),
            action = ?action,
            "Recovery executor applying action"
        );

        if let Some(bus) = self.event_bus {
            bus.emit_simple(
                crate::events::EventKind::Custom("context_recovery"),
                serde_json::json!({
                    "attempt": *attempt,
                    "category": category.as_str(),
                    "action": format!("{:?}", action),
                }),
            );
        }

        let outcome = match action {
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
                Some(llm) => match self.tool_state.compact(llm).await {
                    Ok(_) => RecoveryOutcome::Retry,
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

    /// Walk the current branch's user/tool messages and truncate any
    /// `ToolResultContent` payload longer than `max_chars` to that
    /// length plus a `...[truncated]` suffix. The mutation lives in
    /// `Session::content_overrides`, so the underlying graph is
    /// untouched and replay/branch operations remain correct.
    async fn collapse_tool_results(&self, max_chars: usize) {
        use crate::ir::{ContentPart, Role, ToolResultContent};

        self.tool_state
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
        let tool_state = ToolState::default();
        let executor = RecoveryExecutor {
            registry: &registry,
            tool_state: &tool_state,
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
        let tool_state = ToolState::default();
        let executor = RecoveryExecutor {
            registry: &registry,
            tool_state: &tool_state,
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
        let tool_state = ToolState::default();
        let executor = RecoveryExecutor {
            registry: &registry,
            tool_state: &tool_state,
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
