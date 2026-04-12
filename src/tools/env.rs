//! Tool execution environment.

#![allow(missing_docs)]

#[cfg(feature = "coding-tools")]
use std::sync::Arc;

#[cfg(feature = "coding-tools")]
use super::ProcessScheduler;
use super::context::ExecutionContext;
use crate::session::session_handle::SessionHandle;

#[derive(Clone)]
pub struct ToolExecutionEnv {
    context: ExecutionContext,
    session_handle: Option<SessionHandle>,
    #[cfg(feature = "coding-tools")]
    process_manager: Option<Arc<ProcessScheduler>>,
}

impl ToolExecutionEnv {
    pub fn new(context: ExecutionContext) -> Self {
        Self {
            context,
            session_handle: None,
            #[cfg(feature = "coding-tools")]
            process_manager: None,
        }
    }

    pub fn with_session_handle(mut self, state: SessionHandle) -> Self {
        self.session_handle = Some(state);
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

    pub fn session_handle(&self) -> Option<&SessionHandle> {
        self.session_handle.as_ref()
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
