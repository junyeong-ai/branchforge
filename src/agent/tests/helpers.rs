//! Test helper types for agent tests.

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::hooks::{Hook, HookContext, HookEvent, HookInput, HookOutput};
use crate::tools::{ExecutionContext, Tool, ToolOutput, ToolResult};

pub struct TestTrackingHook {
    pub name: String,
    pub events: Vec<HookEvent>,
    pub call_count: Arc<AtomicUsize>,
}

impl TestTrackingHook {
    pub fn new(name: &str, events: Vec<HookEvent>) -> Self {
        Self {
            name: name.to_string(),
            events,
            call_count: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl Hook for TestTrackingHook {
    fn name(&self) -> &str {
        &self.name
    }

    fn events(&self) -> &[HookEvent] {
        &self.events
    }

    async fn execute(
        &self,
        _input: HookInput,
        _hook_context: &HookContext,
    ) -> crate::Result<HookOutput> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(HookOutput::allow())
    }
}

pub struct BlockingHook {
    pub reason: String,
}

#[async_trait]
impl Hook for BlockingHook {
    fn name(&self) -> &str {
        "blocking-hook"
    }

    fn events(&self) -> &[HookEvent] {
        &[HookEvent::PreToolUse, HookEvent::UserPromptSubmit]
    }

    async fn execute(
        &self,
        _input: HookInput,
        _hook_context: &HookContext,
    ) -> crate::Result<HookOutput> {
        Ok(HookOutput::block(&self.reason))
    }
}

pub struct InputModifyingHook;

#[async_trait]
impl Hook for InputModifyingHook {
    fn name(&self) -> &str {
        "input-modifier"
    }

    fn events(&self) -> &[HookEvent] {
        &[HookEvent::PreToolUse]
    }

    async fn execute(
        &self,
        _input: HookInput,
        _hook_context: &HookContext,
    ) -> crate::Result<HookOutput> {
        Ok(HookOutput::allow().updated_input(serde_json::json!({
            "file_path": "/modified/path"
        })))
    }
}

pub struct DummyTool {
    pub name: String,
    pub output: ToolOutput,
}

#[async_trait]
impl Tool for DummyTool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Dummy tool for testing"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _input: serde_json::Value, _context: &ExecutionContext) -> ToolResult {
        ToolResult::from(self.output.clone())
    }
}
