//! Tool execution environment.

use std::sync::Arc;

use super::ProcessManager;
use super::context::ExecutionContext;
use crate::session::session_state::ToolState;

#[derive(Clone)]
pub struct ToolExecutionEnv {
    context: ExecutionContext,
    tool_state: Option<ToolState>,
    process_manager: Option<Arc<ProcessManager>>,
}

impl ToolExecutionEnv {
    pub fn new(context: ExecutionContext) -> Self {
        Self {
            context,
            tool_state: None,
            process_manager: None,
        }
    }

    pub fn with_tool_state(mut self, state: ToolState) -> Self {
        self.tool_state = Some(state);
        self
    }

    pub fn with_process_manager(mut self, pm: Arc<ProcessManager>) -> Self {
        self.process_manager = Some(pm);
        self
    }

    pub fn context(&self) -> &ExecutionContext {
        &self.context
    }

    pub fn tool_state(&self) -> Option<&ToolState> {
        self.tool_state.as_ref()
    }

    pub fn process_manager(&self) -> Option<&Arc<ProcessManager>> {
        self.process_manager.as_ref()
    }
}

impl Default for ToolExecutionEnv {
    fn default() -> Self {
        Self::new(ExecutionContext::default())
    }
}
