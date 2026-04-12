//! `RequestPipeline` — shared prepare/record scaffolding for unary and
//! streaming model invocations.
//!
//! Prior to this refactor, the execution loop and the streaming loop
//! both inlined the same 4-step pre-send and 4-step post-receive
//! sequence around every model call:
//!
//! ```text
//! pre-send:   budget_fallback → preflight → stash_estimate → send
//! post-recv:  accumulate_usage → reconcile_estimate → emit_tokens → budget_alert
//! ```
//!
//! `RequestPipeline` is a thin, non-stateful view over an
//! [`AgentRuntime`] that exposes these two clusters as single method
//! calls. The stream and unary paths still dispatch the actual HTTP
//! call themselves (they have different response shapes), but every
//! surrounding concern goes through the pipeline — which guarantees
//! that a new budget field, a new OTel attribute, or a new preflight
//! check lands in exactly one place.

use crate::budget::{EstimateReconciler, RequestTokenEstimate, estimate_request_tokens};
use crate::ir::{ModelRequest, Usage};

use super::common::{accumulate_response_usage, emit_tokens_consumed, maybe_emit_budget_alert};
use super::runtime::AgentRuntime;
use super::state::AgentMetrics;

/// Non-owning pipeline view. Construct one per model invocation — it
/// borrows the runtime for the duration of the call.
pub(crate) struct RequestPipeline<'a> {
    runtime: &'a AgentRuntime,
}

/// Result of [`RequestPipeline::prepare`]. Holds the token estimate
/// the caller must stash so [`RequestPipeline::record_usage`] can
/// reconcile it later. Deliberately `#[must_use]` so the caller
/// cannot drop the estimate on the floor.
#[must_use]
pub(crate) struct PreparedRequest {
    /// Pre-send token estimate. Stashed by the caller between
    /// `prepare` and the matching `record_usage` call so the
    /// reconciler can compare it against actual provider usage.
    pub estimate: RequestTokenEstimate,
}

impl<'a> RequestPipeline<'a> {
    /// Build a pipeline view over `runtime`.
    pub fn new(runtime: &'a AgentRuntime) -> Self {
        Self { runtime }
    }

    /// Apply the budget fallback model override to `request_builder` if
    /// the budget tracker has declared one. Returns `true` when a
    /// fallback was applied so the caller can log it.
    pub fn apply_budget_fallback(
        &self,
        request_builder: &mut super::request::RequestBuilder,
    ) -> bool {
        let budget_ctx = self.runtime.budget_context();
        if let Some(fallback) = budget_ctx.fallback_model() {
            request_builder.set_model(fallback);
            return true;
        }
        false
    }

    /// Pre-send preparation: preflight-check the budget and compute
    /// the token estimate that will be reconciled against actual
    /// usage later.
    ///
    /// Returns [`PreparedRequest`] on success; on budget rejection
    /// returns `Err(Error::BudgetExceeded)` per the configured
    /// [`crate::budget::BudgetExceedPolicy`].
    pub fn prepare(&self, request: &ModelRequest) -> crate::Result<PreparedRequest> {
        self.runtime.budget_context().preflight(request)?;
        Ok(PreparedRequest {
            estimate: estimate_request_tokens(request),
        })
    }

    /// Post-receive bookkeeping: accumulate usage into total/metrics,
    /// reconcile the preflight estimate against actual usage (for OTel
    /// calibration), emit the `TokensConsumed` event, and fire the
    /// `BudgetAlert` event if the threshold was crossed.
    ///
    /// Callers supply the actual [`Usage`] reported by the provider.
    /// For streaming, that is the final accumulated usage computed
    /// over the whole chunk sequence; for unary it is
    /// `response.usage`.
    pub fn record_usage(
        &self,
        total_usage: &mut Usage,
        metrics: &mut AgentMetrics,
        model: &str,
        actual_usage: &Usage,
        stashed_estimate: Option<RequestTokenEstimate>,
    ) -> crate::Result<()> {
        accumulate_response_usage(
            total_usage,
            metrics,
            &self.runtime.budget_tracker,
            self.runtime.tenant_budget.as_deref(),
            model,
            actual_usage,
        )?;

        if let Some(estimate) = stashed_estimate {
            let drift = EstimateReconciler::compute(estimate, actual_usage);
            tracing::debug!(
                target: "branchforge::budget::estimate_drift",
                model = %model,
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

        emit_tokens_consumed(self.runtime.event_bus.as_deref(), actual_usage, model);

        maybe_emit_budget_alert(
            &self.runtime.budget_tracker,
            self.runtime.event_bus.as_deref(),
            self.runtime.config.budget.alert_threshold_pct,
        );

        Ok(())
    }
}
