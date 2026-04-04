//! SendMessage tool — inter-agent communication.
//!
//! Allows the coordinator agent to send follow-up messages to running
//! worker agents, continuing their context without spawning new agents.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use super::directory::{AgentDirectory, AgentId};
use crate::tools::{ExecutionContext, SchemaTool};
use crate::types::ToolResult;

/// Input for the SendMessage tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendMessageInput {
    /// Name of the target agent to send the message to.
    pub to: String,
    /// Message content to send to the agent.
    pub content: String,
}

/// Tool for sending messages to running agents.
///
/// Used by the coordinator to continue a worker's context with
/// additional instructions without spawning a new agent.
pub struct SendMessageTool {
    directory: Arc<AgentDirectory>,
    coordinator_id: AgentId,
}

impl SendMessageTool {
    pub fn new(directory: Arc<AgentDirectory>, coordinator_id: AgentId) -> Self {
        Self {
            directory,
            coordinator_id,
        }
    }
}

#[async_trait]
impl SchemaTool for SendMessageTool {
    type Input = SendMessageInput;
    const NAME: &'static str = "SendMessage";
    const DESCRIPTION: &'static str = "Send a follow-up message to a running agent. \
        The agent resumes with its full context preserved. Use this to continue \
        a previously spawned agent with additional instructions rather than \
        spawning a new one. The target agent must be currently running.";

    async fn handle(&self, input: Self::Input, _context: &ExecutionContext) -> ToolResult {
        match self
            .directory
            .send(self.coordinator_id, &input.to, &input.content)
            .await
        {
            Ok(()) => ToolResult::success(format!("Message sent to agent '{}'.", input.to)),
            Err(e) => ToolResult::error(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::directory::AgentHandle;
    use crate::orchestration::messaging::MessageChannel;
    use crate::security::SecurityContext;

    fn test_context() -> ExecutionContext {
        ExecutionContext::new(
            SecurityContext::try_permissive()
                .expect("failed to create permissive security context"),
        )
    }

    #[tokio::test]
    async fn send_message_to_running_agent() {
        let dir = Arc::new(AgentDirectory::new());
        let ch = Arc::new(MessageChannel::new(8));
        let handle = Arc::new(AgentHandle::new("worker-1", ch.clone()));
        dir.register(handle);

        let tool = SendMessageTool::new(dir, AgentId::new());
        let ctx = test_context();

        let result = SchemaTool::handle(
            &tool,
            SendMessageInput {
                to: "worker-1".into(),
                content: "check file.rs".into(),
            },
            &ctx,
        )
        .await;

        assert!(!result.is_error());

        let msg = ch.recv().await.unwrap();
        assert_eq!(msg.content, "check file.rs");
    }

    #[tokio::test]
    async fn send_message_to_unknown_agent() {
        let dir = Arc::new(AgentDirectory::new());
        let tool = SendMessageTool::new(dir, AgentId::new());
        let ctx = test_context();

        let result = SchemaTool::handle(
            &tool,
            SendMessageInput {
                to: "nonexistent".into(),
                content: "hello".into(),
            },
            &ctx,
        )
        .await;

        assert!(result.is_error());
    }
}
