//! Agent state management.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    #[default]
    Initializing,
    Running,
    WaitingForToolResults,
    WaitingForUserInput,
    PlanMode,
    Completed,
    Failed,
    Cancelled,
}

impl AgentState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn is_waiting(&self) -> bool {
        matches!(
            self,
            Self::WaitingForToolResults | Self::WaitingForUserInput | Self::PlanMode
        )
    }

    pub fn can_continue(&self) -> bool {
        matches!(self, Self::Running | Self::Initializing)
    }
}

use crate::types::{AuthorizationDenied, ModelUsage, ServerToolUse, ServerToolUseUsage, Usage};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentMetrics {
    pub iterations: usize,
    pub tool_calls: usize,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    pub execution_time_ms: u64,
    pub errors: usize,
    pub compactions: usize,
    pub api_calls: usize,
    pub total_cost_usd: Decimal,
    pub tool_stats: std::collections::HashMap<String, ToolStats>,
    pub tool_call_records: Vec<ToolCallRecord>,
    pub model_usage: std::collections::HashMap<String, ModelUsage>,
    pub server_tool_use: ServerToolUse,
    pub authorization_denials: Vec<AuthorizationDenied>,
    pub api_time_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolStats {
    pub calls: usize,
    pub total_time_ms: u64,
    pub errors: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool_use_id: String,
    pub tool_name: String,
    pub duration_ms: u64,
    pub is_error: bool,
}

impl AgentMetrics {
    pub fn total_tokens(&self) -> u32 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    pub fn add_usage_with_cache(&mut self, usage: &Usage) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(usage.cache_read_input_tokens.unwrap_or(0));
        self.cache_creation_tokens = self
            .cache_creation_tokens
            .saturating_add(usage.cache_creation_input_tokens.unwrap_or(0));
    }

    /// Calculate cache hit rate as a proportion of input tokens.
    ///
    /// Returns the ratio of cache_read_tokens to input_tokens.
    /// A higher value means more tokens were served from cache.
    pub fn cache_hit_rate(&self) -> f64 {
        if self.input_tokens == 0 {
            return 0.0;
        }
        self.cache_read_tokens as f64 / self.input_tokens as f64
    }

    /// Calculate cache efficiency (reads vs total cache operations).
    ///
    /// Returns 1.0 for perfect cache reuse (all reads, no writes),
    /// and 0.0 when there's no cache activity.
    ///
    /// Per Anthropic pricing:
    /// - Cache reads cost 10% of input tokens
    /// - Cache writes cost 125% of input tokens
    ///
    /// Higher efficiency = better cost savings.
    pub fn cache_efficiency(&self) -> f64 {
        let total = self.cache_read_tokens + self.cache_creation_tokens;
        if total == 0 {
            return 0.0;
        }
        self.cache_read_tokens as f64 / total as f64
    }

    /// Estimate tokens saved through caching.
    ///
    /// Cache reads are billed at 10%, so 90% of read tokens are "saved".
    pub fn cache_tokens_saved(&self) -> u32 {
        (self.cache_read_tokens as f64 * 0.9) as u32
    }

    /// Calculate estimated cost savings from caching in USD.
    ///
    /// Per Anthropic pricing:
    /// - Normal input: full price
    /// - Cache read: 10% of normal price (90% savings)
    /// - Cache write: 125% of normal price (25% overhead)
    ///
    /// Net savings = (cache_read * 0.9 * price) - (cache_write * 0.25 * price)
    pub fn cache_cost_savings(&self, input_price_per_mtok: Decimal) -> Decimal {
        let mtok_divisor = Decimal::from(1_000_000);
        let read_tokens = Decimal::from(self.cache_read_tokens) / mtok_divisor;
        let write_tokens = Decimal::from(self.cache_creation_tokens) / mtok_divisor;

        // Savings from reading cached content (90% discount)
        let read_savings = read_tokens * input_price_per_mtok * Decimal::new(9, 1); // 0.9
        // Overhead from writing to cache (25% extra cost)
        let write_overhead = write_tokens * input_price_per_mtok * Decimal::new(25, 2); // 0.25

        read_savings - write_overhead
    }

    pub fn add_cost(&mut self, cost: Decimal) {
        self.total_cost_usd += cost;
    }

    pub fn record_tool(&mut self, tool_use_id: &str, name: &str, duration_ms: u64, is_error: bool) {
        self.tool_calls += 1;
        let stats = self.tool_stats.entry(name.to_string()).or_default();
        stats.calls += 1;
        stats.total_time_ms += duration_ms;
        if is_error {
            stats.errors += 1;
            self.errors += 1;
        }
        self.tool_call_records.push(ToolCallRecord {
            tool_use_id: tool_use_id.to_string(),
            tool_name: name.to_string(),
            duration_ms,
            is_error,
        });
    }

    pub fn record_api_call(&mut self) {
        self.api_calls += 1;
    }

    pub fn record_compaction(&mut self) {
        self.compactions += 1;
    }

    pub fn avg_tool_time_ms(&self) -> f64 {
        if self.tool_calls == 0 {
            return 0.0;
        }
        let total: u64 = self.tool_stats.values().map(|s| s.total_time_ms).sum();
        total as f64 / self.tool_calls as f64
    }

    /// Record usage for a specific model.
    ///
    /// This enables per-model cost tracking like CLI's modelUsage field.
    pub fn record_model_usage(&mut self, model: &str, usage: &Usage) {
        let entry = self.model_usage.entry(model.to_string()).or_default();
        entry.add_usage(usage, model);
    }

    /// Record an API call with timing information.
    pub fn record_api_call_with_timing(&mut self, duration_ms: u64) {
        self.api_calls += 1;
        self.api_time_ms += duration_ms;
    }

    /// Update server_tool_use from API response.
    ///
    /// This is for server-side tools executed by the API (e.g., Anthropic's
    /// server-side RAG). Not to be confused with local tool usage.
    pub fn update_server_tool_use(&mut self, server_tool_use: &ServerToolUse) {
        self.server_tool_use.web_search_requests += server_tool_use.web_search_requests;
        self.server_tool_use.web_fetch_requests += server_tool_use.web_fetch_requests;
    }

    /// Update server_tool_use from API response's usage.server_tool_use field.
    ///
    /// This parses the server tool usage directly from the API response.
    pub fn update_server_tool_use_from_api(&mut self, usage: &ServerToolUseUsage) {
        self.server_tool_use.add_from_usage(usage);
    }

    /// Record a authorization denial.
    pub fn record_authorization_denial(&mut self, denial: AuthorizationDenied) {
        self.authorization_denials.push(denial);
    }

    /// Get the total cost across all models.
    pub fn total_model_cost(&self) -> Decimal {
        self.model_usage.values().map(|m| m.cost_usd).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_state() {
        assert!(AgentState::Completed.is_terminal());
        assert!(AgentState::Failed.is_terminal());
        assert!(!AgentState::Running.is_terminal());

        assert!(AgentState::WaitingForUserInput.is_waiting());
        assert!(AgentState::PlanMode.is_waiting());
        assert!(!AgentState::Running.is_waiting());

        assert!(AgentState::Running.can_continue());
        assert!(!AgentState::Completed.can_continue());
    }

    #[test]
    fn test_agent_metrics() {
        let mut metrics = AgentMetrics::default();
        metrics.add_usage_with_cache(&Usage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        });
        metrics.add_usage_with_cache(&Usage {
            input_tokens: 200,
            output_tokens: 100,
            cache_read_input_tokens: Some(30),
            ..Default::default()
        });

        assert_eq!(metrics.input_tokens, 300);
        assert_eq!(metrics.output_tokens, 150);
        assert_eq!(metrics.total_tokens(), 450);
        assert_eq!(metrics.cache_read_tokens, 30);
    }

    #[test]
    fn test_agent_metrics_tool_recording() {
        let mut metrics = AgentMetrics::default();
        metrics.record_tool("tu_1", "Read", 50, false);
        metrics.record_tool("tu_2", "Read", 30, false);
        metrics.record_tool("tu_3", "Bash", 100, true);

        assert_eq!(metrics.tool_calls, 3);
        assert_eq!(metrics.errors, 1);
        assert_eq!(metrics.tool_stats.get("Read").unwrap().calls, 2);
        assert_eq!(metrics.tool_stats.get("Read").unwrap().total_time_ms, 80);
        assert_eq!(metrics.tool_stats.get("Bash").unwrap().errors, 1);
        assert_eq!(metrics.tool_call_records.len(), 3);
        assert_eq!(metrics.tool_call_records[0].tool_use_id, "tu_1");
        assert!(metrics.tool_call_records[2].is_error);
    }

    #[test]
    fn test_agent_metrics_avg_time() {
        let mut metrics = AgentMetrics::default();
        assert_eq!(metrics.avg_tool_time_ms(), 0.0);

        metrics.record_tool("tu_1", "Read", 100, false);
        metrics.record_tool("tu_2", "Write", 200, false);
        assert!((metrics.avg_tool_time_ms() - 150.0).abs() < 0.1);
    }

    #[test]
    fn test_cache_efficiency_no_activity() {
        let metrics = AgentMetrics::default();
        assert_eq!(metrics.cache_efficiency(), 0.0);
    }

    #[test]
    fn test_cache_efficiency_all_reads() {
        let metrics = AgentMetrics {
            cache_read_tokens: 1000,
            cache_creation_tokens: 0,
            ..Default::default()
        };

        assert!((metrics.cache_efficiency() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cache_efficiency_mixed() {
        let metrics = AgentMetrics {
            cache_read_tokens: 900,
            cache_creation_tokens: 100,
            ..Default::default()
        };

        // 900 / (900 + 100) = 0.9
        assert!((metrics.cache_efficiency() - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_cache_cost_savings() {
        use rust_decimal_macros::dec;

        let metrics = AgentMetrics {
            cache_read_tokens: 1_000_000,   // 1M tokens
            cache_creation_tokens: 100_000, // 100K tokens
            ..Default::default()
        };

        let price_per_mtok = dec!(3); // $3 per MTok

        // Read savings: 1.0 * 3.0 * 0.9 = $2.70
        // Write overhead: 0.1 * 3.0 * 0.25 = $0.075
        // Net savings: $2.70 - $0.075 = $2.625
        let savings = metrics.cache_cost_savings(price_per_mtok);
        assert_eq!(savings, dec!(2.625));
    }

    #[test]
    fn test_cache_hit_rate() {
        let metrics = AgentMetrics {
            input_tokens: 1000,
            cache_read_tokens: 800,
            ..Default::default()
        };

        // 800 / 1000 = 0.8
        assert!((metrics.cache_hit_rate() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_cache_tokens_saved() {
        let metrics = AgentMetrics {
            cache_read_tokens: 1000,
            ..Default::default()
        };

        // 1000 * 0.9 = 900
        assert_eq!(metrics.cache_tokens_saved(), 900);
    }
}
