#![allow(missing_docs)]

use crate::ir::Usage;

/// Decision returned by [`IterationGate::should_continue`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Proceed to the next iteration.
    Continue,
    /// Stop the loop. `reason` is surfaced in logs and metrics.
    Stop { reason: String },
}

/// Snapshot of the loop state passed to [`IterationGate`] at the top of
/// each iteration. All fields are immutable borrows — the gate is a pure
/// decision function with no side effects.
pub struct IterationContext<'a> {
    pub iteration: usize,
    pub max_iterations: usize,
    pub structured_output_attempts: u32,
    pub max_structured_output_retries: u32,
    pub recovery_attempts: u32,
    pub total_usage: &'a Usage,
    pub is_shutdown_requested: bool,
}

/// Extension point for custom iteration-continue logic.
///
/// The agent loop calls `should_continue` at the top of every
/// iteration, before the budget preflight or any hook. Returning
/// [`GateDecision::Stop`] exits the loop cleanly.
///
/// The default implementation ([`DefaultIterationGate`]) reproduces the
/// current hardcoded behaviour: max-iterations check + shutdown signal.
/// SDK consumers can replace it to implement domain-specific stopping
/// conditions (e.g. "stop after tool X has been called N times").
pub trait IterationGate: Send + Sync {
    fn should_continue(&self, ctx: &IterationContext<'_>) -> GateDecision;
}

/// Default gate: checks max iterations and shutdown signal.
#[derive(Debug, Default)]
pub struct DefaultIterationGate;

impl IterationGate for DefaultIterationGate {
    fn should_continue(&self, ctx: &IterationContext<'_>) -> GateDecision {
        if ctx.is_shutdown_requested {
            return GateDecision::Stop {
                reason: "shutdown requested".into(),
            };
        }
        if ctx.iteration > ctx.max_iterations {
            return GateDecision::Stop {
                reason: format!("max iterations reached ({})", ctx.max_iterations),
            };
        }
        GateDecision::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_gate_allows_within_limit() {
        let gate = DefaultIterationGate;
        let usage = Usage::default();
        let ctx = IterationContext {
            iteration: 1,
            max_iterations: 10,
            structured_output_attempts: 0,
            max_structured_output_retries: 3,
            recovery_attempts: 0,
            total_usage: &usage,
            is_shutdown_requested: false,
        };
        assert_eq!(gate.should_continue(&ctx), GateDecision::Continue);
    }

    #[test]
    fn default_gate_stops_at_limit() {
        let gate = DefaultIterationGate;
        let usage = Usage::default();
        let ctx = IterationContext {
            iteration: 11,
            max_iterations: 10,
            structured_output_attempts: 0,
            max_structured_output_retries: 3,
            recovery_attempts: 0,
            total_usage: &usage,
            is_shutdown_requested: false,
        };
        assert!(matches!(
            gate.should_continue(&ctx),
            GateDecision::Stop { .. }
        ));
    }

    #[test]
    fn default_gate_stops_on_shutdown() {
        let gate = DefaultIterationGate;
        let usage = Usage::default();
        let ctx = IterationContext {
            iteration: 1,
            max_iterations: 10,
            structured_output_attempts: 0,
            max_structured_output_retries: 3,
            recovery_attempts: 0,
            total_usage: &usage,
            is_shutdown_requested: true,
        };
        assert!(matches!(
            gate.should_continue(&ctx),
            GateDecision::Stop { .. }
        ));
    }
}
