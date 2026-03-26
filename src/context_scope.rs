//! Optional execution context scope for propagating task-locals, tracing spans,
//! or other per-request context into agent tool execution and event callbacks.
//!
//! When set on an agent via [`AgentBuilder::context_scope()`], every tool
//! execution future will be wrapped by [`ContextScope::wrap_tool_future()`]
//! before awaiting.
//!
//! This enables patterns like:
//! - Multi-tenant workspace isolation (database RLS via task-locals)
//! - OpenTelemetry span propagation
//! - Request-scoped authorization context
//!
//! # Example
//!
//! ```rust
//! use std::future::Future;
//! use std::pin::Pin;
//! use std::sync::Arc;
//! use branchforge::{ContextScope, ToolResult};
//!
//! struct TracingScope {
//!     trace_id: String,
//! }
//!
//! impl ContextScope for TracingScope {
//!     fn wrap_tool_future<'a>(
//!         &'a self,
//!         fut: Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>,
//!     ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
//!         // In practice, you would wrap with a tracing span or task-local scope.
//!         fut
//!     }
//! }
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::types::ToolResult;

/// Wraps tool execution futures with per-request context.
///
/// Implement this trait to propagate task-locals, tracing spans, or other
/// execution context into parallel tool invocations. The agent calls
/// [`wrap_tool_future`](ContextScope::wrap_tool_future) on every tool
/// execution future before it is awaited.
///
/// The scope is stored as `Arc<dyn ContextScope>` inside the agent, so a
/// single instance is shared across all concurrent tool calls within one
/// agent execution.
pub trait ContextScope: Send + Sync + 'static {
    /// Wrap a tool execution future with execution context.
    ///
    /// The returned future must eventually produce the same `ToolResult` as the
    /// input future. Implementations typically set up task-locals or enter a
    /// tracing span, then await the inner future inside that scope.
    fn wrap_tool_future<'a>(
        &'a self,
        fut: Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;
}

/// Type alias for a shared context scope reference.
pub type SharedContextScope = Arc<dyn ContextScope>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TestScope {
        entered: Arc<AtomicBool>,
    }

    impl ContextScope for TestScope {
        fn wrap_tool_future<'a>(
            &'a self,
            fut: Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>,
        ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
            self.entered.store(true, Ordering::SeqCst);
            fut
        }
    }

    #[tokio::test]
    async fn test_context_scope_wraps_future() {
        let entered = Arc::new(AtomicBool::new(false));
        let scope = TestScope {
            entered: Arc::clone(&entered),
        };

        let result = ToolResult::success("ok");
        let fut = Box::pin(async { result });
        let wrapped = scope.wrap_tool_future(fut);
        let output = wrapped.await;

        assert!(entered.load(Ordering::SeqCst));
        assert_eq!(output.text(), "ok");
    }

    #[tokio::test]
    async fn test_shared_context_scope() {
        let entered = Arc::new(AtomicBool::new(false));
        let scope: SharedContextScope = Arc::new(TestScope {
            entered: Arc::clone(&entered),
        });

        let result = ToolResult::success("shared");
        let fut = Box::pin(async { result });
        let wrapped = scope.wrap_tool_future(fut);
        let output = wrapped.await;

        assert!(entered.load(Ordering::SeqCst));
        assert_eq!(output.text(), "shared");
    }

    #[tokio::test]
    async fn test_none_scope_passthrough() {
        // Verify that Option<SharedContextScope> = None works as a no-op pattern
        let scope: Option<SharedContextScope> = None;
        let result = ToolResult::success("pass");

        let output = if let Some(ref s) = scope {
            let fut = Box::pin(async { result });
            s.wrap_tool_future(fut).await
        } else {
            result
        };

        assert_eq!(output.text(), "pass");
    }
}
