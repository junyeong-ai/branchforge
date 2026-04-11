//! Recovery Recipes — pluggable, named handlers that decide what to
//! do when an [`crate::Error`] occurs mid-execution.
//!
//! # Design: pure decisions, separate execution
//!
//! Recipes are **pure decision functions**: they take an
//! `(error category, attempt count)` input and return a typed
//! [`RecoveryAction`]. They never touch the session, the LLM, or
//! the network. Side effects (compacting the session, collapsing
//! tool results, sleeping for backoff) live in the agent runtime's
//! [`super::recovery_executor::RecoveryExecutor`], which interprets the action and applies
//! the corresponding mechanism.
//!
//! This split enforces SRP across the recovery surface:
//! - **Recipes** answer "what should we do?" — pure, easy to test,
//!   easy to compose, easy to reason about in isolation.
//! - **Executor** answers "how do we do it?" — wired to the
//!   `ToolState`, `LlmCall`, and `EventBus`, knows nothing about
//!   policy.
//!
//! It also makes recipe chaining cheap: a chain of `[A, B, C]` is
//! "first recipe whose decision is not `Defer` wins", with no
//! risk of partial side effects from intermediate recipes.
//!
//! # Layering: general vs application-specific
//!
//! The framework lives at Layer 1 (pure core). Built-in recipes are
//! split into two groups:
//!
//! - **General** ([`builtin_general_recipes`]): rate-limit backoff,
//!   transport retry, context-overflow → collapse-then-compact,
//!   provider-server jitter → fallback. Useful for any agent
//!   regardless of tool surface.
//! - **Application-specific** (registered by the application or by
//!   layer 2b modules): bash exit-code policies, file-not-found
//!   loops, etc. The framework does not ship these — applications
//!   compose their own.
//!
//! # Example
//!
//! ```
//! use branchforge::agent::recovery_recipes::{
//!     RecipeRegistry, builtin_general_recipes, RecoveryDecisionInput, RecoveryAction,
//! };
//! use branchforge::FailureCategory;
//!
//! let registry = RecipeRegistry::new()
//!     .with_boxed_recipes(builtin_general_recipes());
//!
//! let action = registry.decide(&RecoveryDecisionInput {
//!     category: FailureCategory::RateLimit,
//!     attempt: 0,
//! });
//! assert!(matches!(action, RecoveryAction::RetryAfter { .. }));
//! ```

use std::time::Duration;

use crate::FailureCategory;

/// Public action a recipe can recommend.
///
/// `RecoveryAction` is the **public** decision surface. The
/// internal `Defer` (recipe-doesn't-handle-this) variant is hidden
/// inside [`RecipeDecision`] so consumers of `RecoveryAction` only
/// see actionable outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Retry immediately. Use this for transient failures with no
    /// natural backoff signal (circuit-breaker half-open probes,
    /// idempotent retries).
    Retry,
    /// Sleep for `delay` then retry.
    RetryAfter { delay: Duration },
    /// Collapse oversize tool-result blocks in-place, then retry.
    /// Cheaper than full compaction; the executor walks the current
    /// branch and truncates `ToolResultContent` payloads above
    /// `max_chars`.
    CollapseToolResultsAndRetry { max_chars: usize },
    /// Run full session compaction, then retry.
    CompactAndRetry,
    /// Switch to the configured fallback model and retry. The
    /// budget tracker's `BudgetExceedPolicy::Fallback(model)`
    /// captures which model.
    FallbackModel,
    /// Give up. The error is non-recoverable.
    Abort,
}

impl RecoveryAction {
    /// `true` if the action terminates the recovery loop. Only
    /// [`Self::Abort`] is terminal — all other actions imply a
    /// retry afterwards.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Abort)
    }
}

/// Internal decision a single recipe returns. The registry collapses
/// `Defer` to "try the next recipe"; the public [`RecipeRegistry::decide`]
/// API never exposes `Defer` to callers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecipeDecision {
    /// This recipe handles the input — return this action.
    Act(RecoveryAction),
    /// This recipe does not handle the input — keep trying other
    /// recipes in the registry.
    Defer,
}

impl From<RecoveryAction> for RecipeDecision {
    fn from(a: RecoveryAction) -> Self {
        Self::Act(a)
    }
}

/// Input handed to each recipe when the registry asks it to decide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryDecisionInput {
    /// Coarse failure classification, mapped from
    /// [`crate::Error::category`].
    pub category: FailureCategory,
    /// Zero-based attempt counter for the current request. Recipes
    /// use this to escalate (retry → compact → abort).
    pub attempt: u32,
}

/// A named recovery decision function.
///
/// Recipes are **pure** — no allocation, no I/O, no mutation. The
/// registry passes the full input on every call. Recipes that need
/// configuration (max retries, base delay …) capture it in their
/// struct fields.
pub trait RecoveryRecipe: Send + Sync + std::fmt::Debug {
    /// Stable name for logs and metrics.
    fn name(&self) -> &'static str;

    /// Decide what to do, or return [`RecipeDecision::Defer`] to
    /// defer to the next recipe in the registry.
    fn decide(&self, input: &RecoveryDecisionInput) -> RecipeDecision;
}

/// Ordered collection of recipes. The registry tries them in
/// insertion order and returns the first `Act(_)` decision; if every
/// recipe defers, the registry returns [`RecoveryAction::Abort`].
#[derive(Default, Debug)]
pub struct RecipeRegistry {
    recipes: Vec<Box<dyn RecoveryRecipe>>,
}

impl RecipeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a single typed recipe.
    pub fn with_recipe<R: RecoveryRecipe + 'static>(mut self, recipe: R) -> Self {
        self.recipes.push(Box::new(recipe));
        self
    }

    /// Add multiple already-boxed recipes (e.g. from
    /// [`builtin_general_recipes`]).
    pub fn with_boxed_recipes<I>(mut self, recipes: I) -> Self
    where
        I: IntoIterator<Item = Box<dyn RecoveryRecipe>>,
    {
        for r in recipes {
            self.recipes.push(r);
        }
        self
    }

    /// Walk the recipes in priority order. Returns the first
    /// non-`Defer` action, or [`RecoveryAction::Abort`] if no
    /// recipe matched.
    pub fn decide(&self, input: &RecoveryDecisionInput) -> RecoveryAction {
        for recipe in &self.recipes {
            if let RecipeDecision::Act(action) = recipe.decide(input) {
                tracing::debug!(
                    target: "branchforge::recovery::recipe_match",
                    recipe = recipe.name(),
                    category = input.category.as_str(),
                    attempt = input.attempt,
                    action = ?action,
                    "Recovery recipe matched"
                );
                return action;
            }
        }
        RecoveryAction::Abort
    }

    /// Number of registered recipes.
    pub fn len(&self) -> usize {
        self.recipes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.recipes.is_empty()
    }
}

// ─── Built-in general recipes ──────────────────────────────────────

/// Returns the canonical general-purpose recipe set: rate-limit
/// backoff, transport retry, context-overflow → collapse-then-compact,
/// provider-server jitter → fallback. Safe to register on any
/// agent regardless of tool surface.
pub fn builtin_general_recipes() -> Vec<Box<dyn RecoveryRecipe>> {
    vec![
        Box::new(RateLimitBackoffRecipe::default()),
        Box::new(TransportRetryRecipe::default()),
        Box::new(ContextOverflowRecipe::default()),
        Box::new(ProviderServerRecipe::default()),
        Box::new(AuthFailureRecipe),
    ]
}

/// Exponential backoff for `RateLimit` errors. Caps at 30s and
/// gives up after 5 attempts.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitBackoffRecipe {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RateLimitBackoffRecipe {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay_ms: 500,
            max_delay_ms: 30_000,
        }
    }
}

impl RecoveryRecipe for RateLimitBackoffRecipe {
    fn name(&self) -> &'static str {
        "rate_limit_backoff"
    }
    fn decide(&self, input: &RecoveryDecisionInput) -> RecipeDecision {
        if input.category != FailureCategory::RateLimit {
            return RecipeDecision::Defer;
        }
        if input.attempt >= self.max_attempts {
            return RecipeDecision::Act(RecoveryAction::Abort);
        }
        let exp = self
            .base_delay_ms
            .saturating_mul(1u64 << input.attempt.min(10));
        let delay_ms = exp.min(self.max_delay_ms);
        RecipeDecision::Act(RecoveryAction::RetryAfter {
            delay: Duration::from_millis(delay_ms),
        })
    }
}

/// Three-attempt linear retry for `Transport` failures (TLS / DNS /
/// network resets) with a fixed 250ms gap.
#[derive(Debug, Clone, Copy)]
pub struct TransportRetryRecipe {
    pub max_attempts: u32,
    pub delay_ms: u64,
}

impl Default for TransportRetryRecipe {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            delay_ms: 250,
        }
    }
}

impl RecoveryRecipe for TransportRetryRecipe {
    fn name(&self) -> &'static str {
        "transport_retry"
    }
    fn decide(&self, input: &RecoveryDecisionInput) -> RecipeDecision {
        if input.category != FailureCategory::Transport {
            return RecipeDecision::Defer;
        }
        if input.attempt >= self.max_attempts {
            return RecipeDecision::Act(RecoveryAction::Abort);
        }
        RecipeDecision::Act(RecoveryAction::RetryAfter {
            delay: Duration::from_millis(self.delay_ms),
        })
    }
}

/// Two-stage recovery for `ContextWindow` overflows:
///
/// - Attempt 0: collapse oversize tool results to `collapse_max_chars`
///   characters (in-place mutation via the executor) and retry.
/// - Attempt 1: trigger full session compaction and retry.
/// - Attempt 2+: abort.
///
/// Ports the legacy `ContextRecovery::attempt_recovery` decision
/// table — the side-effecting steps now live in the
/// [`super::recovery_executor::RecoveryExecutor`] consumer.
#[derive(Debug, Clone, Copy)]
pub struct ContextOverflowRecipe {
    pub collapse_max_chars: usize,
}

impl Default for ContextOverflowRecipe {
    fn default() -> Self {
        Self {
            collapse_max_chars: 200,
        }
    }
}

impl RecoveryRecipe for ContextOverflowRecipe {
    fn name(&self) -> &'static str {
        "context_overflow_collapse_compact"
    }
    fn decide(&self, input: &RecoveryDecisionInput) -> RecipeDecision {
        if input.category != FailureCategory::ContextWindow {
            return RecipeDecision::Defer;
        }
        match input.attempt {
            0 => RecipeDecision::Act(RecoveryAction::CollapseToolResultsAndRetry {
                max_chars: self.collapse_max_chars,
            }),
            1 => RecipeDecision::Act(RecoveryAction::CompactAndRetry),
            _ => RecipeDecision::Act(RecoveryAction::Abort),
        }
    }
}

/// `ProviderServer` (5xx) — retry once with a short delay then
/// fall back to the configured fallback model.
#[derive(Debug, Clone, Copy)]
pub struct ProviderServerRecipe {
    pub delay_ms: u64,
}

impl Default for ProviderServerRecipe {
    fn default() -> Self {
        Self { delay_ms: 1_000 }
    }
}

impl RecoveryRecipe for ProviderServerRecipe {
    fn name(&self) -> &'static str {
        "provider_server_fallback"
    }
    fn decide(&self, input: &RecoveryDecisionInput) -> RecipeDecision {
        if input.category != FailureCategory::ProviderServer {
            return RecipeDecision::Defer;
        }
        match input.attempt {
            0 => RecipeDecision::Act(RecoveryAction::RetryAfter {
                delay: Duration::from_millis(self.delay_ms),
            }),
            1 => RecipeDecision::Act(RecoveryAction::FallbackModel),
            _ => RecipeDecision::Act(RecoveryAction::Abort),
        }
    }
}

/// Auth failures (`Auth` category): retry once (token refresh /
/// transient credential races), then abort. Ports the legacy
/// `ContextRecovery::attempt_recovery` auth branch.
#[derive(Debug, Clone, Copy, Default)]
pub struct AuthFailureRecipe;

impl RecoveryRecipe for AuthFailureRecipe {
    fn name(&self) -> &'static str {
        "auth_retry_once"
    }
    fn decide(&self, input: &RecoveryDecisionInput) -> RecipeDecision {
        if input.category != FailureCategory::Auth {
            return RecipeDecision::Defer;
        }
        if input.attempt == 0 {
            RecipeDecision::Act(RecoveryAction::Retry)
        } else {
            RecipeDecision::Act(RecoveryAction::Abort)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(category: FailureCategory, attempt: u32) -> RecoveryDecisionInput {
        RecoveryDecisionInput { category, attempt }
    }

    #[test]
    fn empty_registry_aborts() {
        let r = RecipeRegistry::new();
        assert_eq!(
            r.decide(&input(FailureCategory::RateLimit, 0)),
            RecoveryAction::Abort
        );
    }

    #[test]
    fn rate_limit_backoff_doubles() {
        let r = RateLimitBackoffRecipe::default();
        match (
            r.decide(&input(FailureCategory::RateLimit, 0)),
            r.decide(&input(FailureCategory::RateLimit, 1)),
            r.decide(&input(FailureCategory::RateLimit, 2)),
        ) {
            (
                RecipeDecision::Act(RecoveryAction::RetryAfter { delay: d0 }),
                RecipeDecision::Act(RecoveryAction::RetryAfter { delay: d1 }),
                RecipeDecision::Act(RecoveryAction::RetryAfter { delay: d2 }),
            ) => {
                assert_eq!(d0.as_millis(), 500);
                assert_eq!(d1.as_millis(), 1000);
                assert_eq!(d2.as_millis(), 2000);
            }
            other => panic!("expected RetryAfter triple, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_caps_at_max_delay() {
        let r = RateLimitBackoffRecipe {
            max_attempts: 20,
            base_delay_ms: 500,
            max_delay_ms: 30_000,
        };
        match r.decide(&input(FailureCategory::RateLimit, 7)) {
            RecipeDecision::Act(RecoveryAction::RetryAfter { delay }) => {
                assert_eq!(delay.as_millis(), 30_000);
            }
            other => panic!("expected RetryAfter, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_aborts_after_max_attempts() {
        let r = RateLimitBackoffRecipe::default();
        assert_eq!(
            r.decide(&input(FailureCategory::RateLimit, 5)),
            RecipeDecision::Act(RecoveryAction::Abort)
        );
    }

    #[test]
    fn rate_limit_defers_for_other_categories() {
        let r = RateLimitBackoffRecipe::default();
        assert_eq!(
            r.decide(&input(FailureCategory::Auth, 0)),
            RecipeDecision::Defer
        );
    }

    #[test]
    fn context_overflow_collapses_then_compacts_then_aborts() {
        let r = ContextOverflowRecipe::default();
        assert!(matches!(
            r.decide(&input(FailureCategory::ContextWindow, 0)),
            RecipeDecision::Act(RecoveryAction::CollapseToolResultsAndRetry { .. })
        ));
        assert_eq!(
            r.decide(&input(FailureCategory::ContextWindow, 1)),
            RecipeDecision::Act(RecoveryAction::CompactAndRetry)
        );
        assert_eq!(
            r.decide(&input(FailureCategory::ContextWindow, 2)),
            RecipeDecision::Act(RecoveryAction::Abort)
        );
    }

    #[test]
    fn provider_server_retries_then_falls_back() {
        let r = ProviderServerRecipe::default();
        assert!(matches!(
            r.decide(&input(FailureCategory::ProviderServer, 0)),
            RecipeDecision::Act(RecoveryAction::RetryAfter { .. })
        ));
        assert_eq!(
            r.decide(&input(FailureCategory::ProviderServer, 1)),
            RecipeDecision::Act(RecoveryAction::FallbackModel)
        );
        assert_eq!(
            r.decide(&input(FailureCategory::ProviderServer, 2)),
            RecipeDecision::Act(RecoveryAction::Abort)
        );
    }

    #[test]
    fn auth_retries_once_then_aborts() {
        let r = AuthFailureRecipe;
        assert_eq!(
            r.decide(&input(FailureCategory::Auth, 0)),
            RecipeDecision::Act(RecoveryAction::Retry)
        );
        assert_eq!(
            r.decide(&input(FailureCategory::Auth, 1)),
            RecipeDecision::Act(RecoveryAction::Abort)
        );
    }

    #[test]
    fn registry_walks_recipes_in_order() {
        let r = RecipeRegistry::new().with_boxed_recipes(builtin_general_recipes());
        assert_eq!(r.len(), 5);

        // RateLimit hits the rate-limit recipe.
        assert!(matches!(
            r.decide(&input(FailureCategory::RateLimit, 0)),
            RecoveryAction::RetryAfter { .. }
        ));
        // ContextWindow hits the collapse recipe.
        assert!(matches!(
            r.decide(&input(FailureCategory::ContextWindow, 0)),
            RecoveryAction::CollapseToolResultsAndRetry { .. }
        ));
        // Auth hits the auth recipe (was previously Abort with no
        // matching recipe).
        assert_eq!(
            r.decide(&input(FailureCategory::Auth, 0)),
            RecoveryAction::Retry
        );
        // BadRequest doesn't match anything → Abort.
        assert_eq!(
            r.decide(&input(FailureCategory::BadRequest, 0)),
            RecoveryAction::Abort
        );
    }

    /// User-defined recipes can be mixed with the builtin set and
    /// shadow them when registered first.
    #[test]
    fn user_recipe_can_shadow_builtin() {
        #[derive(Debug)]
        struct AggressiveRateLimit;
        impl RecoveryRecipe for AggressiveRateLimit {
            fn name(&self) -> &'static str {
                "aggressive"
            }
            fn decide(&self, input: &RecoveryDecisionInput) -> RecipeDecision {
                if input.category == FailureCategory::RateLimit {
                    RecipeDecision::Act(RecoveryAction::Retry)
                } else {
                    RecipeDecision::Defer
                }
            }
        }

        let r = RecipeRegistry::new()
            .with_recipe(AggressiveRateLimit)
            .with_boxed_recipes(builtin_general_recipes());
        assert_eq!(
            r.decide(&input(FailureCategory::RateLimit, 0)),
            RecoveryAction::Retry
        );
    }

    #[test]
    fn action_is_terminal() {
        assert!(RecoveryAction::Abort.is_terminal());
        assert!(!RecoveryAction::Retry.is_terminal());
        assert!(!RecoveryAction::CompactAndRetry.is_terminal());
    }
}
