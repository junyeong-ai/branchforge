#![allow(missing_docs)]

/// A tool call proposed by the model, before any filtering or approval.
#[derive(Debug, Clone)]
pub struct ToolCallProposal {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// The result of a [`ToolSelectionStrategy`] decision.
#[derive(Debug, Default)]
pub struct ToolPlan {
    /// Tool calls that should proceed to execution.
    pub execute: Vec<ToolCallProposal>,
    /// Tool calls that should be skipped, each with a reason.
    pub skip: Vec<(ToolCallProposal, String)>,
}

/// Snapshot of loop state available when the tool selection strategy runs.
pub struct ToolSelectionContext<'a> {
    pub iteration: usize,
    pub total_usage: &'a crate::ir::Usage,
    pub model: &'a str,
}

/// Extension point for custom tool-call filtering.
///
/// Called after the model returns tool calls but before hooks, HITL
/// approval, and validation. The strategy can reorder, filter, or skip
/// tool calls based on business logic (e.g. "only allow 3 Bash calls
/// per turn", "skip destructive tools in read-only mode").
///
/// The default ([`DefaultToolSelectionStrategy`]) passes everything
/// through unchanged.
pub trait ToolSelectionStrategy: Send + Sync {
    fn plan(&self, calls: Vec<ToolCallProposal>, ctx: &ToolSelectionContext<'_>) -> ToolPlan;
}

/// Default strategy: execute all proposed tool calls, skip none.
#[derive(Debug, Default)]
pub struct DefaultToolSelectionStrategy;

impl ToolSelectionStrategy for DefaultToolSelectionStrategy {
    fn plan(&self, calls: Vec<ToolCallProposal>, _ctx: &ToolSelectionContext<'_>) -> ToolPlan {
        ToolPlan {
            execute: calls,
            skip: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_strategy_passes_all_through() {
        let strategy = DefaultToolSelectionStrategy;
        let calls = vec![
            ToolCallProposal {
                id: "1".into(),
                name: "Read".into(),
                input: serde_json::json!({"file_path": "/x"}),
            },
            ToolCallProposal {
                id: "2".into(),
                name: "Bash".into(),
                input: serde_json::json!({"command": "ls"}),
            },
        ];
        let usage = crate::ir::Usage::default();
        let ctx = ToolSelectionContext {
            iteration: 1,
            total_usage: &usage,
            model: "test",
        };
        let plan = strategy.plan(calls, &ctx);
        assert_eq!(plan.execute.len(), 2);
        assert!(plan.skip.is_empty());
    }

    #[test]
    fn custom_strategy_can_filter() {
        struct NoBash;
        impl ToolSelectionStrategy for NoBash {
            fn plan(
                &self,
                calls: Vec<ToolCallProposal>,
                _ctx: &ToolSelectionContext<'_>,
            ) -> ToolPlan {
                let (execute, skip): (Vec<_>, Vec<_>) =
                    calls.into_iter().partition(|c| c.name != "Bash");
                ToolPlan {
                    execute,
                    skip: skip
                        .into_iter()
                        .map(|c| (c, "Bash blocked".into()))
                        .collect(),
                }
            }
        }

        let strategy = NoBash;
        let calls = vec![
            ToolCallProposal {
                id: "1".into(),
                name: "Read".into(),
                input: serde_json::json!({}),
            },
            ToolCallProposal {
                id: "2".into(),
                name: "Bash".into(),
                input: serde_json::json!({}),
            },
        ];
        let usage = crate::ir::Usage::default();
        let ctx = ToolSelectionContext {
            iteration: 1,
            total_usage: &usage,
            model: "test",
        };
        let plan = strategy.plan(calls, &ctx);
        assert_eq!(plan.execute.len(), 1);
        assert_eq!(plan.execute[0].name, "Read");
        assert_eq!(plan.skip.len(), 1);
        assert_eq!(plan.skip[0].0.name, "Bash");
    }
}
