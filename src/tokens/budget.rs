use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenBudget {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
}

impl TokenBudget {
    #[inline]
    pub fn context_usage(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_creation_tokens
    }

    #[inline]
    pub fn total(&self) -> u64 {
        self.context_usage() + self.output_tokens
    }

    pub fn add(&mut self, other: &TokenBudget) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(other.cache_creation_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
    }

    pub fn is_empty(&self) -> bool {
        self.context_usage() == 0 && self.output_tokens == 0
    }
}

impl From<&crate::types::Usage> for TokenBudget {
    fn from(usage: &crate::types::Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens as u64,
            cache_read_tokens: usage.cache_read_input_tokens.unwrap_or(0) as u64,
            cache_creation_tokens: usage.cache_creation_input_tokens.unwrap_or(0) as u64,
            output_tokens: usage.output_tokens as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_usage() {
        let budget = TokenBudget {
            input_tokens: 100,
            cache_read_tokens: 200_000,
            cache_creation_tokens: 0,
            output_tokens: 500,
        };

        assert_eq!(budget.context_usage(), 200_100);
        assert_eq!(budget.total(), 200_600);
    }

    #[test]
    fn test_add() {
        let mut a = TokenBudget {
            input_tokens: 100,
            cache_read_tokens: 50,
            cache_creation_tokens: 25,
            output_tokens: 200,
        };
        let b = TokenBudget {
            input_tokens: 100,
            cache_read_tokens: 50,
            cache_creation_tokens: 25,
            output_tokens: 200,
        };

        a.add(&b);
        assert_eq!(a.input_tokens, 200);
        assert_eq!(a.cache_read_tokens, 100);
    }
}
