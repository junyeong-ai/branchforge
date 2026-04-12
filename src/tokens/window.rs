use super::tier::{DEFAULT_CRITICAL_THRESHOLD, DEFAULT_WARNING_THRESHOLD};
use crate::models::{Capabilities, ModelSpec};

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowStatus {
    Ok { utilization: f64, remaining: u64 },
    Warning { utilization: f64, remaining: u64 },
    Critical { utilization: f64, remaining: u64 },
    Exceeded { overage: u64 },
}

impl WindowStatus {
    pub fn should_proceed(&self) -> bool {
        !matches!(self, Self::Exceeded { .. })
    }

    pub fn utilization(&self) -> Option<f64> {
        match self {
            Self::Ok { utilization, .. }
            | Self::Warning { utilization, .. }
            | Self::Critical { utilization, .. } => Some(*utilization),
            Self::Exceeded { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextWindow {
    capabilities: Capabilities,
    extended_enabled: bool,
    current_usage: u64,
    peak_usage: u64,
    warning_threshold: f64,
    critical_threshold: f64,
}

impl ContextWindow {
    pub fn new(spec: &ModelSpec, extended_enabled: bool) -> Self {
        Self {
            capabilities: spec.capabilities,
            extended_enabled,
            current_usage: 0,
            peak_usage: 0,
            warning_threshold: DEFAULT_WARNING_THRESHOLD,
            critical_threshold: DEFAULT_CRITICAL_THRESHOLD,
        }
    }

    pub fn limit(&self) -> u64 {
        self.capabilities.effective_context(self.extended_enabled)
    }

    pub fn usage(&self) -> u64 {
        self.current_usage
    }

    pub fn remaining(&self) -> u64 {
        self.limit().saturating_sub(self.current_usage)
    }

    pub fn utilization(&self) -> f64 {
        let limit = self.limit();
        if limit == 0 {
            return 0.0;
        }
        self.current_usage as f64 / limit as f64
    }

    pub fn status(&self) -> WindowStatus {
        let limit = self.limit();
        let utilization = self.utilization();

        if self.current_usage > limit {
            WindowStatus::Exceeded {
                overage: self.current_usage - limit,
            }
        } else if utilization >= self.critical_threshold {
            WindowStatus::Critical {
                utilization,
                remaining: self.remaining(),
            }
        } else if utilization >= self.warning_threshold {
            WindowStatus::Warning {
                utilization,
                remaining: self.remaining(),
            }
        } else {
            WindowStatus::Ok {
                utilization,
                remaining: self.remaining(),
            }
        }
    }

    pub fn can_fit(&self, additional: u64) -> bool {
        self.current_usage + additional <= self.limit()
    }

    pub fn update(&mut self, new_usage: u64) {
        self.current_usage = new_usage;
        if new_usage > self.peak_usage {
            self.peak_usage = new_usage;
        }
    }

    pub fn add(&mut self, tokens: u64) {
        self.update(self.current_usage.saturating_add(tokens));
    }

    pub fn reset(&mut self, new_usage: u64) {
        self.current_usage = new_usage;
    }

    pub fn peak(&self) -> u64 {
        self.peak_usage
    }

    pub fn warning_threshold(&self) -> f64 {
        self.warning_threshold
    }

    pub fn thresholds(mut self, warning: f64, critical: f64) -> Self {
        self.warning_threshold = warning;
        self.critical_threshold = critical;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::registry;

    #[test]
    fn test_context_window_status() {
        let reg = registry();
        let spec = reg.resolve("sonnet").unwrap();
        let mut window = ContextWindow::new(spec, false);

        window.update(100_000);
        assert!(matches!(window.status(), WindowStatus::Ok { .. }));

        window.update(180_000);
        assert!(matches!(window.status(), WindowStatus::Warning { .. }));

        window.update(195_000);
        assert!(matches!(window.status(), WindowStatus::Critical { .. }));

        window.update(250_000);
        assert!(matches!(window.status(), WindowStatus::Exceeded { .. }));
    }

    #[test]
    fn test_extended_context() {
        let reg = registry();
        let spec = reg.resolve("sonnet").unwrap();

        let standard = ContextWindow::new(spec, false);
        assert_eq!(standard.limit(), 200_000);

        let extended = ContextWindow::new(spec, true);
        assert_eq!(extended.limit(), 1_000_000);
    }
}
