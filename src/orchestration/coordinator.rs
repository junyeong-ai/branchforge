//! Coordinator orchestration pattern.
//!
//! Implements the coordinator pattern inspired by claw-code:
//! - Research: parallel workers gather information
//! - Synthesis: coordinator proves understanding with specific details
//! - Implementation: coordinator or workers apply changes
//! - Verification: validate results
//!
//! Key principle: "Never delegate understanding." The coordinator must
//! reference specific file paths and line numbers rather than blindly
//! trusting worker summaries.

use std::sync::Arc;

use super::traits::{Coordination, CoordinationContext};
use super::worker::WorkerConstraints;
use crate::tools::Tool;

/// Coordinator orchestration mode.
///
/// Configures the agent as a coordinator that manages worker agents
/// for complex, multi-step tasks. Workers are isolated (no access to
/// coordinator conversation history) and receive self-contained prompts.
///
/// # Usage
///
/// ```rust,no_run
/// use branchforge::orchestration::Coordinator;
///
/// let coordinator = Coordinator::builder()
///     .max_workers(5)
///     .worker_model("claude-haiku-4-5")
///     .build();
/// ```
pub struct Coordinator {
    /// Maximum number of concurrent workers.
    pub max_workers: usize,
    /// Default model for worker agents (if not specified per-worker).
    pub worker_model: Option<String>,
    /// Whether synthesis step is mandatory before implementation.
    pub require_synthesis: bool,
}

impl Coordinator {
    pub fn builder() -> CoordinatorBuilder {
        CoordinatorBuilder::default()
    }
}

impl Default for Coordinator {
    fn default() -> Self {
        Self {
            max_workers: 10,
            worker_model: None,
            require_synthesis: true,
        }
    }
}

impl Coordination for Coordinator {
    fn name(&self) -> &str {
        "coordinator"
    }

    fn system_prompt_supplement(&self, _ctx: &CoordinationContext) -> String {
        let mut prompt = String::from(COORDINATOR_SYSTEM_PROMPT);

        if let Some(ref model) = self.worker_model {
            prompt.push_str(&format!(
                "\n\nDefault worker model: {}. Override per-worker if needed.",
                model
            ));
        }

        prompt.push_str(&format!(
            "\n\nMaximum concurrent workers: {}.",
            self.max_workers
        ));

        if self.require_synthesis {
            prompt.push_str(
                "\n\nIMPORTANT: You MUST synthesize worker findings yourself before \
                 proceeding to implementation. Prove your understanding with specific \
                 file paths, line numbers, and code references.",
            );
        }

        prompt
    }

    fn additional_tools(&self, _ctx: &CoordinationContext) -> Vec<Arc<dyn Tool>> {
        // SendMessageTool is added by the AgentBuilder when coordination is set.
        // No additional tools needed from the Coordinator itself.
        Vec::new()
    }

    fn worker_constraints(&self) -> WorkerConstraints {
        WorkerConstraints {
            isolated_context: true,
            self_contained_prompt: true,
            max_concurrent: self.max_workers,
        }
    }
}

/// Builder for [`Coordinator`].
#[derive(Default)]
pub struct CoordinatorBuilder {
    max_workers: Option<usize>,
    worker_model: Option<String>,
    require_synthesis: Option<bool>,
}

impl CoordinatorBuilder {
    pub fn max_workers(mut self, max: usize) -> Self {
        self.max_workers = Some(max);
        self
    }

    pub fn worker_model(mut self, model: impl Into<String>) -> Self {
        self.worker_model = Some(model.into());
        self
    }

    pub fn require_synthesis(mut self, required: bool) -> Self {
        self.require_synthesis = Some(required);
        self
    }

    pub fn build(self) -> Coordinator {
        Coordinator {
            max_workers: self.max_workers.unwrap_or(10),
            worker_model: self.worker_model,
            require_synthesis: self.require_synthesis.unwrap_or(true),
        }
    }
}

const COORDINATOR_SYSTEM_PROMPT: &str = r#"# Coordinator Mode

You are operating as a coordinator agent. Your role is to orchestrate multiple worker agents to accomplish complex tasks efficiently.

## Workflow

1. **Research Phase**: Launch independent workers concurrently to gather information.
   - Use the Task tool to spawn workers for parallel research.
   - Use SendMessage to send follow-up instructions to running workers.
   - Each worker prompt must be self-contained — workers cannot see your conversation.

2. **Synthesis Phase**: Analyze and integrate worker findings.
   - Never write "based on your findings" or delegate understanding.
   - Prove comprehension with specific file paths, line numbers, and code references.
   - Identify conflicts or gaps in worker reports.

3. **Implementation Phase**: Apply changes based on synthesized understanding.
   - You may implement directly or delegate to workers with precise instructions.
   - Include file paths, exact code snippets, and clear expected outcomes.

4. **Verification Phase**: Validate that changes are correct.
   - Run tests, check for regressions, verify integration.

## Principles

- **Never delegate understanding**: You must prove you understood the findings.
- **Self-contained prompts**: Every worker prompt must include all necessary context.
- **Parallel when possible**: Launch independent research workers concurrently.
- **Sequential when dependent**: Wait for research results before implementation."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinator_default() {
        let coord = Coordinator::default();
        assert_eq!(coord.name(), "coordinator");
        assert_eq!(coord.max_workers, 10);
        assert!(coord.require_synthesis);
        assert!(coord.worker_model.is_none());
    }

    #[test]
    fn coordinator_builder() {
        let coord = Coordinator::builder()
            .max_workers(5)
            .worker_model("claude-haiku-4-5")
            .require_synthesis(false)
            .build();

        assert_eq!(coord.max_workers, 5);
        assert_eq!(coord.worker_model.as_deref(), Some("claude-haiku-4-5"));
        assert!(!coord.require_synthesis);
    }

    #[test]
    fn coordinator_worker_constraints() {
        let coord = Coordinator::builder().max_workers(3).build();
        let constraints = coord.worker_constraints();
        assert!(constraints.isolated_context);
        assert!(constraints.self_contained_prompt);
        assert_eq!(constraints.max_concurrent, 3);
    }
}
