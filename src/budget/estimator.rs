//! Pre-send token estimation for budget preflight.
//!
//! Goal: a coarse but **never-under** estimate of the input and output
//! tokens that a given [`crate::ir::ModelRequest`] would consume, so
//! that `BudgetContext::preflight` can reject a
//! request before any bytes hit the wire when it would push the
//! session or tenant over its configured limit.
//!
//! # Heuristic
//!
//! BranchForge does not ship a tokenizer in the pure core — pulling
//! in `tiktoken-rs` or equivalent would add 5+ MB of BPE tables to
//! Layer 1 for marginal benefit. Instead we use a **~4 characters per
//! token** heuristic that is known to overestimate slightly for
//! English and to track well enough for non-English text that
//! preflight rejection is never wildly wrong.
//!
//! The rule is: *overestimate on input, trust caller on output*.
//! Input overestimation means we sometimes reject a request that
//! would have squeaked under the limit — the cost of a false reject
//! is one extra iteration, whereas the cost of a false accept is a
//! billed API call that blows the budget. Output uses
//! [`crate::ir::ModelSettings::max_output_tokens`] directly when set,
//! or the `DEFAULT_MAX_OUTPUT_TOKENS` fallback.

use crate::ir::{ContentPart, Message, ModelRequest, SystemPrompt};

/// Upper bound for the output-token side of the estimate when
/// `settings.max_output_tokens` is unset. Matches the conservative
/// default most providers ship (Anthropic Messages, OpenAI Chat).
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 4096;

/// Bytes-per-token divisor. Slight overestimate for English so we err
/// toward preflight *rejection* rather than under-reporting. The
/// exact multiplier is not critical — downstream, the session will
/// still reconcile against the real `Usage` returned by the provider.
const CHARS_PER_TOKEN: u64 = 4;

/// Fixed per-message overhead in tokens (role marker + structural
/// tokens like `<|im_start|>`, `\n\n Assistant:`, etc.). Six is an
/// over-approximation that fits every mainstream provider.
const MESSAGE_OVERHEAD: u64 = 6;

/// A preflight estimate for a single request.
///
/// Produced by [`estimate_request_tokens`] and consumed by
/// `BudgetContext::preflight`. Cost estimation uses
/// [`crate::budget::PricingTable::get`] with the request's model
/// against these token counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestTokenEstimate {
    /// Upper bound on input tokens (system + messages + tool schemas).
    pub input: u64,
    /// Upper bound on output tokens (from `settings.max_output_tokens`
    /// or [`DEFAULT_MAX_OUTPUT_TOKENS`]).
    pub output: u64,
}

impl RequestTokenEstimate {
    /// Total = input + output. Used as the single number in cost
    /// computation when a pricing table doesn't care about the split.
    pub fn total(&self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

/// Produce a conservative upper-bound token estimate for a request.
///
/// Walks every [`Message`] in the request, the optional system
/// prompt, and each [`crate::ir::ToolDefinition`]'s JSON schema,
/// summing char counts and dividing by `CHARS_PER_TOKEN`. Adds a
/// fixed `MESSAGE_OVERHEAD` per message to cover role markers.
pub fn estimate_request_tokens(request: &ModelRequest) -> RequestTokenEstimate {
    let mut input_chars: u64 = 0;

    if let Some(system) = &request.system {
        input_chars = input_chars.saturating_add(system_chars(system));
    }

    for msg in &request.messages {
        input_chars = input_chars.saturating_add(message_chars(msg));
    }

    for tool in &request.tools {
        // Tool name + description + serialized JSON schema. The
        // schema is what dominates for real-world tool-heavy
        // requests, so we serialize once and count chars.
        input_chars = input_chars.saturating_add(tool.name.len() as u64);
        if let Some(desc) = &tool.description {
            input_chars = input_chars.saturating_add(desc.len() as u64);
        }
        if let Ok(s) = serde_json::to_string(&tool.parameters) {
            input_chars = input_chars.saturating_add(s.len() as u64);
        }
    }

    let message_count = request.messages.len() as u64;
    let input = input_chars / CHARS_PER_TOKEN + message_count * MESSAGE_OVERHEAD;

    let output = request
        .settings
        .max_output_tokens
        .map(u64::from)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);

    RequestTokenEstimate { input, output }
}

fn system_chars(system: &SystemPrompt) -> u64 {
    match system {
        SystemPrompt::Text(t) => t.len() as u64,
        SystemPrompt::Blocks(blocks) => blocks
            .iter()
            .map(|b| b.text.len() as u64)
            .fold(0u64, u64::saturating_add),
    }
}

fn message_chars(msg: &Message) -> u64 {
    msg.content
        .iter()
        .map(content_part_chars)
        .fold(0u64, u64::saturating_add)
}

fn content_part_chars(part: &ContentPart) -> u64 {
    use crate::ir::ReasoningContent;
    match part {
        ContentPart::Text { text } => text.len() as u64,
        ContentPart::Reasoning { content, .. } => match content {
            ReasoningContent::Visible { text } => text.len() as u64,
            ReasoningContent::Redacted { data } => data.len() as u64,
        },
        ContentPart::ToolCall {
            name, arguments, ..
        } => {
            let args_len = serde_json::to_string(arguments)
                .map(|s| s.len())
                .unwrap_or(0) as u64;
            name.len() as u64 + args_len
        }
        ContentPart::ToolResult { content, .. } => match content {
            crate::ir::ToolResultContent::Text(t) => t.len() as u64,
            crate::ir::ToolResultContent::MultiPart(parts) => parts
                .iter()
                .map(content_part_chars)
                .fold(0u64, u64::saturating_add),
            crate::ir::ToolResultContent::Json(v) => {
                serde_json::to_string(v).map(|s| s.len()).unwrap_or(0) as u64
            }
        },
        // Images and documents are approximated as a flat token
        // cost. Real providers charge by resolution/page count; a
        // fixed upper bound keeps the estimator from silently
        // ignoring them.
        ContentPart::Image { .. } | ContentPart::Document { .. } => 4000,
        // Source citations and Unknown escape-hatch parts: serialize
        // as a last-resort size proxy.
        ContentPart::Source {
            url,
            title,
            snippet,
        } => {
            let mut n = url.len();
            if let Some(t) = title {
                n += t.len();
            }
            if let Some(s) = snippet {
                n += s.len();
            }
            n as u64
        }
        ContentPart::Unknown { payload, .. } => {
            serde_json::to_string(payload).map(|s| s.len()).unwrap_or(0) as u64
        }
    }
}

/// Observed drift between the preflight estimate and the actual
/// [`crate::ir::Usage`] returned by the provider.
///
/// A ratio `>1.0` means the estimator **under-estimated** (the real
/// call used more tokens than predicted). A ratio `<1.0` means the
/// estimator **over-estimated**. The 4-chars-per-token heuristic
/// tends to over-estimate for dense English and under-estimate for
/// text-heavy languages or long tool schemas — tracking drift lets
/// operators calibrate their budget headroom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EstimateDrift {
    pub estimated_input: u64,
    pub actual_input: u64,
    pub estimated_output: u64,
    pub actual_output: u64,
    /// `actual_input / estimated_input`. `None` if the estimate was
    /// zero (would divide by zero).
    pub input_ratio: Option<f64>,
    /// `actual_output / estimated_output`. `None` if the estimate
    /// was zero.
    pub output_ratio: Option<f64>,
}

impl EstimateDrift {
    /// `true` when the actual usage was within ±25% of the estimate
    /// on both input and output axes. A rough "close enough" check
    /// for tests and dashboards.
    pub fn is_close(&self) -> bool {
        let tolerant = |r: Option<f64>| r.map(|v| (0.75..=1.25).contains(&v)).unwrap_or(true);
        tolerant(self.input_ratio) && tolerant(self.output_ratio)
    }
}

/// Single entry point for token-estimate reconciliation.
///
/// `EstimateReconciler` provides a single typed entry point for
/// estimate-vs-actual reconciliation. Tests use [`Self::compute`]
/// to get pure drift; production agent loops use [`Self::observe`]
/// to also fire a structured tracing event. Splitting reduces the
/// risk of callers accidentally running compute without the
/// side-effect or vice versa. The constructor is
/// stateless; the type exists only to namespace the operation and
/// to make the call site self-documenting.
///
/// Pipeline:
///
/// 1. Estimate the request via [`estimate_request_tokens`].
/// 2. Compute drift against `actual` usage.
/// 3. Emit a structured `tracing::debug!` event under
///    `branchforge::budget::estimate_drift` for downstream OTel
///    aggregation.
/// 4. Return the [`EstimateDrift`] struct so callers (and tests)
///    can inspect the result.
///
/// Tests use [`Self::compute`] to get the drift without firing the
/// tracing event.
pub struct EstimateReconciler;

impl EstimateReconciler {
    /// Compute drift between an estimate and the provider's reported
    /// usage. Pure: no tracing emit, no allocation beyond the
    /// returned struct.
    pub fn compute(estimate: RequestTokenEstimate, actual: &crate::ir::Usage) -> EstimateDrift {
        let ratio = |est: u64, act: u64| {
            if est == 0 {
                None
            } else {
                Some(act as f64 / est as f64)
            }
        };
        EstimateDrift {
            estimated_input: estimate.input,
            actual_input: actual.input_tokens,
            estimated_output: estimate.output,
            actual_output: actual.output_tokens,
            input_ratio: ratio(estimate.input, actual.input_tokens),
            output_ratio: ratio(estimate.output, actual.output_tokens),
        }
    }

    /// Observe a request → response pair: estimate the request,
    /// compute drift, emit a structured tracing event, and return
    /// the drift. **This is the canonical agent-loop callsite.**
    pub fn observe(request: &ModelRequest, actual: &crate::ir::Usage) -> EstimateDrift {
        let estimate = estimate_request_tokens(request);
        let drift = Self::compute(estimate, actual);
        tracing::debug!(
            target: "branchforge::budget::estimate_drift",
            model = %request.model,
            estimated_input = drift.estimated_input,
            actual_input = drift.actual_input,
            estimated_output = drift.estimated_output,
            actual_output = drift.actual_output,
            input_ratio = drift.input_ratio.unwrap_or(f64::NAN),
            output_ratio = drift.output_ratio.unwrap_or(f64::NAN),
            is_close = drift.is_close(),
            "Token estimate drift recorded"
        );
        drift
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Message, ModelRequest, ModelSettings, SystemPrompt};

    #[test]
    fn estimates_scale_with_message_length() {
        let mut req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("hi")]);
        let small = estimate_request_tokens(&req);

        req.messages.push(Message::user("x".repeat(4000).as_str()));
        let big = estimate_request_tokens(&req);

        assert!(
            big.input > small.input + 800,
            "expected ~1000 more tokens, got {} vs {}",
            big.input,
            small.input
        );
    }

    #[test]
    fn system_prompt_counted() {
        let req_no_sys = ModelRequest::new("m", vec![Message::user("hello")]);
        let without = estimate_request_tokens(&req_no_sys);

        let mut req_sys = req_no_sys.clone();
        req_sys.system = Some(SystemPrompt::Text("x".repeat(400)));
        let with = estimate_request_tokens(&req_sys);

        assert!(with.input > without.input);
    }

    #[test]
    fn max_output_tokens_used_verbatim() {
        let mut req = ModelRequest::new("m", vec![Message::user("hi")]);
        req.settings = ModelSettings::default().with_max_output_tokens(256);
        let est = estimate_request_tokens(&req);
        assert_eq!(est.output, 256);
    }

    #[test]
    fn default_output_fallback() {
        let req = ModelRequest::new("m", vec![Message::user("hi")]);
        let est = estimate_request_tokens(&req);
        assert_eq!(est.output, DEFAULT_MAX_OUTPUT_TOKENS);
    }

    #[test]
    fn drift_detects_underestimate() {
        use crate::ir::Usage;
        let req = ModelRequest::new("m", vec![Message::user("hi")]);
        let estimate = estimate_request_tokens(&req);
        let actual = Usage {
            input_tokens: estimate.input * 3,
            output_tokens: 100,
            ..Default::default()
        };
        let drift = EstimateReconciler::compute(estimate, &actual);
        assert!(drift.input_ratio.unwrap() > 2.5);
        assert!(!drift.is_close());
    }

    #[test]
    fn drift_accepts_close_match() {
        use crate::ir::Usage;
        let req = ModelRequest::new("m", vec![Message::user("hi")]);
        let estimate = estimate_request_tokens(&req);
        // actual = estimate exactly
        let actual = Usage {
            input_tokens: estimate.input,
            output_tokens: estimate.output,
            ..Default::default()
        };
        let drift = EstimateReconciler::compute(estimate, &actual);
        assert!(drift.is_close());
        assert_eq!(drift.input_ratio, Some(1.0));
    }

    #[test]
    fn drift_handles_zero_estimate() {
        use crate::ir::Usage;
        let estimate = RequestTokenEstimate {
            input: 0,
            output: 0,
        };
        let actual = Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };
        let drift = EstimateReconciler::compute(estimate, &actual);
        assert_eq!(drift.input_ratio, None);
        assert_eq!(drift.output_ratio, None);
        // Zero-estimate drift is vacuously "close" — nothing to compare against.
        assert!(drift.is_close());
    }

    #[test]
    fn observe_emits_and_returns() {
        use crate::ir::Usage;
        let req = ModelRequest::new("claude-sonnet-4-5", vec![Message::user("hello")]);
        let actual = Usage {
            input_tokens: 50,
            output_tokens: 30,
            ..Default::default()
        };
        let drift = EstimateReconciler::observe(&req, &actual);
        assert_eq!(drift.actual_input, 50);
        assert_eq!(drift.actual_output, 30);
    }

    #[test]
    fn per_message_overhead_present() {
        let req = ModelRequest::new(
            "m",
            vec![Message::user(""), Message::user(""), Message::user("")],
        );
        let est = estimate_request_tokens(&req);
        // 3 empty messages × MESSAGE_OVERHEAD = 18 tokens minimum
        assert!(est.input >= 3 * MESSAGE_OVERHEAD);
    }
}
