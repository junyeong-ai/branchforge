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
//! let decision = registry.decide(&RecoveryDecisionInput::new(
//!     FailureCategory::RateLimit,
//!     0,
//! ));
//! assert!(matches!(decision.action, RecoveryAction::RetryAfter { .. }));
//! ```

use std::time::Duration;

use crate::FailureCategory;
use crate::ir::RateLimitSnapshot;

/// Public action a recipe can recommend.
///
/// `RecoveryAction` is the **public** decision surface. The
/// internal `Defer` (recipe-doesn't-handle-this) variant is hidden
/// inside [`RecipeDecision`] so consumers of `RecoveryAction` only
/// see actionable outcomes.
#[non_exhaustive]
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
    /// Phase D B-2: "prompt too long" fallback when collapse and
    /// compaction are not enough. Archive the oldest `rounds`
    /// complete user→assistant turns from the current branch via
    /// [`crate::graph::SessionGraph::archive_before`], then retry.
    /// Graph events are preserved for replay; only the projection
    /// shrinks.
    DrainOldestRounds { rounds: usize },
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

/// Full result of a recipe-registry lookup: the action plus the
/// recipe that produced it. Callers (observability, error logs)
/// use `recipe` as the provenance label so "why did the runtime
/// retry?" has a stable answer beyond guessing from the error kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryDecision {
    pub action: RecoveryAction,
    /// Stable name of the recipe that returned the action. Set to
    /// `"default_abort"` when no recipe matched and the registry
    /// fell through to [`RecoveryAction::Abort`].
    pub recipe: &'static str,
}

impl RecoveryDecision {
    /// Helper for building a decision from a matched recipe.
    pub fn from_recipe(recipe: &'static str, action: RecoveryAction) -> Self {
        Self { action, recipe }
    }

    /// Terminal abort with no recipe match.
    pub fn default_abort() -> Self {
        Self {
            action: RecoveryAction::Abort,
            recipe: "default_abort",
        }
    }
}

impl crate::decision::DecisionReason for RecoveryDecision {
    fn category(&self) -> &'static str {
        // Category is the recipe name — cardinality is bounded by
        // the number of registered recipes, which is typically <20.
        self.recipe
    }

    fn summary(&self) -> String {
        format!("recipe={} action={:?}", self.recipe, self.action)
    }
}

/// Internal decision a single recipe returns. The registry collapses
/// `Defer` to "try the next recipe"; the public [`RecipeRegistry::decide`]
/// API never exposes `Defer` to callers.
#[non_exhaustive]
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
    /// Phase D E-2: provider-published rate-limit accounting
    /// attached to the failing response, when available. Populated
    /// from [`crate::Error::rate_limit_snapshot`] by the recovery
    /// executor. `None` when the transport does not publish
    /// rate-limit headers or the error path was not HTTP
    /// (e.g. local hook failure, budget exceeded).
    pub rate_limit: Option<RateLimitSnapshot>,
}

impl RecoveryDecisionInput {
    /// Convenience constructor for callers that only have
    /// category + attempt (tests, legacy call sites).
    pub fn new(category: FailureCategory, attempt: u32) -> Self {
        Self {
            category,
            attempt,
            rate_limit: None,
        }
    }
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
    /// non-`Defer` action as a [`RecoveryDecision`] that also
    /// records which recipe matched, or a terminal
    /// [`RecoveryDecision::default_abort`] when no recipe matched.
    pub fn decide(&self, input: &RecoveryDecisionInput) -> RecoveryDecision {
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
                return RecoveryDecision::from_recipe(recipe.name(), action);
            }
        }
        RecoveryDecision::default_abort()
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

/// Rate-limit backoff that prefers the provider's own accounting
/// when available and falls back to capped exponential backoff.
///
/// # Data-driven path (Phase D E-2)
///
/// When `input.rate_limit` is `Some(snapshot)`, the recipe reads
/// `seconds_until_reset(now())` and uses that as the retry delay,
/// clamped to `[base_delay_ms, max_delay_ms]`. This closes the
/// logical gap between Phase C-6 (RateLimitSnapshot is parsed from
/// headers and emitted on events) and the recovery loop (which
/// previously ignored the snapshot and slept on blind exponential
/// backoff instead).
///
/// The clamp floor prevents a snapshot reporting "0s until reset"
/// from triggering a tight retry loop against a provider that has
/// not yet propagated its window rollover. The ceiling is the same
/// max the exponential path uses, so a single misbehaving snapshot
/// (e.g. clock skew reporting a 12-hour window) cannot stall the
/// agent forever.
///
/// # Fallback path
///
/// When no snapshot is attached (Vertex/Bedrock/Foundry that don't
/// publish rate-limit headers, or local rate-limit errors from
/// budget guards), the classic `base * 2^attempt` exponential
/// backoff applies, capped at `max_delay_ms`.
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

impl RateLimitBackoffRecipe {
    /// Compute the retry delay in milliseconds for a given attempt,
    /// preferring the provider-published snapshot when present.
    /// Pure function — separated out so tests can inject a
    /// deterministic `now` and avoid clock flakiness.
    fn delay_ms(
        &self,
        attempt: u32,
        snapshot: Option<&RateLimitSnapshot>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> u64 {
        if let Some(snap) = snapshot
            && let Some(secs) = snap.seconds_until_reset(now)
        {
            let from_snapshot = secs.saturating_mul(1_000);
            return from_snapshot.clamp(self.base_delay_ms, self.max_delay_ms);
        }
        let exp = self.base_delay_ms.saturating_mul(1u64 << attempt.min(10));
        exp.min(self.max_delay_ms)
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
        let delay_ms = self.delay_ms(input.attempt, input.rate_limit.as_ref(), chrono::Utc::now());
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

/// Escalating recovery ladder for `ContextWindow` overflows:
///
/// - Attempt 0: collapse oversize tool results to `collapse_max_chars`
///   characters (in-place mutation via the executor) and retry.
/// - Attempt 1: trigger full session compaction and retry.
/// - Attempt 2: Phase D B-2 PTL fallback — drop the oldest
///   user→assistant round via `DrainOldestRounds { rounds: 1 }`.
/// - Attempt 3: drop two more rounds (`DrainOldestRounds { rounds: 2 }`).
/// - Attempt 4+: abort.
///
/// The PTL ladder handles the worst case where compaction itself
/// hit a context-window overflow (the compaction prompt is larger
/// than the model's window). Ported from claw-code's "prompt too
/// long" retry pattern in `services/compact/compact.ts`.
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
            2 => RecipeDecision::Act(RecoveryAction::DrainOldestRounds { rounds: 1 }),
            3 => RecipeDecision::Act(RecoveryAction::DrainOldestRounds { rounds: 2 }),
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
        RecoveryDecisionInput::new(category, attempt)
    }

    #[test]
    fn empty_registry_aborts() {
        let r = RecipeRegistry::new();
        let decision = r.decide(&input(FailureCategory::RateLimit, 0));
        assert_eq!(decision.action, RecoveryAction::Abort);
        assert_eq!(decision.recipe, "default_abort");
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
    fn context_overflow_ladder_escalates_via_collapse_compact_drain_abort() {
        let r = ContextOverflowRecipe::default();
        // Attempt 0: collapse oversize tool results.
        assert!(matches!(
            r.decide(&input(FailureCategory::ContextWindow, 0)),
            RecipeDecision::Act(RecoveryAction::CollapseToolResultsAndRetry { .. })
        ));
        // Attempt 1: full compaction.
        assert_eq!(
            r.decide(&input(FailureCategory::ContextWindow, 1)),
            RecipeDecision::Act(RecoveryAction::CompactAndRetry)
        );
        // Attempt 2: PTL drain — drop one oldest round.
        assert_eq!(
            r.decide(&input(FailureCategory::ContextWindow, 2)),
            RecipeDecision::Act(RecoveryAction::DrainOldestRounds { rounds: 1 })
        );
        // Attempt 3: drop two more rounds.
        assert_eq!(
            r.decide(&input(FailureCategory::ContextWindow, 3)),
            RecipeDecision::Act(RecoveryAction::DrainOldestRounds { rounds: 2 })
        );
        // Attempt 4+: abort.
        assert_eq!(
            r.decide(&input(FailureCategory::ContextWindow, 4)),
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
        let d = r.decide(&input(FailureCategory::RateLimit, 0));
        assert!(matches!(d.action, RecoveryAction::RetryAfter { .. }));
        assert_eq!(d.recipe, "rate_limit_backoff");

        // ContextWindow hits the collapse recipe.
        let d = r.decide(&input(FailureCategory::ContextWindow, 0));
        assert!(matches!(
            d.action,
            RecoveryAction::CollapseToolResultsAndRetry { .. }
        ));
        assert_eq!(d.recipe, "context_overflow_collapse_compact");

        // Auth hits the auth recipe.
        let d = r.decide(&input(FailureCategory::Auth, 0));
        assert_eq!(d.action, RecoveryAction::Retry);
        assert_eq!(d.recipe, "auth_retry_once");

        // BadRequest doesn't match anything → default_abort.
        let d = r.decide(&input(FailureCategory::BadRequest, 0));
        assert_eq!(d.action, RecoveryAction::Abort);
        assert_eq!(d.recipe, "default_abort");
    }

    /// User-defined recipes can be mixed with the builtin set and
    /// shadow them when registered first. The `recipe` field on the
    /// returned [`RecoveryDecision`] identifies which one matched
    /// — the main observability win from Workstream A-3.
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
        let d = r.decide(&input(FailureCategory::RateLimit, 0));
        assert_eq!(d.action, RecoveryAction::Retry);
        assert_eq!(
            d.recipe, "aggressive",
            "custom recipe must win over builtin when registered first"
        );
    }

    /// Phase D E-2: when the failing error carries a
    /// `RateLimitSnapshot` with `seconds_until_reset = 12s`, the
    /// recipe returns that exact delay (clamped into its
    /// `[base, max]` band) instead of falling through to the
    /// exponential formula. The snapshot path is what closes the
    /// Phase C-6 → Phase D recovery gap.
    #[test]
    fn rate_limit_snapshot_drives_retry_after_delay() {
        let recipe = RateLimitBackoffRecipe::default();
        let now = chrono::Utc::now();
        let snap = RateLimitSnapshot {
            requests_reset: Some(now + chrono::Duration::seconds(12)),
            ..Default::default()
        };
        // 12s * 1000 = 12000ms, inside [500, 30000] band → returned verbatim.
        assert_eq!(recipe.delay_ms(0, Some(&snap), now), 12_000);
        // Attempt index must not influence the delay when the
        // snapshot is present — the provider's own accounting wins.
        assert_eq!(recipe.delay_ms(3, Some(&snap), now), 12_000);
    }

    /// A snapshot reporting "0s until reset" still clamps to
    /// `base_delay_ms` to prevent a tight retry loop against a
    /// provider that has not yet propagated its window rollover.
    #[test]
    fn rate_limit_snapshot_zero_floor_clamps_to_base() {
        let recipe = RateLimitBackoffRecipe::default();
        let now = chrono::Utc::now();
        let snap = RateLimitSnapshot {
            tokens_reset: Some(now - chrono::Duration::seconds(1)),
            ..Default::default()
        };
        assert_eq!(
            recipe.delay_ms(0, Some(&snap), now),
            recipe.base_delay_ms,
            "zero/negative reset must clamp up to base, not sleep for 0"
        );
    }

    /// A snapshot reporting an absurd window (e.g. 12 hours from a
    /// misconfigured provider or wall-clock skew) still clamps down
    /// to `max_delay_ms` so the agent never stalls indefinitely on
    /// a single bad value.
    #[test]
    fn rate_limit_snapshot_large_value_clamps_to_max() {
        let recipe = RateLimitBackoffRecipe::default();
        let now = chrono::Utc::now();
        let snap = RateLimitSnapshot {
            requests_reset: Some(now + chrono::Duration::hours(12)),
            ..Default::default()
        };
        assert_eq!(recipe.delay_ms(0, Some(&snap), now), recipe.max_delay_ms);
    }

    /// Without a snapshot the recipe falls back to classic
    /// `base * 2^attempt` exponential backoff, matching the
    /// pre-Phase-D-E-2 behaviour. Regression guard: the fallback
    /// path must stay identical so transports that don't publish
    /// rate-limit headers (Vertex, Bedrock, Foundry) keep working.
    #[test]
    fn rate_limit_without_snapshot_falls_back_to_exponential() {
        let recipe = RateLimitBackoffRecipe::default();
        let now = chrono::Utc::now();
        assert_eq!(recipe.delay_ms(0, None, now), 500);
        assert_eq!(recipe.delay_ms(1, None, now), 1_000);
        assert_eq!(recipe.delay_ms(2, None, now), 2_000);
    }

    #[test]
    fn action_is_terminal() {
        assert!(RecoveryAction::Abort.is_terminal());
        assert!(!RecoveryAction::Retry.is_terminal());
        assert!(!RecoveryAction::CompactAndRetry.is_terminal());
    }
}
