//! Request building utilities for agent execution.

use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::config::{AgentConfig, CacheConfig, ServerToolsConfig, SystemPromptMode};
use crate::client::messages::{CreateMessageRequest, RequestMetadata};
use crate::context::{McpToolMeta, StaticContext};
use crate::output_style::{OutputStyle, SystemPromptGenerator};
use crate::tools::ToolRegistry;
use crate::tools::search::{PreparedTools, SearchMode};
use crate::types::{CacheTtl, Message, SystemBlock, SystemPrompt, ToolDefinition, ToolSearchTool};

pub struct RequestBuilder {
    model: String,
    max_tokens: u32,
    tools: Arc<ToolRegistry>,
    server_tools: ServerToolsConfig,
    tool_access: crate::tools::ToolAccess,
    system_prompt_mode: SystemPromptMode,
    custom_system_prompt: Option<String>,
    base_system_prompt: String,
    static_context: StaticContext,
    cache_config: CacheConfig,
    prepared_mcp_tools: Option<PreparedTools>,
    /// JSON schema for structured output
    output_schema: Option<serde_json::Value>,
    metadata: Option<RequestMetadata>,
}

impl RequestBuilder {
    pub fn new(
        config: &AgentConfig,
        tools: Arc<ToolRegistry>,
        static_context: StaticContext,
    ) -> Self {
        let base_system_prompt = Self::generate_base_prompt(
            &config.model.primary,
            config.working_dir.as_ref(),
            config.prompt.output_style.as_ref(),
        );

        Self {
            model: config.model.primary.clone(),
            max_tokens: config.model.max_tokens,
            tools,
            server_tools: config.server_tools.clone(),
            tool_access: config.security.tool_access.clone(),
            system_prompt_mode: config.prompt.system_prompt_mode,
            custom_system_prompt: config.prompt.system_prompt.clone(),
            base_system_prompt,
            static_context,
            cache_config: config.cache.clone(),
            prepared_mcp_tools: None,
            output_schema: config.prompt.output_schema.clone(),
            metadata: None,
        }
    }

    pub fn prepared_tools(mut self, prepared: PreparedTools) -> Self {
        self.prepared_mcp_tools = Some(prepared);
        self
    }

    pub fn metadata(mut self, metadata: Option<RequestMetadata>) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    pub fn build(&self, messages: Vec<Message>, dynamic_rules: &str) -> CreateMessageRequest {
        let prepared_tools = self.prepare_request_tools();
        let system_prompt = self.build_system_prompt_blocks(dynamic_rules, &prepared_tools);

        let mut request = CreateMessageRequest::new(&self.model, messages)
            .max_tokens(self.max_tokens)
            .system(system_prompt);

        if !prepared_tools.tool_definitions.is_empty() {
            request = request.tools(prepared_tools.tool_definitions.clone());
        }

        if let Some(metadata) = self.metadata.clone() {
            request = request.metadata(metadata);
        }

        if let Some(tool_search) = prepared_tools.tool_search {
            request = request.tool_search(tool_search);
        }

        if self.tool_access.is_allowed("WebSearch") {
            let web_search = self.server_tools.web_search.clone().unwrap_or_default();
            request = request.web_search(web_search);
        }

        if self.tool_access.is_allowed("WebFetch") {
            let web_fetch = self.server_tools.web_fetch.clone().unwrap_or_default();
            request = request.web_fetch(web_fetch);
        }

        // Add structured output schema if configured
        if let Some(ref schema) = self.output_schema {
            request = request.json_schema(schema.clone());
        }

        request
    }

    fn build_system_prompt_blocks(
        &self,
        dynamic_rules: &str,
        prepared_tools: &PreparedRequestTools,
    ) -> SystemPrompt {
        let mut blocks = Vec::new();

        let mut static_context = self
            .static_context
            .clone()
            .system_prompt(self.composed_system_prompt());
        if !prepared_tools.static_tool_definitions.is_empty() {
            static_context = static_context.tools(prepared_tools.static_tool_definitions.clone());
        }
        if !prepared_tools.mcp_tool_metadata.is_empty() {
            static_context = static_context.mcp_tools(prepared_tools.mcp_tool_metadata.clone());
        }

        blocks.extend(static_context.to_system_blocks(
            self.cache_config.strategy.cache_static(),
            self.cache_config.static_ttl,
        ));

        if let Some(tool_summary) = static_context.tool_summary() {
            let mut combined_tool_summary = tool_summary;
            if !prepared_tools.server_tool_summaries.is_empty() {
                combined_tool_summary.push_str("\n\n# Server Tools\n");
                combined_tool_summary.push_str(&prepared_tools.server_tool_summaries.join("\n"));
            }
            blocks.push(self.make_block(
                &combined_tool_summary,
                self.cache_config.strategy.cache_tools(),
                self.cache_config.static_ttl,
            ));
        } else if !prepared_tools.server_tool_summaries.is_empty() {
            blocks.push(self.make_block(
                &format!(
                    "# Server Tools\n{}",
                    prepared_tools.server_tool_summaries.join("\n")
                ),
                self.cache_config.strategy.cache_tools(),
                self.cache_config.static_ttl,
            ));
        }

        // Dynamic rules are never cached (they change frequently)
        if !dynamic_rules.is_empty() {
            blocks.push(SystemBlock::uncached(dynamic_rules));
        }

        if blocks.is_empty() {
            SystemPrompt::Text(String::new())
        } else {
            SystemPrompt::Blocks(blocks)
        }
    }

    fn prepare_request_tools(&self) -> PreparedRequestTools {
        let registry_tools = self.tools.definitions();
        let builtin_tools: Vec<_> = registry_tools
            .into_iter()
            .filter(|tool| !crate::mcp::is_mcp_name(&tool.name))
            .collect();

        match &self.prepared_mcp_tools {
            Some(prepared) => {
                let mut tool_definitions = Vec::with_capacity(
                    builtin_tools.len() + prepared.immediate.len() + prepared.deferred.len(),
                );
                tool_definitions.extend(builtin_tools.iter().cloned());
                tool_definitions.extend(prepared.immediate.iter().cloned());
                tool_definitions.extend(prepared.deferred.iter().cloned());

                let mcp_tool_metadata = prepared
                    .immediate
                    .iter()
                    .chain(prepared.deferred.iter())
                    .filter_map(|tool| {
                        split_mcp_tool_name(&tool.name).map(|(server, name)| McpToolMeta {
                            server: server.to_string(),
                            name: name.to_string(),
                            description: tool.description.clone(),
                        })
                    })
                    .collect();

                PreparedRequestTools {
                    tool_definitions,
                    static_tool_definitions: builtin_tools,
                    mcp_tool_metadata,
                    server_tool_summaries: self.server_tool_summaries(prepared.use_search),
                    tool_search: if prepared.use_search {
                        Some(match prepared.search_mode {
                            SearchMode::Regex => ToolSearchTool::regex(),
                            SearchMode::Bm25 => ToolSearchTool::bm25(),
                        })
                    } else {
                        None
                    },
                }
            }
            None => PreparedRequestTools {
                static_tool_definitions: builtin_tools.clone(),
                tool_definitions: builtin_tools,
                mcp_tool_metadata: Vec::new(),
                server_tool_summaries: self.server_tool_summaries(false),
                tool_search: None,
            },
        }
    }

    fn composed_system_prompt(&self) -> String {
        match self.system_prompt_mode {
            SystemPromptMode::Replace => self
                .custom_system_prompt
                .clone()
                .unwrap_or_else(|| self.base_system_prompt.clone()),
            SystemPromptMode::Append => {
                let mut base = self.base_system_prompt.clone();
                if let Some(custom) = &self.custom_system_prompt {
                    base.push_str("\n\n");
                    base.push_str(custom);
                }
                base
            }
        }
    }

    fn make_block(&self, text: &str, cached: bool, ttl: CacheTtl) -> SystemBlock {
        if cached {
            SystemBlock::cached_with_ttl(text, ttl)
        } else {
            SystemBlock::uncached(text)
        }
    }

    fn server_tool_summaries(&self, include_tool_search: bool) -> Vec<String> {
        let mut lines = Vec::new();
        if self.tool_access.is_allowed("WebSearch") {
            lines.push("- web_search: server-side web search".to_string());
        }
        if self.tool_access.is_allowed("WebFetch") {
            lines.push("- web_fetch: server-side URL fetch".to_string());
        }
        if include_tool_search {
            lines.push("- tool_search: MCP deferred tool search".to_string());
        }
        lines
    }

    fn generate_base_prompt(
        model: &str,
        working_dir: Option<&PathBuf>,
        output_style: Option<&OutputStyle>,
    ) -> String {
        let mut generator = SystemPromptGenerator::new().model(model);

        if let Some(dir) = working_dir {
            generator = generator.working_dir(dir);
        }

        if let Some(style) = output_style {
            generator = generator.output_style(style.clone());
        }

        generator.generate()
    }
}

#[derive(Default)]
struct PreparedRequestTools {
    tool_definitions: Vec<ToolDefinition>,
    static_tool_definitions: Vec<ToolDefinition>,
    mcp_tool_metadata: Vec<McpToolMeta>,
    server_tool_summaries: Vec<String>,
    tool_search: Option<ToolSearchTool>,
}

fn split_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix("mcp__")?.split_once("__")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::agent::config::{AgentConfig, CacheConfig, CacheStrategy};
    use crate::tools::{ToolAccess, ToolRegistry};
    use crate::types::CacheTtl;

    fn test_config() -> AgentConfig {
        AgentConfig::default()
    }

    #[test]
    fn static_context_is_wired_into_system_blocks() {
        let config = test_config();
        let tools = Arc::new(ToolRegistry::new());
        let static_context = StaticContext::new()
            .claude_md("# Project Memory")
            .skill_summary("# Available Skills\n- test")
            .rules_summary("# Available Rules\n- rule");

        let builder = RequestBuilder::new(&config, tools, static_context);
        let request = builder.build(vec![Message::user("hello")], "# Active Rules");
        let system = request.system.expect("system prompt should exist");
        let text = system.as_text();

        assert!(text.contains("Project Memory"));
        assert!(text.contains("Available Skills"));
        assert!(text.contains("Available Rules"));
        assert!(text.contains("Active Rules"));
    }

    #[test]
    fn tools_segment_can_be_cached_independently() {
        let mut config = AgentConfig::default();
        config.cache = CacheConfig::default()
            .strategy(CacheStrategy::ToolsOnly)
            .static_ttl(CacheTtl::OneHour);
        let tools = Arc::new(ToolRegistry::default_tools(ToolAccess::All, None, None));
        let builder = RequestBuilder::new(&config, tools, StaticContext::new());
        let request = builder.build(vec![Message::user("hello")], "");
        let system = request.system.expect("system prompt should exist");

        match system {
            SystemPrompt::Blocks(blocks) => {
                assert!(blocks.iter().any(|block| {
                    block.text.contains("Built-in Tools") && block.cache_control.is_some()
                }));
                assert!(blocks.iter().any(|block| {
                    !block.text.contains("Built-in Tools") && block.cache_control.is_none()
                }));
            }
            _ => panic!("expected system prompt blocks"),
        }
    }

    #[test]
    fn request_metadata_uses_session_identity() {
        let config = test_config();
        let tools = Arc::new(ToolRegistry::new());
        let metadata =
            RequestMetadata::from_identity(Some("tenant-a"), Some("user-1"), Some("session-1"));

        let builder = RequestBuilder::new(&config, tools, StaticContext::new()).metadata(metadata);
        let request = builder.build(vec![Message::user("hello")], "");
        let metadata = request
            .metadata
            .expect("request metadata should be present");

        assert_eq!(metadata.user_id.as_deref(), Some("user-1"));
        assert_eq!(
            metadata.extra.get("tenant_id"),
            Some(&serde_json::json!("tenant-a"))
        );
    }

    #[test]
    fn request_metadata_is_absent_without_principal() {
        let config = test_config();
        let tools = Arc::new(ToolRegistry::new());
        let builder = RequestBuilder::new(&config, tools, StaticContext::new()).metadata(
            RequestMetadata::from_identity(Some("tenant-a"), None, Some("session-1")),
        );
        let request = builder.build(vec![Message::user("hello")], "");

        assert!(request.metadata.is_none());
    }
}
