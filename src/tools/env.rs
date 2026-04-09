//! Tool execution environment.

#[cfg(feature = "coding-tools")]
use std::sync::Arc;

#[cfg(feature = "coding-tools")]
use super::ProcessScheduler;
use super::context::ExecutionContext;
use crate::session::session_state::ToolState;

#[derive(Clone)]
pub struct ToolExecutionEnv {
    context: ExecutionContext,
    tool_state: Option<ToolState>,
    #[cfg(feature = "coding-tools")]
    process_manager: Option<Arc<ProcessScheduler>>,
}

impl ToolExecutionEnv {
    pub fn new(context: ExecutionContext) -> Self {
        Self {
            context,
            tool_state: None,
            #[cfg(feature = "coding-tools")]
            process_manager: None,
        }
    }

    pub fn with_tool_state(mut self, state: ToolState) -> Self {
        self.tool_state = Some(state);
        self
    }

    #[cfg(feature = "coding-tools")]
    pub fn with_process_manager(mut self, pm: Arc<ProcessScheduler>) -> Self {
        self.process_manager = Some(pm);
        self
    }

    pub fn context(&self) -> &ExecutionContext {
        &self.context
    }

    pub fn tool_state(&self) -> Option<&ToolState> {
        self.tool_state.as_ref()
    }

    #[cfg(feature = "coding-tools")]
    pub fn process_manager(&self) -> Option<&Arc<ProcessScheduler>> {
        self.process_manager.as_ref()
    }
}

impl Default for ToolExecutionEnv {
    fn default() -> Self {
        Self::new(ExecutionContext::default())
    }
}
