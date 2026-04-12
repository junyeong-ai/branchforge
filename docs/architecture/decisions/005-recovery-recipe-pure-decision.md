# ADR-005: `RecoveryRecipe` pure decision + executor

**Status:** Accepted  •  **Date:** 2026-04-10

## Context

The pre-refactor `RecoveryStrategy` type bundled three different
concerns into one trait implementation:

1. **Classifying** whether an error was recoverable.
2. **Deciding** what recovery action to take (retry, back off,
   compact context, fail).
3. **Executing** the action, which involved mutating the agent's
   `SessionHandle`, calling `llm.send` again, re-emitting hook events,
   and in some code paths spawning new async tasks.

Mixing pure decision logic with impure execution made the trait
impossible to test in isolation and impossible to extend without
editing every existing implementation. Adding a new recovery path
required understanding the full agent runtime surface. Test
coverage was sparse for exactly this reason.

## Decision

Split the two concerns:

1. **`RecoveryRecipe`** is a **pure function** from
   `(error, attempts_so_far) → RecipeDecision`. Implementations are
   trivially unit-testable. The `RecipeRegistry` is an open registry
   (same pattern as `ProfileRegistry`) that iterates recipes in
   order until one matches.

   ```rust
   pub trait RecoveryRecipe: Send + Sync {
       fn decide(&self, input: &RecoveryDecisionInput) -> Option<RecipeDecision>;
   }
   pub enum RecoveryAction { Retry, RetryAfter { delay: Duration }, Compact, Fail }
   ```

2. **`RecoveryExecutor`** is the one piece that knows how to act
   on a `RecipeDecision`: it holds the `&SessionHandle`, `&Arc<dyn LlmCall>`,
   and `&Option<EventBus>` and exposes a single `apply` method
   consumed by both the streaming and unary loops.

The canonical built-in recipes ship in
`builtin_general_recipes()` and are injected by default. Callers
who want different behaviour register their own recipes before the
builtins.

## Consequences

- **Unit-testable decisions**: every recipe is a pure function over
  small input structs. No mocked runtimes, no test scaffolding.
- **One execution path**: the executor is the single consumer of
  `RecipeDecision`. Adding observability, metrics, or rate limits
  to recovery only touches one file.
- **OCP**: new recipes are additive. The existing code never learns
  about them.
- **Slightly more ceremony**: callers now think in terms of two
  objects (registry + executor) instead of one. The win is worth
  the extra ritual.

## Alternatives rejected

- **Keep `RecoveryStrategy` as a single trait and add methods.**
  Would continue to couple decision and execution and continue to
  block unit testing.
- **Functional closures instead of trait objects.** Would prevent
  serialisation and runtime registration by id, and make the
  recipe stack opaque to observability.
- **Per-error-kind global table.** Less flexible than per-recipe
  predicates; could not express "retry only if attempt < 3" without
  smuggling state through the table.
