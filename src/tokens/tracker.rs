use rust_decimal::Decimal;

use super::{ContextWindow, PricingTier, WindowStatus};
use crate::ir::Usage;
use crate::models::ModelSpec;

#[derive(Debug, Clone)]
pub enum PreflightResult {
    Ok {
        estimated_tokens: u64,
        remaining: u64,
        tier: PricingTier,
    },
    Warning {
        estimated_tokens: u64,
        utilization: f64,
        tier: PricingTier,
    },
    Exceeded {
        estimated_tokens: u64,
        limit: u64,
        overage: u64,
    },
}

impl PreflightResult {
    pub fn should_proceed(&self) -> bool {
        !matches!(self, Self::Exceeded { .. })
    }

    pub fn estimated_tokens(&self) -> u64 {
        match self {
            Self::Ok {
                estimated_tokens, ..
            }
            | Self::Warning {
                estimated_tokens, ..
            }
            | Self::Exceeded {
                estimated_tokens, ..
            } => *estimated_tokens,
        }
    }

    pub fn tier(&self) -> Option<PricingTier> {
        match self {
            Self::Ok { tier, .. } | Self::Warning { tier, .. } => Some(*tier),
            Self::Exceeded { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct TokenTracker {
    context_window: ContextWindow,
    cumulative: Usage,
    last_turn: Usage,
    model_spec: ModelSpec,
}

impl TokenTracker {
    pub fn new(model_spec: ModelSpec, extended_context: bool) -> Self {
        Self {
            context_window: ContextWindow::new(&model_spec, extended_context),
            cumulative: Usage::default(),
            last_turn: Usage::default(),
            model_spec,
        }
    }

    pub fn thresholds(mut self, warning: f64, critical: f64) -> Self {
        self.context_window = self.context_window.thresholds(warning, critical);
        self
    }

    pub fn check(&self, estimated_tokens: u64) -> PreflightResult {
        let new_usage = self.context_window.usage() + estimated_tokens;
        let limit = self.context_window.limit();

        if new_usage > limit {
            return PreflightResult::Exceeded {
                estimated_tokens,
                limit,
                overage: new_usage - limit,
            };
        }

        let utilization = if limit == 0 {
            0.0
        } else {
            new_usage as f64 / limit as f64
        };
        let tier = PricingTier::for_context(new_usage);

        if utilization >= self.context_window.warning_threshold() {
            PreflightResult::Warning {
                estimated_tokens,
                utilization,
                tier,
            }
        } else {
            PreflightResult::Ok {
                estimated_tokens,
                remaining: limit - new_usage,
                tier,
            }
        }
    }

    pub fn record(&mut self, usage: &Usage) {
        self.last_turn = usage.clone();
        self.cumulative.add(usage);
        self.context_window.update(usage.context_usage());
    }

    pub fn status(&self) -> WindowStatus {
        self.context_window.status()
    }

    pub fn context_window(&self) -> &ContextWindow {
        &self.context_window
    }

    pub fn cumulative(&self) -> &Usage {
        &self.cumulative
    }

    pub fn last_turn(&self) -> &Usage {
        &self.last_turn
    }

    pub fn pricing_tier(&self) -> PricingTier {
        PricingTier::for_context(self.context_window.usage())
    }

    pub fn total_cost(&self) -> Decimal {
        self.model_spec.pricing.calculate_raw(
            self.cumulative.input_tokens,
            self.cumulative.output_tokens,
            self.cumulative.cached_input_tokens.unwrap_or(0),
            self.cumulative.cache_creation_tokens.unwrap_or(0),
        )
    }

    pub fn reset(&mut self, new_context_usage: u64) {
        self.context_window.reset(new_context_usage);
    }

    pub fn model(&self) -> &ModelSpec {
        &self.model_spec
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::registry;

    #[test]
    fn test_preflight_ok() {
        let spec = registry().resolve("sonnet").unwrap().clone();
        let tracker = TokenTracker::new(spec, false);

        let result = tracker.check(50_000);
        assert!(result.should_proceed());
        assert!(matches!(result, PreflightResult::Ok { .. }));
    }

    #[test]
    fn test_preflight_warning() {
        let spec = registry().resolve("sonnet").unwrap().clone();
        let tracker = TokenTracker::new(spec, false);

        let result = tracker.check(180_000);
        assert!(result.should_proceed());
        assert!(matches!(result, PreflightResult::Warning { .. }));
    }

    #[test]
    fn test_preflight_exceeded() {
        let spec = registry().resolve("sonnet").unwrap().clone();
        let tracker = TokenTracker::new(spec, false);

        let result = tracker.check(250_000);
        assert!(!result.should_proceed());
        assert!(matches!(result, PreflightResult::Exceeded { .. }));
    }

    #[test]
    fn test_extended_context_not_exceeded() {
        let spec = registry().resolve("sonnet").unwrap().clone();
        let tracker = TokenTracker::new(spec, true);

        let result = tracker.check(500_000);
        assert!(result.should_proceed());
    }
}
