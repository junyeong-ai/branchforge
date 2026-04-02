use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostSummary {
    pub total_cost_usd: Decimal,
    pub per_model: Vec<ModelCostEntry>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCostEntry {
    pub model: String,
    pub cost_usd: Decimal,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl CostSummary {
    pub fn format_report(&self) -> String {
        let mut lines = vec![format!(
            "Total: ${:.4} ({:.1}s)",
            self.total_cost_usd,
            self.duration_ms as f64 / 1000.0
        )];
        if !self.per_model.is_empty() {
            lines.push(format!(
                "{:<20} {:>10} {:>10} {:>10}",
                "Model", "Input", "Output", "Cost"
            ));
            let mut sorted = self.per_model.clone();
            sorted.sort_by(|a, b| b.cost_usd.cmp(&a.cost_usd));
            for entry in &sorted {
                lines.push(format!(
                    "{:<20} {:>10} {:>10} ${:>9.4}",
                    entry.model, entry.input_tokens, entry.output_tokens, entry.cost_usd
                ));
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_format_report_empty_models() {
        let summary = CostSummary {
            total_cost_usd: dec!(0.1234),
            per_model: vec![],
            total_input_tokens: 1000,
            total_output_tokens: 500,
            cache_read_tokens: 200,
            cache_creation_tokens: 100,
            duration_ms: 5500,
        };
        let report = summary.format_report();
        assert!(report.contains("Total: $0.1234 (5.5s)"));
        assert!(!report.contains("Model"));
    }

    #[test]
    fn test_format_report_with_models() {
        let summary = CostSummary {
            total_cost_usd: dec!(0.5),
            per_model: vec![
                ModelCostEntry {
                    model: "claude-haiku".to_string(),
                    cost_usd: dec!(0.1),
                    input_tokens: 500,
                    output_tokens: 200,
                },
                ModelCostEntry {
                    model: "claude-sonnet".to_string(),
                    cost_usd: dec!(0.4),
                    input_tokens: 1000,
                    output_tokens: 800,
                },
            ],
            total_input_tokens: 1500,
            total_output_tokens: 1000,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            duration_ms: 10000,
        };
        let report = summary.format_report();
        assert!(report.contains("Total: $0.5000 (10.0s)"));
        assert!(report.contains("Model"));
        // Sonnet should come first (higher cost)
        let sonnet_pos = report.find("claude-sonnet").unwrap();
        let haiku_pos = report.find("claude-haiku").unwrap();
        assert!(sonnet_pos < haiku_pos);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let summary = CostSummary {
            total_cost_usd: dec!(1.2345),
            per_model: vec![ModelCostEntry {
                model: "test-model".to_string(),
                cost_usd: dec!(1.2345),
                input_tokens: 100,
                output_tokens: 50,
            }],
            total_input_tokens: 100,
            total_output_tokens: 50,
            cache_read_tokens: 10,
            cache_creation_tokens: 5,
            duration_ms: 3000,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: CostSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total_cost_usd, dec!(1.2345));
        assert_eq!(deserialized.per_model.len(), 1);
        assert_eq!(deserialized.per_model[0].model, "test-model");
    }
}
