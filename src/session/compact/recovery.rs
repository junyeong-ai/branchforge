//! Context recovery strategies for handling context overflow errors.
//!
//! When the context window is exceeded, a [`RecoveryStrategy`] can attempt
//! to reduce the context (e.g., by collapsing tool results or triggering
//! compaction) and retry the request.

use std::fmt;

use crate::session::ToolState;

/// Action to take after a recovery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Retry the request immediately (context was reduced in-place).
    Retry,
    /// Compact the session first, then retry.
    CompactAndRetry,
    /// Give up — the error is unrecoverable.
    Abort,
}

/// Classification of the overflow error.
#[derive(Debug, Clone)]
pub enum RecoveryErrorKind {
    /// The estimated token count exceeds the model's context window.
    ContextOverflow { estimated: u64, limit: u64 },
    /// The API rejected the payload as too large (HTTP 413).
    ApiPayloadTooLarge { message: String },
    /// Authentication or authorization failure (401/403, expired token).
    AuthFailure { message: String },
}

/// Contextual information passed to [`RecoveryStrategy::attempt_recovery`].
#[derive(Debug, Clone)]
pub struct RecoveryContext {
    pub error_kind: RecoveryErrorKind,
    pub attempt: u32,
    pub max_attempts: u32,
    pub current_tokens: u64,
    pub max_tokens: u64,
}

/// Trait for pluggable context recovery strategies.
#[async_trait::async_trait]
pub trait RecoveryStrategy: Send + Sync + fmt::Debug {
    /// Attempt to recover from a context overflow.
    ///
    /// Implementations may modify the session state (e.g., truncating tool
    /// results) and return a [`RecoveryAction`] indicating what to do next.
    async fn attempt_recovery(
        &self,
        ctx: &RecoveryContext,
        tool_state: &ToolState,
        llm: Option<&dyn crate::client::LlmCall>,
        compaction_chain: Option<&str>,
    ) -> crate::Result<RecoveryAction>;

    /// A human-readable name for this strategy (for logging).
    fn name(&self) -> &str;
}

/// Default recovery strategy that progressively reduces context.
///
/// - Attempt 0: Collapse all tool result content to 200 chars.
/// - Attempt 1: Trigger full compaction.
/// - Attempt 2+: Abort.
#[derive(Debug, Clone)]
pub struct ContextRecovery {
    pub max_attempts: u32,
    /// Maximum length to keep when collapsing tool result content.
    pub collapse_max_len: usize,
}

impl Default for ContextRecovery {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            collapse_max_len: 200,
        }
    }
}

#[async_trait::async_trait]
impl RecoveryStrategy for ContextRecovery {
    async fn attempt_recovery(
        &self,
        ctx: &RecoveryContext,
        tool_state: &ToolState,
        llm: Option<&dyn crate::client::LlmCall>,
        _compaction_chain: Option<&str>,
    ) -> crate::Result<RecoveryAction> {
        // Auth failures: allow one retry (token refresh / transient), then abort.
        if matches!(ctx.error_kind, RecoveryErrorKind::AuthFailure { .. }) {
            return if ctx.attempt == 0 {
                Ok(RecoveryAction::Retry)
            } else {
                Ok(RecoveryAction::Abort)
            };
        }

        if ctx.attempt >= self.max_attempts {
            return Ok(RecoveryAction::Abort);
        }

        match ctx.attempt {
            0 => {
                let max_len = self.collapse_max_len;
                tool_state
                    .with_session_mut(|s| collapse_context(s, max_len))
                    .await;
                Ok(RecoveryAction::Retry)
            }
            1 => {
                if let Some(llm) = llm {
                    let result = tool_state.compact(llm).await;
                    match result {
                        Ok(_) => Ok(RecoveryAction::CompactAndRetry),
                        Err(e) => {
                            tracing::warn!(error = %e, "Recovery compaction failed");
                            Ok(RecoveryAction::Abort)
                        }
                    }
                } else {
                    Ok(RecoveryAction::Abort)
                }
            }
            _ => Ok(RecoveryAction::Abort),
        }
    }

    fn name(&self) -> &str {
        "ContextRecovery"
    }
}

/// Aggressively truncate tool result content blocks to `max_len` characters
/// using `content_overrides` so the graph remains untouched.
fn collapse_context(session: &mut crate::session::Session, max_len: usize) {
    use crate::ir::{ContentPart, Role, ToolResultContent};

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
                        if text.len() > max_len {
                            text.truncate(max_len);
                            text.push_str("...[truncated]");
                            needs_override = true;
                        }
                    }
                    ToolResultContent::Json(val) => {
                        let s = val.to_string();
                        if s.len() > max_len {
                            *content =
                                ToolResultContent::Text(format!("{}...[truncated]", &s[..max_len]));
                            needs_override = true;
                        }
                    }
                    ToolResultContent::MultiPart(parts) => {
                        for inner in parts.iter_mut() {
                            if let ContentPart::Text { text } = inner
                                && text.len() > max_len
                            {
                                text.truncate(max_len);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recovery_action_debug() {
        assert_eq!(format!("{:?}", RecoveryAction::Retry), "Retry");
        assert_eq!(
            format!("{:?}", RecoveryAction::CompactAndRetry),
            "CompactAndRetry"
        );
        assert_eq!(format!("{:?}", RecoveryAction::Abort), "Abort");
    }

    #[test]
    fn test_context_recovery_default() {
        let recovery = ContextRecovery::default();
        assert_eq!(recovery.max_attempts, 3);
        assert_eq!(recovery.collapse_max_len, 200);
    }

    #[tokio::test]
    async fn test_context_recovery_abort_on_max() {
        let recovery = ContextRecovery::default();
        let ctx = RecoveryContext {
            error_kind: RecoveryErrorKind::ContextOverflow {
                estimated: 300_000,
                limit: 200_000,
            },
            attempt: 3,
            max_attempts: 3,
            current_tokens: 300_000,
            max_tokens: 200_000,
        };
        let tool_state = ToolState::default();
        let result = recovery
            .attempt_recovery(&ctx, &tool_state, None, None)
            .await
            .unwrap();
        assert_eq!(result, RecoveryAction::Abort);
    }

    #[tokio::test]
    async fn test_context_recovery_first_attempt_retry() {
        let recovery = ContextRecovery::default();
        let ctx = RecoveryContext {
            error_kind: RecoveryErrorKind::ContextOverflow {
                estimated: 300_000,
                limit: 200_000,
            },
            attempt: 0,
            max_attempts: 3,
            current_tokens: 300_000,
            max_tokens: 200_000,
        };
        let tool_state = ToolState::default();
        let result = recovery
            .attempt_recovery(&ctx, &tool_state, None, None)
            .await
            .unwrap();
        assert_eq!(result, RecoveryAction::Retry);
    }

    #[tokio::test]
    async fn test_auth_failure_retries_once_then_aborts() {
        let recovery = ContextRecovery::default();
        let tool_state = ToolState::default();

        // First attempt -> Retry
        let ctx = RecoveryContext {
            error_kind: RecoveryErrorKind::AuthFailure {
                message: "token expired".to_string(),
            },
            attempt: 0,
            max_attempts: 3,
            current_tokens: 0,
            max_tokens: 200_000,
        };
        let result = recovery
            .attempt_recovery(&ctx, &tool_state, None, None)
            .await
            .unwrap();
        assert_eq!(result, RecoveryAction::Retry);

        // Second attempt -> Abort
        let ctx = RecoveryContext {
            error_kind: RecoveryErrorKind::AuthFailure {
                message: "token expired".to_string(),
            },
            attempt: 1,
            max_attempts: 3,
            current_tokens: 0,
            max_tokens: 200_000,
        };
        let result = recovery
            .attempt_recovery(&ctx, &tool_state, None, None)
            .await
            .unwrap();
        assert_eq!(result, RecoveryAction::Abort);
    }
}
