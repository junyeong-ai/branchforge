//! Automatic model fallback for handling overload and rate limit errors.

use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct FallbackConfig {
    pub fallback_model: String,
    pub triggers: HashSet<FallbackTrigger>,
    pub max_retries: u32,
}

impl FallbackConfig {
    pub fn new(fallback_model: impl Into<String>) -> Self {
        Self {
            fallback_model: fallback_model.into(),
            triggers: Self::default_triggers(),
            max_retries: 1,
        }
    }

    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    pub fn trigger(mut self, trigger: FallbackTrigger) -> Self {
        self.triggers.insert(trigger);
        self
    }

    pub fn triggers(mut self, triggers: impl IntoIterator<Item = FallbackTrigger>) -> Self {
        self.triggers.extend(triggers);
        self
    }

    pub fn should_fallback(&self, error: &crate::Error) -> bool {
        self.triggers.iter().any(|t| t.matches(error))
    }

    fn default_triggers() -> HashSet<FallbackTrigger> {
        let mut triggers = HashSet::new();
        triggers.insert(FallbackTrigger::Overloaded);
        triggers.insert(FallbackTrigger::RateLimited);
        triggers
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackTrigger {
    Overloaded,
    RateLimited,
    HttpStatus(u16),
    Timeout,
    CircuitOpen,
}

impl FallbackTrigger {
    pub fn matches(&self, error: &crate::Error) -> bool {
        match self {
            Self::Overloaded => error.is_overloaded(),
            Self::RateLimited => matches!(error, crate::Error::RateLimit { .. }),
            Self::HttpStatus(code) => error.status_code() == Some(*code),
            Self::Timeout => matches!(error, crate::Error::Timeout(_)),
            Self::CircuitOpen => matches!(error, crate::Error::CircuitOpen),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fallback_trigger_overloaded() {
        let config = FallbackConfig::new("claude-haiku-4-5-20251001");

        let overloaded_error = crate::Error::Api {
            message: "Model is overloaded".to_string(),
            status: Some(529),
            error_type: None,
        };
        assert!(config.should_fallback(&overloaded_error));

        let auth_error = crate::Error::Api {
            message: "Invalid API key".to_string(),
            status: Some(401),
            error_type: None,
        };
        assert!(!config.should_fallback(&auth_error));
    }

    #[test]
    fn test_fallback_trigger_rate_limit() {
        let config = FallbackConfig::new("claude-haiku-4-5-20251001");

        let rate_limit_error = crate::Error::RateLimit {
            retry_after: Some(std::time::Duration::from_secs(60)),
        };
        assert!(config.should_fallback(&rate_limit_error));
    }

    #[test]
    fn test_fallback_trigger_circuit_open() {
        let config =
            FallbackConfig::new("claude-haiku-4-5-20251001").trigger(FallbackTrigger::CircuitOpen);

        let circuit_error = crate::Error::CircuitOpen;
        assert!(config.should_fallback(&circuit_error));

        // Default triggers should not match CircuitOpen
        let default_config = FallbackConfig::new("claude-haiku-4-5-20251001");
        assert!(!default_config.should_fallback(&circuit_error));
    }

    #[test]
    fn test_custom_triggers() {
        let config = FallbackConfig::new("claude-haiku-4-5-20251001")
            .trigger(FallbackTrigger::Timeout)
            .trigger(FallbackTrigger::HttpStatus(500));

        let timeout_error = crate::Error::Timeout(std::time::Duration::from_secs(30));
        assert!(config.should_fallback(&timeout_error));

        let server_error = crate::Error::Api {
            message: "Internal server error".to_string(),
            status: Some(500),
            error_type: None,
        };
        assert!(config.should_fallback(&server_error));
    }
}
