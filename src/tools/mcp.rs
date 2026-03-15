//! MCP tool wrapper for seamless integration with ToolRegistry.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{ExecutionContext, Tool, ToolSurface};
use crate::mcp::{McpManager, McpToolDefinition, make_mcp_name, parse_mcp_name};
use crate::types::{ToolDefinition, ToolResult};

/// Wrapper that adapts an MCP tool to the Tool trait.
///
/// This enables MCP tools to be registered in ToolRegistry and called
/// through the standard tool execution pipeline.
pub struct McpToolWrapper {
    manager: Arc<McpManager>,
    qualified_name: String,
    server_name: String,
    tool_name: String,
    definition: McpToolDefinition,
}

impl McpToolWrapper {
    pub fn new(
        manager: Arc<McpManager>,
        server_name: impl Into<String>,
        tool: McpToolDefinition,
    ) -> Self {
        let server_name = server_name.into();
        let qualified_name = make_mcp_name(&server_name, &tool.name);
        Self {
            manager,
            qualified_name,
            server_name,
            tool_name: tool.name.clone(),
            definition: tool,
        }
    }

    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> &str {
        &self.qualified_name
    }

    fn description(&self) -> &str {
        &self.definition.description
    }

    fn input_schema(&self) -> Value {
        self.definition.input_schema.clone()
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            &self.qualified_name,
            &self.definition.description,
            self.definition.input_schema.clone(),
        )
    }

    async fn execute(&self, input: Value, _context: &ExecutionContext) -> ToolResult {
        match self.manager.call_tool(&self.qualified_name, input).await {
            Ok(result) => {
                if result.is_error {
                    ToolResult::error(result.to_string_content())
                } else {
                    ToolResult::success(result.to_string_content())
                }
            }
            Err(e) => ToolResult::error(e.to_string()),
        }
    }
}

/// Creates MCP tool wrappers for all tools from an McpManager.
pub async fn create_mcp_tools(manager: Arc<McpManager>) -> Vec<Arc<dyn Tool>> {
    create_mcp_tools_with_access(manager, &ToolSurface::all()).await
}

/// Creates MCP tool wrappers filtered through the current tool access policy.
pub async fn create_mcp_tools_with_access(
    manager: Arc<McpManager>,
    access: &ToolSurface,
) -> Vec<Arc<dyn Tool>> {
    let tools_list = manager.list_tools().await;
    let mut wrappers: Vec<Arc<dyn Tool>> = Vec::with_capacity(tools_list.len());

    for (qualified_name, definition) in tools_list {
        if !access.is_allowed(&qualified_name) {
            continue;
        }
        if let Some((server, _tool)) = parse_mcp_name(&qualified_name) {
            let wrapper = McpToolWrapper::new(Arc::clone(&manager), server, definition);
            wrappers.push(Arc::new(wrapper));
        }
    }

    wrappers
}
