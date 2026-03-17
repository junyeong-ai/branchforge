//! SDK Core Tests
//!
//! Tests for core SDK features: authentication, client, agent builder, types,
//! tools (registry + execution), API types, MCP, and error handling.
//!
//! Run: cargo nextest run --test sdk_core_tests --all-features

use branchforge::ToolOutput;
use branchforge::tools::{
    BashTool, EditTool, ExecutionContext, GlobTool, GrepTool, ReadTool, Tool, WriteTool,
};
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;

fn create_test_context(temp_dir: &TempDir) -> ExecutionContext {
    ExecutionContext::from_path(std::fs::canonicalize(temp_dir.path()).unwrap())
        .unwrap_or_else(|_| ExecutionContext::permissive())
}

// =============================================================================
// Authentication
// =============================================================================

mod auth_tests {
    use branchforge::auth::{
        ChainProvider, Credential, CredentialProvider, EnvironmentProvider, ExplicitProvider,
        OAuthCredential,
    };
    use chrono::Utc;
    use secrecy::{ExposeSecret, SecretString};

    #[test]
    fn test_auth_api_key_credential() {
        let cred = Credential::api_key("sk-ant-api-test");
        assert!(!cred.is_expired());
        assert!(!cred.needs_refresh());
        assert_eq!(cred.credential_type(), "api_key");
    }

    #[test]
    fn test_auth_oauth_credential() {
        let cred = Credential::oauth("sk-ant-oat01-test");
        assert_eq!(cred.credential_type(), "oauth");
        match cred {
            Credential::OAuth(oauth) => {
                assert_eq!(oauth.access_token.expose_secret(), "sk-ant-oat01-test");
            }
            _ => panic!("Expected OAuth credential"),
        }
    }

    #[test]
    fn test_auth_credential_factory_methods() {
        let api_key = Credential::api_key("test-key");
        assert!(matches!(api_key, Credential::ApiKey(_)));

        let oauth = Credential::oauth("test-token");
        assert!(matches!(oauth, Credential::OAuth(_)));
    }

    #[test]
    fn test_auth_expired_credential_detection() {
        let expired = OAuthCredential {
            access_token: SecretString::from("token"),
            refresh_token: None,
            expires_at: Some(0),
            scopes: vec![],
            subscription_type: None,
        };
        assert!(expired.is_expired());
        assert!(expired.needs_refresh());
    }

    #[test]
    fn test_auth_valid_credential() {
        let future_time = Utc::now().timestamp() + 7200;
        let valid = OAuthCredential {
            access_token: SecretString::from("token"),
            refresh_token: None,
            expires_at: Some(future_time),
            scopes: vec![],
            subscription_type: None,
        };
        assert!(!valid.is_expired());
        assert!(!valid.needs_refresh());
    }

    #[test]
    fn test_auth_soon_to_expire_credential() {
        let soon = Utc::now().timestamp() + 60;
        let expiring = OAuthCredential {
            access_token: SecretString::from("token"),
            refresh_token: None,
            expires_at: Some(soon),
            scopes: vec![],
            subscription_type: None,
        };
        assert!(!expiring.is_expired());
        assert!(expiring.needs_refresh());
    }

    #[test]
    fn test_auth_no_expiry_credential() {
        let no_expiry = OAuthCredential {
            access_token: SecretString::from("token"),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
            subscription_type: None,
        };
        assert!(!no_expiry.is_expired());
        assert!(!no_expiry.needs_refresh());
    }

    #[test]
    fn test_auth_credential_wrapping_expired() {
        let cred = Credential::OAuth(OAuthCredential {
            access_token: SecretString::from("test"),
            refresh_token: None,
            expires_at: Some(0),
            scopes: vec![],
            subscription_type: None,
        });
        assert!(cred.is_expired());
        assert!(cred.needs_refresh());
    }

    #[tokio::test]
    async fn test_auth_explicit_provider() {
        let provider = ExplicitProvider::api_key("test-key");
        assert_eq!(provider.name(), "explicit");
        let cred = provider.resolve().await.unwrap();
        assert!(matches!(&cred, Credential::ApiKey(k) if k.expose_secret() == "test-key"));
    }

    #[tokio::test]
    async fn test_auth_environment_provider() {
        // SAFETY: tokio::test defaults to current_thread (single-threaded) runtime,
        // so no other threads are concurrently reading environment variables.
        unsafe { std::env::set_var("TEST_AUTH_KEY", "env-test-key") };
        let provider = EnvironmentProvider::from_var("TEST_AUTH_KEY");
        assert_eq!(provider.name(), "environment");
        let cred = provider.resolve().await.unwrap();
        assert!(matches!(&cred, Credential::ApiKey(k) if k.expose_secret() == "env-test-key"));
        unsafe { std::env::remove_var("TEST_AUTH_KEY") };
    }

    #[tokio::test]
    async fn test_auth_chain_provider() {
        let chain = ChainProvider::new(vec![]).provider(ExplicitProvider::api_key("chain-key"));
        assert_eq!(chain.name(), "chain");
        let cred = chain.resolve().await.unwrap();
        assert!(matches!(&cred, Credential::ApiKey(k) if k.expose_secret() == "chain-key"));
    }
}

// =============================================================================
// Client Builder & Config
// =============================================================================

mod client_tests {
    use branchforge::client::{DEFAULT_SMALL_MODEL, GatewayConfig, ModelConfig, ProviderConfig};
    use branchforge::{Auth, BetaConfig, BetaFeature, Client, OAuthConfig};

    #[tokio::test]
    async fn test_client_builder() {
        let models = ModelConfig::new("claude-sonnet-4-5-20250514", DEFAULT_SMALL_MODEL);
        let config = ProviderConfig::new(models).max_tokens(4096);
        let client = Client::builder()
            .auth("test-key")
            .await
            .expect("Auth failed")
            .config(config)
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .await;
        assert!(client.is_ok());
        assert_eq!(client.unwrap().config().max_tokens, 4096);
    }

    #[tokio::test]
    async fn test_client_custom_base_url() {
        let gateway = GatewayConfig::base_url("https://custom.api.com/v1");
        let client = Client::builder()
            .auth("test-key")
            .await
            .expect("Auth failed")
            .gateway(gateway)
            .build()
            .await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_client_oauth_token() {
        let client = Client::builder()
            .auth(Auth::oauth("sk-ant-oat01-test-token"))
            .await
            .expect("Auth failed")
            .build()
            .await;
        assert!(client.is_ok());
        assert_eq!(client.unwrap().adapter().name(), "anthropic");
    }

    #[tokio::test]
    async fn test_client_auto_resolve_builder() {
        let result = Client::builder().auth(Auth::FromEnv).await;
        if std::env::var("ANTHROPIC_API_KEY").is_err() {
            assert!(result.is_err());
        } else {
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_client_builder_with_credential() {
        let cred = branchforge::Credential::api_key("test-key");
        let _builder = Client::builder().auth(cred).await.expect("Auth failed");
    }

    #[tokio::test]
    async fn test_client_builder_with_oauth_config() {
        let config = OAuthConfig::builder().user_agent("test-agent/1.0").build();
        let _builder = Client::builder()
            .auth(Auth::oauth("test-token"))
            .await
            .expect("Auth failed")
            .oauth_config(config);
    }

    #[test]
    fn test_client_oauth_default_values() {
        let config = OAuthConfig::default();
        assert!(config.user_agent.contains("claude-cli"));
        assert_eq!(config.app_identifier, "cli");
        assert!(config.url_params.contains_key("beta"));
    }

    #[test]
    fn test_client_oauth_builder() {
        let config = OAuthConfig::builder()
            .user_agent("test-agent/1.0")
            .app_identifier("test-app")
            .url_param("custom", "value")
            .header("X-Custom", "header")
            .build();
        assert_eq!(config.user_agent, "test-agent/1.0");
        assert_eq!(config.app_identifier, "test-app");
        assert!(config.url_params.contains_key("custom"));
        assert!(config.extra_headers.contains_key("X-Custom"));
    }

    #[test]
    fn test_client_beta_config_with_features() {
        let config = BetaConfig::new()
            .feature(BetaFeature::InterleavedThinking)
            .feature(BetaFeature::ContextManagement);
        let header = config.header_value().unwrap();
        assert!(header.contains("interleaved-thinking"));
        assert!(header.contains("context-management"));
    }

    #[test]
    fn test_client_beta_config_custom_flag() {
        let config = BetaConfig::new().custom("custom-flag-2025");
        let header = config.header_value().unwrap();
        assert!(header.contains("custom-flag-2025"));
    }

    #[test]
    fn test_client_provider_config_beta() {
        let config = ProviderConfig::default().beta_config(
            BetaConfig::new()
                .feature(BetaFeature::InterleavedThinking)
                .custom("experimental-2026"),
        );
        assert!(config.beta.has(BetaFeature::InterleavedThinking));
        let header = config.beta.header_value().unwrap();
        assert!(header.contains("experimental-2026"));
    }

    #[tokio::test]
    async fn test_client_builder_anthropic_method() {
        let _builder = Client::builder()
            .auth("test-key")
            .await
            .expect("Auth failed")
            .anthropic();
    }
}

// =============================================================================
// Cloud Providers
// =============================================================================

mod cloud_provider_tests {
    use branchforge::client::CloudProvider;

    #[test]
    fn test_cloud_provider_default() {
        assert_eq!(CloudProvider::default(), CloudProvider::Anthropic);
    }

    #[test]
    fn test_cloud_provider_from_env() {
        let provider = CloudProvider::from_env();
        assert_eq!(provider, CloudProvider::Anthropic);
    }

    #[test]
    fn test_cloud_provider_anthropic_models() {
        let models = CloudProvider::Anthropic.default_models();
        assert!(models.primary.contains("claude"));
    }

    #[test]
    fn test_cloud_provider_equality() {
        assert_eq!(CloudProvider::Anthropic, CloudProvider::Anthropic);
        #[cfg(feature = "aws")]
        {
            assert_eq!(CloudProvider::Bedrock, CloudProvider::Bedrock);
            assert_ne!(CloudProvider::Bedrock, CloudProvider::Anthropic);
        }
        #[cfg(feature = "gcp")]
        {
            assert_eq!(CloudProvider::Vertex, CloudProvider::Vertex);
            assert_ne!(CloudProvider::Vertex, CloudProvider::Anthropic);
        }
        #[cfg(feature = "azure")]
        {
            assert_eq!(CloudProvider::Foundry, CloudProvider::Foundry);
            assert_ne!(CloudProvider::Foundry, CloudProvider::Anthropic);
        }
    }

    #[cfg(feature = "aws")]
    #[test]
    fn test_cloud_provider_bedrock_models() {
        let models = CloudProvider::Bedrock.default_models();
        assert!(models.primary.contains("global.anthropic"));
    }

    #[cfg(feature = "gcp")]
    #[test]
    fn test_cloud_provider_vertex_models() {
        let models = CloudProvider::Vertex.default_models();
        assert!(models.primary.contains("@"));
    }
}

// =============================================================================
// API Types
// =============================================================================

mod api_types_tests {
    use branchforge::client::messages::RequestMetadata;
    use branchforge::client::{
        ContextManagement, CreateMessageRequest, OutputFormat, ThinkingConfig,
    };
    use branchforge::types::{
        ContentBlock, Message, Role, StopReason, ToolDefinition, ToolResultBlock, Usage,
    };

    #[test]
    fn test_types_message_structure() {
        let user_msg = Message::user("Hello");
        assert!(matches!(user_msg.role, Role::User));
        let assistant_msg = Message::assistant("Hi!");
        assert!(matches!(assistant_msg.role, Role::Assistant));
    }

    #[test]
    fn test_types_content_blocks() {
        let text = ContentBlock::text("Hello");
        assert!(matches!(text, ContentBlock::Text { .. }));
        let tool_result = ToolResultBlock::success("tool-id", "result");
        assert!(tool_result.is_error.is_none() || tool_result.is_error == Some(false));
    }

    #[test]
    fn test_types_tool_definition() {
        let def = ToolDefinition::new(
            "Read",
            "Read files",
            serde_json::json!({
                "type": "object",
                "properties": { "file_path": {"type": "string"} },
                "required": ["file_path"]
            }),
        );
        assert_eq!(def.name, "Read");
    }

    #[test]
    fn test_types_usage_calculation() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(10),
            cache_read_input_tokens: Some(5),
            server_tool_use: None,
        };
        assert_eq!(usage.total(), 150);
    }

    #[test]
    fn test_types_stop_reasons() {
        let json = serde_json::to_value(StopReason::EndTurn).unwrap();
        assert_eq!(json, "end_turn");
        let json = serde_json::to_value(StopReason::ToolUse).unwrap();
        assert_eq!(json, "tool_use");
        let json = serde_json::to_value(StopReason::MaxTokens).unwrap();
        assert_eq!(json, "max_tokens");
        let deserialized: StopReason = serde_json::from_str("\"end_turn\"").unwrap();
        assert_eq!(deserialized, StopReason::EndTurn);
    }

    #[test]
    fn test_api_create_message_request() {
        let request = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("Hello")])
            .max_tokens(1000)
            .temperature(0.7);
        assert_eq!(request.model, "claude-sonnet-4-5");
        assert_eq!(request.max_tokens, 1000);
    }

    #[test]
    fn test_api_extended_thinking_config() {
        let thinking = ThinkingConfig::enabled(10000);
        assert_eq!(thinking.budget_tokens, Some(10000));
        let disabled = ThinkingConfig::disabled();
        assert!(disabled.budget_tokens.is_none());
    }

    #[test]
    fn test_api_structured_output() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "name": {"type": "string"} }
        });
        let format = OutputFormat::json_schema(schema);
        let request =
            CreateMessageRequest::new("model", vec![Message::user("Hi")]).output_format(format);
        assert!(request.output_format.is_some());
    }

    #[test]
    fn test_api_context_management() {
        let management = ContextManagement::new()
            .edit(ContextManagement::clear_tool_uses())
            .edit(ContextManagement::clear_thinking(1));
        assert_eq!(management.edits.len(), 2);
    }

    #[test]
    fn test_api_metadata_generation() {
        let metadata =
            RequestMetadata::from_identity(Some("tenant-a"), Some("user-1"), Some("session-1"))
                .expect("metadata should exist");
        assert_eq!(metadata.user_id.as_deref(), Some("user-1"));
        assert_eq!(metadata.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(metadata.session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn test_api_metadata_absent_without_principal() {
        let metadata = RequestMetadata::from_identity(Some("tenant-a"), None, Some("session-1"));
        assert!(metadata.is_none());
    }
}

// =============================================================================
// Error Types
// =============================================================================

mod error_tests {
    use branchforge::Error;

    #[test]
    fn test_error_types() {
        let api_error = Error::Api {
            message: "Invalid API key".to_string(),
            status: Some(401),
            error_type: None,
        };
        assert!(api_error.to_string().contains("Invalid API key"));

        let tool_error = Error::Tool(branchforge::types::ToolError::not_found("/test/file.txt"));
        assert!(tool_error.to_string().contains("not found"));

        let rate_limit = Error::RateLimit {
            retry_after: Some(std::time::Duration::from_secs(60)),
        };
        assert!(rate_limit.to_string().contains("Rate limit"));

        let context_overflow = Error::ContextOverflow {
            current: 250_000,
            max: 200_000,
        };
        assert!(context_overflow.to_string().contains("Context limit"));
    }
}

// =============================================================================
// Agent Builder
// =============================================================================

mod agent_builder_tests {
    use branchforge::{Agent, AgentEvent, ToolSurface};

    #[tokio::test]
    async fn test_agent_builder_pattern() {
        let agent_result = Agent::builder()
            .auth("test-api-key")
            .await
            .expect("Auth failed")
            .model("claude-sonnet-4-5-20250514")
            .tools(ToolSurface::all())
            .working_dir(".")
            .max_tokens(4096)
            .max_iterations(10)
            .system_prompt("Custom system prompt")
            .build()
            .await;
        assert!(agent_result.is_ok());
    }

    #[test]
    fn test_agent_tool_surface_modes() {
        let access = ToolSurface::all();
        assert!(access.is_allowed("Read"));
        assert!(access.is_allowed("Bash"));

        let access = ToolSurface::none();
        assert!(!access.is_allowed("Read"));

        let access =
            ToolSurface::only(["Read".to_string(), "Glob".to_string(), "Grep".to_string()]);
        assert!(access.is_allowed("Read"));
        assert!(!access.is_allowed("Bash"));

        let access = ToolSurface::except(["Bash".to_string()]);
        assert!(access.is_allowed("Read"));
        assert!(!access.is_allowed("Bash"));
    }

    #[test]
    fn test_agent_events() {
        let text_event = AgentEvent::Text {
            delta: "Hello".to_string(),
        };
        let tool_complete = AgentEvent::ToolComplete {
            id: "id1".to_string(),
            name: "Read".to_string(),
            output: "file contents".to_string(),
            is_error: false,
            duration_ms: 50,
        };
        let tool_blocked = AgentEvent::ToolBlocked {
            id: "id2".to_string(),
            name: "Bash".to_string(),
            reason: "Authorization denied".to_string(),
        };
        assert!(matches!(text_event, AgentEvent::Text { .. }));
        assert!(matches!(tool_complete, AgentEvent::ToolComplete { .. }));
        assert!(matches!(tool_blocked, AgentEvent::ToolBlocked { .. }));
    }
}

// =============================================================================
// Tool Registry
// =============================================================================

mod tool_registry_tests {
    use branchforge::{ToolRegistry, ToolSurface};

    #[test]
    fn test_registry_all_tools_registered() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        let expected = [
            "Read",
            "Write",
            "Edit",
            "Glob",
            "Grep",
            "Bash",
            "KillShell",
            "Task",
            "TaskOutput",
            "TodoWrite",
            "Plan",
            "Skill",
        ];
        for tool in expected {
            assert!(registry.contains(tool), "Missing tool: {}", tool);
        }
    }

    #[test]
    fn test_registry_tool_definitions_count() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        let definitions = registry.definitions();
        assert_eq!(definitions.len(), 12);
        for def in &definitions {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
        }
    }

    #[test]
    fn test_registry_tool_surface_filtering() {
        let registry =
            ToolRegistry::default_tools(ToolSurface::only(["Read", "Write"]), None, None);
        assert!(registry.contains("Read"));
        assert!(registry.contains("Write"));
        assert!(!registry.contains("Bash"));
    }

    #[test]
    fn test_registry_tool_surface_except() {
        let registry = ToolRegistry::default_tools(ToolSurface::except(["Bash"]), None, None);
        assert!(registry.contains("Read"));
        assert!(!registry.contains("Bash"));
    }

    #[test]
    fn test_registry_tool_surface_none() {
        let registry = ToolRegistry::default_tools(ToolSurface::None, None, None);
        assert_eq!(registry.names().len(), 0);
    }

    #[test]
    fn test_registry_definitions_have_required_fields() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        for def in registry.definitions() {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
            assert!(def.input_schema.is_object());
        }
    }

    #[test]
    fn test_registry_read_tool_schema() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        let read_tool = registry.get("Read").expect("Read tool should exist");
        let def = read_tool.definition();
        let props = def
            .input_schema
            .get("properties")
            .expect("Should have properties");
        assert!(props.get("file_path").is_some());
        let required = def
            .input_schema
            .get("required")
            .expect("Should have required");
        assert!(
            required
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("file_path"))
        );
    }

    #[test]
    fn test_registry_bash_tool_schema() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        let bash_tool = registry.get("Bash").expect("Bash tool should exist");
        let def = bash_tool.definition();
        let props = def
            .input_schema
            .get("properties")
            .expect("Should have properties");
        assert!(props.get("command").is_some());
        assert!(props.get("timeout").is_some());
        let required = def
            .input_schema
            .get("required")
            .expect("Should have required");
        assert!(
            required
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("command"))
        );
    }

    #[test]
    fn test_registry_edit_tool_schema() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        let edit_tool = registry.get("Edit").expect("Edit tool should exist");
        let def = edit_tool.definition();
        let props = def
            .input_schema
            .get("properties")
            .expect("Should have properties");
        assert!(props.get("file_path").is_some());
        assert!(props.get("old_string").is_some());
        assert!(props.get("new_string").is_some());
        let required = def
            .input_schema
            .get("required")
            .expect("Should have required");
        let req_arr = required.as_array().unwrap();
        assert!(req_arr.contains(&serde_json::json!("file_path")));
        assert!(req_arr.contains(&serde_json::json!("old_string")));
        assert!(req_arr.contains(&serde_json::json!("new_string")));
    }

    #[test]
    fn test_registry_write_tool_schema() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        let tool = registry.get("Write").expect("Write tool should exist");
        let def = tool.definition();
        let props = def
            .input_schema
            .get("properties")
            .expect("Should have properties");
        assert!(props.get("file_path").is_some());
        assert!(props.get("content").is_some());
        let required = def
            .input_schema
            .get("required")
            .expect("Should have required");
        let req_arr = required.as_array().unwrap();
        assert!(req_arr.contains(&serde_json::json!("file_path")));
        assert!(req_arr.contains(&serde_json::json!("content")));
    }

    #[test]
    fn test_registry_glob_tool_schema() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        let tool = registry.get("Glob").expect("Glob tool should exist");
        let def = tool.definition();
        let props = def
            .input_schema
            .get("properties")
            .expect("Should have properties");
        assert!(props.get("pattern").is_some());
        assert!(props.get("path").is_some());
        let required = def
            .input_schema
            .get("required")
            .expect("Should have required");
        let req_arr = required.as_array().unwrap();
        assert!(req_arr.contains(&serde_json::json!("pattern")));
    }

    #[test]
    fn test_registry_grep_tool_schema() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        let tool = registry.get("Grep").expect("Grep tool should exist");
        let def = tool.definition();
        let props = def
            .input_schema
            .get("properties")
            .expect("Should have properties");
        assert!(props.get("pattern").is_some());
        assert!(props.get("path").is_some());
        assert!(props.get("output_mode").is_some());
        let required = def
            .input_schema
            .get("required")
            .expect("Should have required");
        let req_arr = required.as_array().unwrap();
        assert!(req_arr.contains(&serde_json::json!("pattern")));
    }

    #[test]
    fn test_registry_tool_descriptions_reasonable_size() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        for def in registry.definitions() {
            let desc_len = def.description.len();
            let max_len = match def.name.as_str() {
                "TodoWrite" => 12000,
                "Bash" => 6000,
                "Plan" => 4000,
                _ => 4000,
            };
            assert!(desc_len > 20, "Tool {} description too short", def.name);
            assert!(
                desc_len < max_len,
                "Tool {} description too long: {}",
                def.name,
                desc_len
            );
        }
    }

    #[test]
    fn test_registry_input_schemas_complete() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        for def in registry.definitions() {
            let schema = &def.input_schema;
            assert!(schema.get("type").is_some(), "{} missing type", def.name);
            assert_eq!(schema["type"], "object", "{} type must be object", def.name);
            assert!(
                schema.get("properties").is_some(),
                "{} missing properties",
                def.name
            );
        }
    }

    #[test]
    fn test_registry_tool_count_reasonable() {
        let registry = ToolRegistry::default_tools(ToolSurface::All, None, None);
        let tool_count = registry.names().len();
        assert!(tool_count >= 5);
        assert!(tool_count <= 20);
    }
}

// =============================================================================
// Individual Tool Execution
// =============================================================================

mod tool_execution_tests {
    use super::*;
    use branchforge::session::SessionId;
    use branchforge::session::ToolState;
    use branchforge::tools::TodoWriteTool;

    #[tokio::test]
    async fn test_tool_read_basic() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n").unwrap();
        let tool = ReadTool;
        let ctx = create_test_context(&temp_dir);
        let result = tool
            .execute(json!({"file_path": file_path.to_str().unwrap()}), &ctx)
            .await;
        match &result.output {
            ToolOutput::Success(content) => {
                assert!(content.contains("Line 1"));
                assert!(content.contains("Line 5"));
            }
            other => panic!("Expected success, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_tool_read_with_offset_limit() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, "Line 1\nLine 2\nLine 3\nLine 4").unwrap();
        let tool = ReadTool;
        let ctx = ExecutionContext::permissive();
        let result = tool
            .execute(
                json!({"file_path": file_path.to_str().unwrap(), "offset": 1, "limit": 2}),
                &ctx,
            )
            .await;
        match &result.output {
            ToolOutput::Success(content) => {
                assert!(!content.contains("Line 1"));
                assert!(content.contains("Line 2"));
                assert!(content.contains("Line 3"));
                assert!(!content.contains("Line 4"));
            }
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_tool_write_basic() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("new_file.txt");
        let tool = WriteTool;
        let ctx = create_test_context(&temp_dir);
        let result = tool
            .execute(
                json!({"file_path": file_path.to_str().unwrap(), "content": "Hello, World!"}),
                &ctx,
            )
            .await;
        assert!(!result.is_error());
        assert!(file_path.exists());
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "Hello, World!"
        );
    }

    #[tokio::test]
    async fn test_tool_write_creates_directories() {
        let temp_dir = TempDir::new().unwrap();
        let canonical_root = std::fs::canonicalize(temp_dir.path()).unwrap();
        let file_path = canonical_root.join("deep/nested/dir/file.txt");
        let tool = WriteTool;
        let ctx = create_test_context(&temp_dir);
        let result = tool
            .execute(
                json!({"file_path": file_path.to_str().unwrap(), "content": "Nested content"}),
                &ctx,
            )
            .await;
        assert!(!result.is_error());
        assert!(file_path.exists());
    }

    #[tokio::test]
    async fn test_tool_edit_single_replacement() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("edit_test.txt");
        std::fs::write(&file_path, "hello world").unwrap();
        let tool = EditTool;
        let ctx = create_test_context(&temp_dir);
        let result = tool.execute(json!({"file_path": file_path.to_str().unwrap(), "old_string": "hello", "new_string": "hi"}), &ctx).await;
        assert!(!result.is_error());
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "hi world");
    }

    #[tokio::test]
    async fn test_tool_edit_replace_all() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("edit_test.txt");
        std::fs::write(&file_path, "foo bar foo baz").unwrap();
        let tool = EditTool;
        let ctx = create_test_context(&temp_dir);
        let result = tool.execute(json!({"file_path": file_path.to_str().unwrap(), "old_string": "foo", "new_string": "qux", "replace_all": true}), &ctx).await;
        assert!(!result.is_error());
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "qux bar qux baz"
        );
    }

    #[tokio::test]
    async fn test_tool_edit_duplicate_string_error() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("edit_test.txt");
        std::fs::write(&file_path, "foo bar foo baz").unwrap();
        let tool = EditTool;
        let ctx = create_test_context(&temp_dir);
        let result = tool.execute(json!({"file_path": file_path.to_str().unwrap(), "old_string": "foo", "new_string": "qux"}), &ctx).await;
        assert!(
            result.is_error(),
            "Edit should fail when old_string is not unique"
        );
    }

    #[tokio::test]
    async fn test_tool_glob_pattern_matching() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("test1.rs"), "").unwrap();
        std::fs::write(temp_dir.path().join("test2.rs"), "").unwrap();
        std::fs::write(temp_dir.path().join("test.txt"), "").unwrap();
        std::fs::create_dir(temp_dir.path().join("subdir")).unwrap();
        std::fs::write(temp_dir.path().join("subdir/nested.rs"), "").unwrap();
        let tool = GlobTool;
        let ctx = create_test_context(&temp_dir);
        let result = tool
            .execute(
                json!({"pattern": "*.rs", "path": temp_dir.path().to_str().unwrap()}),
                &ctx,
            )
            .await;
        match &result.output {
            ToolOutput::Success(output) => {
                assert!(output.contains("test1.rs"));
                assert!(output.contains("test2.rs"));
                assert!(!output.contains("test.txt"));
            }
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_tool_glob_recursive() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join("subdir")).unwrap();
        std::fs::write(temp_dir.path().join("subdir/nested.rs"), "").unwrap();
        let tool = GlobTool;
        let ctx = create_test_context(&temp_dir);
        let result = tool
            .execute(
                json!({"pattern": "**/*.rs", "path": temp_dir.path().to_str().unwrap()}),
                &ctx,
            )
            .await;
        match &result.output {
            ToolOutput::Success(output) => assert!(output.contains("nested.rs")),
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_tool_grep_basic() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join("search.txt"),
            "Hello World\nfoo bar\nHello Again\n",
        )
        .unwrap();
        let tool = GrepTool;
        let ctx = create_test_context(&temp_dir);
        let result = tool.execute(json!({"pattern": "Hello", "path": temp_dir.path().to_str().unwrap(), "output_mode": "content"}), &ctx).await;
        match &result.output {
            ToolOutput::Success(output) => {
                assert!(output.contains("Hello World"));
                assert!(output.contains("Hello Again"));
            }
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_tool_grep_case_insensitive() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("search.txt"), "Hello World\nfoo bar\n").unwrap();
        let tool = GrepTool;
        let ctx = create_test_context(&temp_dir);
        let result = tool.execute(json!({"pattern": "hello", "path": temp_dir.path().to_str().unwrap(), "output_mode": "content", "-i": true}), &ctx).await;
        match &result.output {
            ToolOutput::Success(output) => assert!(output.contains("Hello")),
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_tool_bash_basic() {
        let temp_dir = TempDir::new().unwrap();
        let tool = BashTool::new();
        let ctx = create_test_context(&temp_dir);
        let result = tool
            .execute(
                json!({"command": "echo 'Hello from Bash'", "description": "Echo test"}),
                &ctx,
            )
            .await;
        match &result.output {
            ToolOutput::Success(output) => assert!(output.contains("Hello from Bash")),
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_tool_bash_with_timeout() {
        let temp_dir = TempDir::new().unwrap();
        let tool = BashTool::new();
        let ctx = create_test_context(&temp_dir);
        let result = tool
            .execute(json!({"command": "echo 'done'", "timeout": 5000}), &ctx)
            .await;
        match &result.output {
            ToolOutput::Success(output) => assert!(output.contains("done")),
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_tool_bash_background() {
        let process_manager = Arc::new(branchforge::tools::ProcessManager::new());
        let tool = BashTool::process_manager(process_manager);
        let ctx = ExecutionContext::default();
        let result = tool
            .execute(
                json!({"command": "echo 'background'", "run_in_background": true}),
                &ctx,
            )
            .await;
        match &result.output {
            ToolOutput::Success(content) => assert!(content.contains("Background process started")),
            _ => panic!("Expected success"),
        }
    }

    #[tokio::test]
    async fn test_tool_todo_write() {
        let temp_dir = TempDir::new().unwrap();
        let session_id = SessionId::new();
        let session_ctx = ToolState::new(session_id);
        let tool = TodoWriteTool::new(session_ctx, session_id);
        let ctx = create_test_context(&temp_dir);
        let result = tool.execute(json!({
            "todos": [
                {"content": "First task", "status": "pending", "activeForm": "Working on first task"},
                {"content": "Second task", "status": "in_progress", "activeForm": "Working on second task"}
            ]
        }), &ctx).await;
        assert!(!result.is_error());
    }

    #[tokio::test]
    async fn test_tool_unknown_tool_error() {
        let registry =
            branchforge::ToolRegistry::default_tools(branchforge::ToolSurface::All, None, None);
        let result = registry.execute("NonexistentTool", json!({})).await;
        assert!(result.is_error());
        match &result.output {
            ToolOutput::Error(e) => assert!(e.to_string().contains("unknown tool")),
            _ => panic!("Expected error"),
        }
    }

    #[tokio::test]
    async fn test_tool_read_nonexistent_file() {
        let registry = branchforge::ToolRegistry::default_tools(
            branchforge::ToolSurface::All,
            Some(std::path::PathBuf::from("/tmp")),
            Some(branchforge::ToolPolicy::permissive()),
        );
        let result = registry
            .execute(
                "Read",
                json!({"file_path": "/nonexistent/path/to/file.txt"}),
            )
            .await;
        assert!(result.is_error());
    }
}

// =============================================================================
// Server Tool Config
// =============================================================================

mod server_tool_tests {
    use branchforge::tools::{WebFetchTool, WebSearchTool};

    #[test]
    fn test_web_fetch_tool_config() {
        let tool = WebFetchTool::new()
            .max_uses(10)
            .allowed_domains(vec!["example.com".to_string()])
            .citations(true);
        assert_eq!(tool.tool_type, "web_fetch_20250910");
        assert_eq!(tool.max_uses, Some(10));
        assert!(tool.allowed_domains.is_some());
        assert!(tool.citations.is_some());
    }

    #[test]
    fn test_web_search_tool_config() {
        let tool = WebSearchTool::new()
            .max_uses(5)
            .blocked_domains(vec!["spam.com".to_string()]);
        assert_eq!(tool.tool_type, "web_search_20250305");
        assert_eq!(tool.max_uses, Some(5));
        assert!(tool.blocked_domains.is_some());
    }
}

// =============================================================================
// Integration Scenarios
// =============================================================================

mod integration_scenarios {
    use super::*;

    #[tokio::test]
    async fn test_scenario_file_workflow() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("workflow.txt");
        let write_tool = WriteTool;
        let read_tool = ReadTool;
        let edit_tool = EditTool;
        let ctx = create_test_context(&temp_dir);

        let result = write_tool.execute(json!({"file_path": file_path.to_str().unwrap(), "content": "function hello() {\n  console.log('Hello');\n}"}), &ctx).await;
        assert!(!result.is_error());

        let result = read_tool
            .execute(json!({"file_path": file_path.to_str().unwrap()}), &ctx)
            .await;
        match &result.output {
            ToolOutput::Success(content) => assert!(content.contains("function hello")),
            _ => panic!("Read should succeed"),
        }

        let result = edit_tool.execute(json!({"file_path": file_path.to_str().unwrap(), "old_string": "Hello", "new_string": "World"}), &ctx).await;
        assert!(!result.is_error());
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("World"));
    }

    #[tokio::test]
    async fn test_scenario_code_search() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join("src")).unwrap();
        std::fs::write(
            temp_dir.path().join("src/main.rs"),
            "fn main() {\n    println!(\"Hello\");\n}",
        )
        .unwrap();
        std::fs::write(
            temp_dir.path().join("src/lib.rs"),
            "pub fn hello() {\n    println!(\"Hello from lib\");\n}",
        )
        .unwrap();
        let glob_tool = GlobTool;
        let grep_tool = GrepTool;
        let ctx = create_test_context(&temp_dir);

        let result = glob_tool
            .execute(
                json!({"pattern": "**/*.rs", "path": temp_dir.path().to_str().unwrap()}),
                &ctx,
            )
            .await;
        match &result.output {
            ToolOutput::Success(output) => {
                assert!(output.contains("main.rs"));
                assert!(output.contains("lib.rs"));
            }
            _ => panic!("Glob should succeed"),
        }

        let result = grep_tool.execute(json!({"pattern": "println!", "path": temp_dir.path().to_str().unwrap(), "output_mode": "content"}), &ctx).await;
        match &result.output {
            ToolOutput::Success(output) => assert!(output.contains("Hello")),
            _ => panic!("Grep should succeed"),
        }
    }
}

// =============================================================================
// MCP Integration
// =============================================================================

mod mcp_tests {
    use branchforge::mcp::{
        McpConnectionStatus, McpContent, McpServerConfig, McpServerState, McpToolResult,
    };
    use std::collections::HashMap;

    #[test]
    fn test_mcp_server_config() {
        let stdio_config = McpServerConfig::Stdio {
            command: "npx".to_string(),
            args: vec!["@modelcontextprotocol/server".to_string()],
            env: HashMap::new(),
            cwd: None,
        };
        let json = serde_json::to_string(&stdio_config).unwrap();
        assert!(json.contains("stdio"));
        assert!(json.contains("npx"));

        let sse_config = McpServerConfig::Sse {
            url: "https://sse.example.com".to_string(),
            headers: HashMap::new(),
        };
        let json = serde_json::to_string(&sse_config).unwrap();
        assert!(json.contains("sse"));
    }

    #[test]
    fn test_mcp_server_state() {
        let state = McpServerState::new(
            "test-server",
            McpServerConfig::Stdio {
                command: "test".to_string(),
                args: vec![],
                env: HashMap::new(),
                cwd: None,
            },
        );
        assert_eq!(state.name, "test-server");
        assert_eq!(state.status, McpConnectionStatus::Connecting);
        assert!(!state.is_connected());
    }

    #[test]
    fn test_mcp_content_types() {
        let text_content = McpContent::Text {
            text: "Hello".to_string(),
        };
        assert_eq!(text_content.as_text(), Some("Hello"));
        let image_content = McpContent::Image {
            data: "base64data".to_string(),
            mime_type: "image/png".to_string(),
        };
        assert_eq!(image_content.as_text(), None);
    }

    #[test]
    fn test_mcp_tool_result() {
        let result = McpToolResult {
            content: vec![
                McpContent::Text {
                    text: "Line 1".to_string(),
                },
                McpContent::Text {
                    text: "Line 2".to_string(),
                },
            ],
            is_error: false,
        };
        assert!(!result.is_error);
        assert_eq!(result.to_string_content(), "Line 1\nLine 2");
    }
}

// =============================================================================
// Prompt Caching
// =============================================================================

mod caching_tests {
    use branchforge::types::{
        CacheControl, CacheType, SystemBlock, SystemPrompt, TokenUsage, Usage,
    };

    #[test]
    fn test_cache_system_prompt_cached() {
        let prompt = SystemPrompt::cached("You are helpful");
        if let SystemPrompt::Blocks(blocks) = prompt {
            assert!(!blocks.is_empty());
            assert!(blocks[0].cache_control.is_some());
            assert_eq!(
                blocks[0].cache_control.as_ref().unwrap().cache_type,
                CacheType::Ephemeral
            );
        } else {
            panic!("Expected Blocks variant");
        }
    }

    #[test]
    fn test_cache_system_prompt_text() {
        let prompt = SystemPrompt::text("Simple prompt");
        if let SystemPrompt::Text(text) = prompt {
            assert_eq!(text, "Simple prompt");
        } else {
            panic!("Expected Text variant");
        }
    }

    #[test]
    fn test_cache_system_prompt_blocks() {
        let blocks = vec![SystemBlock {
            block_type: "text".to_string(),
            text: "Block content".to_string(),
            cache_control: None,
        }];
        let prompt = SystemPrompt::Blocks(blocks);
        match prompt {
            SystemPrompt::Blocks(b) => {
                assert_eq!(b.len(), 1);
                assert_eq!(b[0].text, "Block content");
            }
            _ => panic!("Expected Blocks variant"),
        }
    }

    #[test]
    fn test_cache_system_block() {
        let cached = SystemBlock::cached("Cached content");
        assert!(cached.cache_control.is_some());
        assert_eq!(
            cached.cache_control.unwrap().cache_type,
            CacheType::Ephemeral
        );
        assert_eq!(cached.block_type, "text");
        let uncached = SystemBlock::uncached("Uncached content");
        assert!(uncached.cache_control.is_none());
    }

    #[test]
    fn test_cache_control() {
        let ephemeral = CacheControl::ephemeral();
        assert_eq!(ephemeral.cache_type, CacheType::Ephemeral);
    }

    #[test]
    fn test_cache_usage_with_cache_fields() {
        let usage = Usage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_input_tokens: Some(800),
            cache_creation_input_tokens: Some(100),
            server_tool_use: None,
        };
        assert_eq!(usage.total(), 1500);

        let mut token_usage = TokenUsage::default();
        token_usage.add_usage(&usage);
        assert_eq!(token_usage.cache_read_input_tokens, 800);
        assert_eq!(token_usage.cache_creation_input_tokens, 100);
        let rate = token_usage.cache_hit_rate();
        assert!(rate > 0.0);
    }

    #[test]
    fn test_cache_hit_rate_calculation() {
        let usage = TokenUsage {
            input_tokens: 10000,
            output_tokens: 500,
            cache_read_input_tokens: 8000,
            cache_creation_input_tokens: 0,
            ..Default::default()
        };
        assert!((usage.cache_hit_rate() - 0.8).abs() < 0.01);
    }
}

// =============================================================================
// Live Tests (require CLI credentials)
// =============================================================================

#[cfg(feature = "cli-auth")]
mod live_tests {
    use branchforge::{Agent, Auth, Client, ToolSurface};
    use futures::StreamExt;
    use std::pin::pin;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    #[ignore = "Requires CLI credentials"]
    async fn test_live_cli_oauth_authentication() {
        let client = Client::builder()
            .auth(Auth::ClaudeCli)
            .await
            .expect("Failed to load CLI credentials")
            .build()
            .await
            .expect("Failed to build client");
        let response = client
            .query("Reply with exactly: AUTH_OK")
            .await
            .expect("Query failed");
        assert!(response.contains("AUTH_OK"));
    }

    #[tokio::test]
    #[ignore = "Requires CLI credentials"]
    async fn test_live_streaming() {
        let client = Client::builder()
            .auth(Auth::ClaudeCli)
            .await
            .expect("Failed to load CLI credentials")
            .build()
            .await
            .expect("Failed to build client");
        let stream = client
            .stream("Count from 1 to 3.")
            .await
            .expect("Stream failed");
        let mut stream = pin!(stream);
        let mut text_chunks = Vec::new();
        while let Some(item) = stream.next().await {
            text_chunks.push(item.expect("Stream error"));
        }
        assert!(!text_chunks.is_empty());
    }

    #[tokio::test]
    #[ignore = "Requires CLI credentials"]
    async fn test_live_agent_with_read_tool() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("data.json"), r#"{"count": 42}"#)
            .await
            .unwrap();
        let agent = Agent::builder()
            .auth(Auth::ClaudeCli)
            .await
            .expect("Failed to load CLI credentials")
            .tools(ToolSurface::only(["Read"]))
            .working_dir(dir.path())
            .max_iterations(5)
            .build()
            .await
            .expect("Failed to build agent");
        let result = agent
            .execute("Read data.json and tell me the count value")
            .await
            .expect("Agent failed");
        assert!(result.text().contains("42") || result.tool_calls > 0);
    }

    #[tokio::test]
    #[ignore = "Requires CLI credentials"]
    async fn test_live_agent_streaming() {
        let agent = Agent::builder()
            .auth(Auth::ClaudeCli)
            .await
            .expect("Failed to load CLI credentials")
            .tools(ToolSurface::none())
            .max_iterations(1)
            .build()
            .await
            .expect("Failed to build agent");
        let stream = agent
            .execute_stream("Say hello briefly.")
            .await
            .expect("Stream failed");
        let mut stream = pin!(stream);
        let mut has_text = false;
        let mut has_complete = false;
        while let Some(event) = stream.next().await {
            match event.expect("Event error") {
                branchforge::AgentEvent::Text { .. } => has_text = true,
                branchforge::AgentEvent::Complete(_) => has_complete = true,
                _ => {}
            }
        }
        assert!(has_text);
        assert!(has_complete);
    }
}
