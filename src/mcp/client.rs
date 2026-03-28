//! MCP Client implementation using rmcp
//!
//! This module provides a high-level MCP client that wraps rmcp functionality.

#[cfg(feature = "mcp")]
use serde_json::Value;
#[cfg(feature = "mcp")]
use std::collections::HashMap;
#[cfg(feature = "mcp")]
use std::sync::Arc;
#[cfg(feature = "mcp")]
use tokio::sync::RwLock;

use super::{
    McpConnectionStatus, McpError, McpResourceDefinition, McpResult, McpServerConfig,
    McpServerState, McpToolDefinition, McpToolResult,
};
#[cfg(feature = "mcp")]
use super::{McpContent, McpServerInfo};

#[cfg(feature = "mcp")]
use rmcp::{
    RoleClient,
    model::{CallToolRequestParams, ReadResourceRequestParams},
    service::{RunningService, ServiceError, ServiceExt},
    transport::{ConfigureCommandExt, TokioChildProcess},
};
#[cfg(feature = "mcp")]
use tokio::process::Command;

#[cfg(feature = "mcp")]
type McpRunningService = RunningService<RoleClient, ()>;

/// Convert rmcp ServiceError into our McpError, preserving JSON-RPC error codes.
#[cfg(feature = "mcp")]
fn map_service_error(e: ServiceError, context: &str) -> McpError {
    match e {
        ServiceError::McpError(err_data) => McpError::JsonRpc {
            code: err_data.code.0,
            message: err_data.message.to_string(),
        },
        _ => McpError::Protocol {
            message: format!("{}: {}", context, e),
        },
    }
}

pub struct McpClient {
    name: String,
    state: McpServerState,
    #[cfg(feature = "mcp")]
    service: Option<Arc<RwLock<McpRunningService>>>,
    #[cfg(not(feature = "mcp"))]
    _phantom: std::marker::PhantomData<()>,
}

impl McpClient {
    pub fn new(name: impl Into<String>, config: McpServerConfig) -> Self {
        let name = name.into();
        Self {
            name: name.clone(),
            state: McpServerState::new(name, config),
            #[cfg(feature = "mcp")]
            service: None,
            #[cfg(not(feature = "mcp"))]
            _phantom: std::marker::PhantomData,
        }
    }

    #[cfg(feature = "mcp")]
    pub async fn connect(&mut self) -> McpResult<()> {
        match &self.state.config {
            McpServerConfig::Stdio {
                command, args, env, ..
            } => {
                self.connect_stdio(command.clone(), args.clone(), env.clone())
                    .await
            }
            McpServerConfig::Sse { url, headers } => {
                self.connect_sse(url.clone(), headers.clone()).await
            }
        }
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn connect(&mut self) -> McpResult<()> {
        Err(McpError::Protocol {
            message: "MCP feature not enabled".to_string(),
        })
    }

    #[cfg(feature = "mcp")]
    async fn connect_stdio(
        &mut self,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> McpResult<()> {
        use tokio::time::timeout;

        let transport = TokioChildProcess::new(Command::new(&command).configure(|cmd| {
            cmd.args(&args);
            for (key, value) in &env {
                cmd.env(key, value);
            }
        }))
        .map_err(|e| McpError::ConnectionFailed {
            message: format!("Failed to create transport: {}", e),
        })?;

        let service: McpRunningService = timeout(super::MCP_CONNECT_TIMEOUT, ().serve(transport))
            .await
            .map_err(|_| McpError::ConnectionFailed {
                message: format!(
                    "Connection timed out after {:?}",
                    super::MCP_CONNECT_TIMEOUT
                ),
            })?
            .map_err(|e| McpError::ConnectionFailed {
                message: format!("Failed to connect: {}", e),
            })?;

        if let Some(info) = service.peer_info() {
            let protocol_version = info.protocol_version.to_string();

            if !super::SUPPORTED_PROTOCOL_VERSIONS.contains(&protocol_version.as_str()) {
                tracing::warn!(
                    server = %self.name,
                    server_version = %protocol_version,
                    supported = ?super::SUPPORTED_PROTOCOL_VERSIONS,
                    "MCP protocol version mismatch"
                );
            }

            self.state.server_info = Some(McpServerInfo {
                name: info.server_info.name.to_string(),
                version: info.server_info.version.to_string(),
                protocol_version,
            });
        }
        self.state.status = McpConnectionStatus::Connected;

        let tools_result = service
            .list_tools(Default::default())
            .await
            .map_err(|e| map_service_error(e, "Failed to list tools"))?;

        self.state.tools = tools_result
            .tools
            .into_iter()
            .map(|t| McpToolDefinition {
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: serde_json::Value::Object((*t.input_schema).clone()),
            })
            .collect();

        if let Ok(resources_result) = service.list_resources(Default::default()).await {
            self.state.resources = resources_result
                .resources
                .into_iter()
                .map(|r| McpResourceDefinition {
                    uri: r.raw.uri,
                    name: r.raw.name,
                    description: r.raw.description,
                    mime_type: r.raw.mime_type,
                })
                .collect();
        }

        self.service = Some(Arc::new(RwLock::new(service)));

        Ok(())
    }

    /// Connect via SSE transport.
    ///
    /// SSE transport is not yet supported by the rmcp crate.
    /// The MCP specification has moved to Streamable HTTP as the preferred
    /// remote transport. Use stdio transport for local servers.
    #[cfg(feature = "mcp")]
    async fn connect_sse(
        &mut self,
        url: String,
        _headers: HashMap<String, String>,
    ) -> McpResult<()> {
        Err(McpError::Protocol {
            message: format!(
                "SSE transport is not supported (url: {}). \
                 Use stdio transport for local MCP servers, or wait for \
                 Streamable HTTP transport support.",
                url
            ),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn state(&self) -> &McpServerState {
        &self.state
    }

    pub fn is_connected(&self) -> bool {
        self.state.is_connected()
    }

    pub fn tools(&self) -> &[McpToolDefinition] {
        &self.state.tools
    }

    pub fn resources(&self) -> &[McpResourceDefinition] {
        &self.state.resources
    }

    #[cfg(feature = "mcp")]
    pub async fn call_tool(&self, name: &str, arguments: Value) -> McpResult<McpToolResult> {
        use tokio::time::timeout;

        let service = self
            .service
            .as_ref()
            .ok_or_else(|| McpError::ConnectionFailed {
                message: "Not connected".to_string(),
            })?;

        let service = service.read().await;
        let result = timeout(
            super::MCP_CALL_TIMEOUT,
            service.call_tool({
                let mut params = CallToolRequestParams::new(name.to_string());
                if let Some(args) = arguments.as_object().cloned() {
                    params = params.with_arguments(args);
                }
                params
            }),
        )
        .await
        .map_err(|_| McpError::ToolError {
            message: format!("Tool call timed out after {:?}", super::MCP_CALL_TIMEOUT),
        })?
        .map_err(|e| map_service_error(e, "Tool call failed"))?;

        let content = result
            .content
            .into_iter()
            .map(|c| {
                // Content in rmcp 0.12 is Annotated<RawContent>
                // Access the raw content through the raw field
                match &c.raw {
                    rmcp::model::RawContent::Text(t) => McpContent::Text {
                        text: t.text.clone(),
                    },
                    rmcp::model::RawContent::Image(i) => McpContent::Image {
                        data: i.data.clone(),
                        mime_type: i.mime_type.clone(),
                    },
                    rmcp::model::RawContent::Resource(r) => {
                        // ResourceContents is an enum with TextResourceContents and BlobResourceContents
                        match &r.resource {
                            rmcp::model::ResourceContents::TextResourceContents {
                                uri,
                                mime_type,
                                text,
                                ..
                            } => McpContent::Resource {
                                uri: uri.clone(),
                                text: Some(text.clone()),
                                blob: None,
                                mime_type: mime_type.clone(),
                            },
                            rmcp::model::ResourceContents::BlobResourceContents {
                                uri,
                                mime_type,
                                blob,
                                ..
                            } => McpContent::Resource {
                                uri: uri.clone(),
                                text: None,
                                blob: Some(blob.clone()),
                                mime_type: mime_type.clone(),
                            },
                        }
                    }
                    rmcp::model::RawContent::Audio(_) => {
                        // Audio content not yet fully supported, return as text placeholder
                        McpContent::Text {
                            text: "[Audio content]".to_string(),
                        }
                    }
                    rmcp::model::RawContent::ResourceLink(r) => McpContent::Resource {
                        uri: r.uri.clone(),
                        text: None,
                        blob: None,
                        mime_type: r.mime_type.clone(),
                    },
                }
            })
            .collect();

        Ok(McpToolResult {
            content,
            is_error: result.is_error.unwrap_or(false),
        })
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn call_tool(
        &self,
        _name: &str,
        _arguments: serde_json::Value,
    ) -> McpResult<McpToolResult> {
        Err(McpError::Protocol {
            message: "MCP feature not enabled".to_string(),
        })
    }

    #[cfg(feature = "mcp")]
    pub async fn read_resource(&self, uri: &str) -> McpResult<Vec<McpContent>> {
        use tokio::time::timeout;

        let service = self
            .service
            .as_ref()
            .ok_or_else(|| McpError::ConnectionFailed {
                message: "Not connected".to_string(),
            })?;

        let service = service.read().await;
        let result = timeout(
            super::MCP_RESOURCE_TIMEOUT,
            service.read_resource(ReadResourceRequestParams::new(uri)),
        )
        .await
        .map_err(|_| McpError::ResourceNotFound {
            uri: format!("{}: timed out after {:?}", uri, super::MCP_RESOURCE_TIMEOUT),
        })?
        .map_err(|e| map_service_error(e, &format!("Resource read failed for {}", uri)))?;

        Ok(result
            .contents
            .into_iter()
            .map(|c| match c {
                rmcp::model::ResourceContents::TextResourceContents {
                    uri,
                    text,
                    mime_type,
                    ..
                } => McpContent::Resource {
                    uri,
                    text: Some(text),
                    blob: None,
                    mime_type,
                },
                rmcp::model::ResourceContents::BlobResourceContents {
                    uri,
                    blob,
                    mime_type,
                    ..
                } => McpContent::Resource {
                    uri,
                    text: None,
                    blob: Some(blob),
                    mime_type,
                },
            })
            .collect())
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn read_resource(&self, _uri: &str) -> McpResult<Vec<super::McpContent>> {
        Err(McpError::Protocol {
            message: "MCP feature not enabled".to_string(),
        })
    }

    #[cfg(feature = "mcp")]
    pub async fn close(&mut self) -> McpResult<()> {
        if let Some(service_arc) = self.service.take() {
            match Arc::try_unwrap(service_arc) {
                Ok(service_rwlock) => {
                    let service = service_rwlock.into_inner();
                    service.cancel().await.map_err(|e| McpError::Protocol {
                        message: format!("Failed to cancel: {}", e),
                    })?;
                    self.state.status = McpConnectionStatus::Disconnected;
                }
                Err(arc) => {
                    tracing::debug!(
                        server = %self.name,
                        refs = Arc::strong_count(&arc),
                        "MCP service has active references, deferring cleanup"
                    );
                    self.service = Some(arc);
                    return Err(McpError::Protocol {
                        message: "Cannot close: service has active references".to_string(),
                    });
                }
            }
        } else {
            self.state.status = McpConnectionStatus::Disconnected;
        }
        Ok(())
    }

    #[cfg(not(feature = "mcp"))]
    pub async fn close(&mut self) -> McpResult<()> {
        self.state.status = McpConnectionStatus::Disconnected;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_client_new() {
        let client = McpClient::new(
            "test",
            McpServerConfig::Stdio {
                command: "echo".to_string(),
                args: vec![],
                env: std::collections::HashMap::new(),
                cwd: None,
            },
        );

        assert_eq!(client.name(), "test");
        assert!(!client.is_connected());
    }
}
