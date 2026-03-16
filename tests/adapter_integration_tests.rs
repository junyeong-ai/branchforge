//! Integration tests for OpenAI and Gemini provider adapters.
//!
//! All tests use wiremock to mock HTTP responses, requiring no API keys.
//!
//! Run: cargo nextest run --test adapter_integration_tests --features openai,gemini

#[cfg(feature = "openai")]
mod openai_tests {
    use branchforge::client::StreamItem;
    use branchforge::client::adapter::{
        ModelConfig, OpenAiAdapter, ProviderAdapter, ProviderConfig,
    };
    use branchforge::client::messages::{CreateMessageRequest, OutputFormat};
    use branchforge::types::{Message, StopReason, ToolDefinition};
    use serde_json::{Value, json};
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_openai_config() -> ProviderConfig {
        ProviderConfig::new(ModelConfig::new("gpt-4o", "gpt-4o-mini"))
    }

    fn mock_openai_response(content: &str) -> Value {
        json!({
            "id": "chatcmpl-test-123",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 12,
                "completion_tokens": 8,
                "total_tokens": 20
            }
        })
    }

    fn mock_openai_tool_response() -> Value {
        json!({
            "id": "chatcmpl-tool-456",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"San Francisco\",\"unit\":\"celsius\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 25,
                "completion_tokens": 18,
                "total_tokens": 43
            }
        })
    }

    fn mock_openai_stream_body(chunks: &[&str]) -> String {
        let mut body = String::new();
        for chunk in chunks {
            body.push_str(&format!("data: {}\n\n", chunk));
        }
        body.push_str("data: [DONE]\n\n");
        body
    }

    fn mock_openai_error_response(message: &str, error_type: &str) -> Value {
        json!({
            "error": {
                "message": message,
                "type": error_type,
                "param": null,
                "code": null
            }
        })
    }

    #[tokio::test]
    async fn test_openai_send_simple_message() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("Authorization", "Bearer test-key-123"))
            .and(header("Content-Type", "application/json"))
            .and(body_partial_json(json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "Hello"}]
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(mock_openai_response("Hello! How can I help you?")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter = OpenAiAdapter::from_api_key(test_openai_config(), "test-key-123")
            .base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Hello")]);

        let response = adapter.send(&http, request).await.unwrap();

        assert_eq!(response.id, "chatcmpl-test-123");
        assert_eq!(response.model, "gpt-4o");
        assert_eq!(response.text(), "Hello! How can I help you?");
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(response.usage.input_tokens, 12);
        assert_eq!(response.usage.output_tokens, 8);
    }

    #[tokio::test]
    async fn test_openai_send_with_tools() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_openai_tool_response()))
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();

        let tool = ToolDefinition::new(
            "get_weather",
            "Get weather for a location",
            json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"},
                    "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}
                },
                "required": ["location"]
            }),
        );
        let request =
            CreateMessageRequest::new("gpt-4o", vec![Message::user("What's the weather in SF?")])
                .tools(vec![tool]);

        let response = adapter.send(&http, request).await.unwrap();

        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        let tool_uses = response.tool_uses();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "get_weather");
        assert_eq!(tool_uses[0].id, "call_abc123");
        assert_eq!(tool_uses[0].input["location"], "San Francisco");
        assert_eq!(tool_uses[0].input["unit"], "celsius");
    }

    #[tokio::test]
    async fn test_openai_send_verifies_request_body() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_openai_response("Response")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Test")])
            .max_tokens(2048)
            .temperature(0.5);

        // Use transform_request to inspect the serialized body
        let body = adapter.transform_request(request.clone()).await.unwrap();
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["max_completion_tokens"], 2048);
        assert_eq!(body["temperature"], 0.5);
        assert!(body["messages"].is_array());
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "Test");
        // stream should not be set for non-streaming
        assert!(body.get("stream").is_none());

        let _ = adapter.send(&http, request).await.unwrap();
    }

    #[tokio::test]
    async fn test_openai_streaming() {
        let server = MockServer::start().await;

        let chunks = vec![
            r#"{"id":"chatcmpl-stream","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-stream","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-stream","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-stream","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
        ];

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(mock_openai_stream_body(&chunks)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Hello")]);

        let response = adapter.send_stream(&http, request).await.unwrap();

        // Parse the SSE stream using the adapter's event parser
        use branchforge::client::StreamParser;
        use futures::StreamExt;

        let stream = StreamParser::with_event_parser(response.bytes_stream(), {
            let adapter_ref = OpenAiAdapter::from_api_key(test_openai_config(), "test-key");
            move |json| adapter_ref.parse_stream_event(json)
        });

        let items: Vec<StreamItem> = stream
            .filter_map(|item| async move { item.ok() })
            .collect()
            .await;

        let text_items: Vec<&str> = items
            .iter()
            .filter_map(|item| match item {
                StreamItem::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(text_items, vec!["Hello", " world"]);
    }

    #[tokio::test]
    async fn test_openai_streaming_tool_call() {
        let server = MockServer::start().await;

        // OpenAI streams tool calls with chunked function arguments
        let chunks = vec![
            r#"{"id":"chatcmpl-tc","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_xyz","type":"function","function":{"name":"read_file","arguments":""}}]},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-tc","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-tc","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"test.rs\"}"}}]},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-tc","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(mock_openai_stream_body(&chunks)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Read test.rs")]);

        let response = adapter.send_stream(&http, request).await.unwrap();

        use branchforge::client::StreamParser;
        use futures::StreamExt;

        let stream = StreamParser::with_event_parser(response.bytes_stream(), {
            let adapter_ref = OpenAiAdapter::from_api_key(test_openai_config(), "test-key");
            move |json| adapter_ref.parse_stream_event(json)
        });

        let items: Vec<StreamItem> = stream
            .filter_map(|item| async move { item.ok() })
            .collect()
            .await;

        // The streaming tool call chunks should produce a stop event at the end
        let has_stop = items.iter().any(|item| {
            matches!(
                item,
                StreamItem::Event(branchforge::types::StreamEvent::MessageDelta { .. })
            )
        });
        assert!(
            has_stop,
            "Expected a MessageDelta stop event for tool_calls finish_reason"
        );
    }

    #[tokio::test]
    async fn test_openai_system_prompt() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(mock_openai_response("I am a pirate! Arrr!")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Say something")])
            .system("You are a pirate. Always respond like a pirate.");

        // Verify transform_request includes the system message
        let body = adapter.transform_request(request.clone()).await.unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(
            messages[0]["content"],
            "You are a pirate. Always respond like a pirate."
        );
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Say something");

        let response = adapter.send(&http, request).await.unwrap();
        assert_eq!(response.text(), "I am a pirate! Arrr!");
    }

    #[tokio::test]
    async fn test_openai_structured_output() {
        let server = MockServer::start().await;

        let structured_response = json!({
            "id": "chatcmpl-structured",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "{\"name\":\"Alice\",\"age\":30}"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 10,
                "total_tokens": 30
            }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(structured_response))
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();

        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer"}
            },
            "required": ["name", "age"]
        });

        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Generate a person")])
            .output_format(OutputFormat::json_schema_named("person", schema));

        // Verify the request body includes response_format
        let body = adapter.transform_request(request.clone()).await.unwrap();
        let rf = &body["response_format"];
        assert_eq!(rf["type"], "json_schema");
        assert_eq!(rf["json_schema"]["name"], "person");
        assert_eq!(rf["json_schema"]["strict"], true);
        assert!(rf["json_schema"]["schema"].is_object());

        let response = adapter.send(&http, request).await.unwrap();
        let parsed: Value = serde_json::from_str(&response.text()).unwrap();
        assert_eq!(parsed["name"], "Alice");
        assert_eq!(parsed["age"], 30);
    }

    #[tokio::test]
    async fn test_openai_error_response_429() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(429).set_body_json(mock_openai_error_response(
                    "Rate limit exceeded. Please retry after 20s.",
                    "rate_limit_error",
                )),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Hello")]);

        let err = adapter.send(&http, request).await.unwrap_err();
        match err {
            branchforge::Error::Api {
                message,
                status,
                error_type,
            } => {
                assert_eq!(status, Some(429));
                assert!(message.contains("Rate limit"));
                assert_eq!(error_type, Some("rate_limit_error".to_string()));
            }
            other => panic!("Expected Api error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_openai_error_response_500() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(mock_openai_error_response(
                    "Internal server error",
                    "server_error",
                )),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Hello")]);

        let err = adapter.send(&http, request).await.unwrap_err();
        match err {
            branchforge::Error::Api { status, .. } => {
                assert_eq!(status, Some(500));
            }
            other => panic!("Expected Api error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_openai_custom_base_url() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_openai_response("Ollama response")),
            )
            .expect(1)
            .mount(&server)
            .await;

        // Simulate Ollama/Together/Groq-style custom base URL
        let custom_url = server.uri();
        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "custom-key").base_url(&custom_url);

        assert_eq!(ProviderAdapter::base_url(&adapter), custom_url);

        let http = reqwest::Client::new();
        let request =
            CreateMessageRequest::new("llama-3.1-70b", vec![Message::user("Hello from Ollama")]);

        let response = adapter.send(&http, request).await.unwrap();
        assert_eq!(response.text(), "Ollama response");
    }

    #[tokio::test]
    async fn test_openai_response_with_text_and_tool_calls() {
        let server = MockServer::start().await;

        // Some OpenAI responses include both text content and tool calls
        let response_json = json!({
            "id": "chatcmpl-mixed",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Let me look that up for you.",
                    "tool_calls": [{
                        "id": "call_lookup",
                        "type": "function",
                        "function": {
                            "name": "search",
                            "arguments": "{\"query\":\"rust programming\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 20,
                "total_tokens": 35
            }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_json))
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Search for Rust")]);

        let response = adapter.send(&http, request).await.unwrap();

        assert_eq!(response.text(), "Let me look that up for you.");
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        let tool_uses = response.tool_uses();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "search");
        assert_eq!(tool_uses[0].input["query"], "rust programming");
    }

    #[tokio::test]
    async fn test_openai_multiple_tool_calls() {
        let server = MockServer::start().await;

        let response_json = json!({
            "id": "chatcmpl-multi-tool",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments": "{\"path\":\"src/main.rs\"}"
                            }
                        },
                        {
                            "id": "call_2",
                            "type": "function",
                            "function": {
                                "name": "read_file",
                                "arguments": "{\"path\":\"Cargo.toml\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 30,
                "completion_tokens": 25,
                "total_tokens": 55
            }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_json))
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Read both files")]);

        let response = adapter.send(&http, request).await.unwrap();

        let tool_uses = response.tool_uses();
        assert_eq!(tool_uses.len(), 2);
        assert_eq!(tool_uses[0].id, "call_1");
        assert_eq!(tool_uses[0].name, "read_file");
        assert_eq!(tool_uses[0].input["path"], "src/main.rs");
        assert_eq!(tool_uses[1].id, "call_2");
        assert_eq!(tool_uses[1].name, "read_file");
        assert_eq!(tool_uses[1].input["path"], "Cargo.toml");
    }

    #[tokio::test]
    async fn test_openai_build_url() {
        let adapter = OpenAiAdapter::from_api_key(test_openai_config(), "key")
            .base_url("https://api.example.com/v1");

        let url = adapter.build_url("gpt-4o", false).await;
        assert_eq!(url, "https://api.example.com/v1/chat/completions");

        // Streaming uses the same endpoint
        let stream_url = adapter.build_url("gpt-4o", true).await;
        assert_eq!(stream_url, "https://api.example.com/v1/chat/completions");
    }

    #[tokio::test]
    async fn test_openai_transform_request_with_tools() {
        let adapter = OpenAiAdapter::from_api_key(test_openai_config(), "key");

        let tool = ToolDefinition::new(
            "calculator",
            "Perform arithmetic",
            json!({
                "type": "object",
                "properties": {
                    "expression": {"type": "string"}
                },
                "required": ["expression"]
            }),
        );

        let request =
            CreateMessageRequest::new("gpt-4o", vec![Message::user("2+2")]).tools(vec![tool]);

        let body = adapter.transform_request(request).await.unwrap();

        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "calculator");
        assert_eq!(tools[0]["function"]["description"], "Perform arithmetic");
        assert!(tools[0]["function"]["parameters"].is_object());
    }

    #[tokio::test]
    async fn test_openai_empty_choices_error() {
        let server = MockServer::start().await;

        let bad_response = json!({
            "id": "chatcmpl-empty",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 0,
                "total_tokens": 5
            }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bad_response))
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            OpenAiAdapter::from_api_key(test_openai_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gpt-4o", vec![Message::user("Hello")]);

        let err = adapter.send(&http, request).await.unwrap_err();
        assert!(
            matches!(err, branchforge::Error::Parse(ref msg) if msg.contains("no choices")),
            "Expected parse error about no choices, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_openai_finish_reason_mapping() {
        let adapter = OpenAiAdapter::from_api_key(test_openai_config(), "key");

        // "length" maps to MaxTokens
        let resp = json!({
            "id": "test",
            "model": "gpt-4o",
            "choices": [{
                "message": { "role": "assistant", "content": "truncated..." },
                "finish_reason": "length"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 4096 }
        });
        let response = adapter.transform_response(resp).unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::MaxTokens));

        // "content_filter" maps to Refusal
        let resp = json!({
            "id": "test",
            "model": "gpt-4o",
            "choices": [{
                "message": { "role": "assistant", "content": "" },
                "finish_reason": "content_filter"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 0 }
        });
        let response = adapter.transform_response(resp).unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::Refusal));
    }

    #[tokio::test]
    async fn test_openai_parse_stream_event_text() {
        let adapter = OpenAiAdapter::from_api_key(test_openai_config(), "key");

        let json = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let item = adapter.parse_stream_event(json);
        assert!(matches!(item, Some(StreamItem::Text(ref t)) if t == "Hello"));
    }

    #[tokio::test]
    async fn test_openai_parse_stream_event_stop() {
        let adapter = OpenAiAdapter::from_api_key(test_openai_config(), "key");

        let json = r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let item = adapter.parse_stream_event(json);
        assert!(
            matches!(
                item,
                Some(StreamItem::Event(
                    branchforge::types::StreamEvent::MessageDelta { .. }
                ))
            ),
            "Expected MessageDelta event, got {:?}",
            item
        );
    }
}

#[cfg(feature = "gemini")]
mod gemini_tests {
    use branchforge::client::StreamItem;
    use branchforge::client::adapter::{
        GeminiAdapter, ModelConfig, ProviderAdapter, ProviderConfig,
    };
    use branchforge::client::messages::{CreateMessageRequest, OutputFormat};
    use branchforge::types::{Message, StopReason, ToolDefinition};
    use serde_json::{Value, json};
    use wiremock::matchers::{body_partial_json, header, method, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_gemini_config() -> ProviderConfig {
        ProviderConfig::new(ModelConfig::new(
            "gemini-2.0-flash",
            "gemini-2.0-flash-lite",
        ))
    }

    fn mock_gemini_response(content: &str) -> Value {
        json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": content}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 6,
                "totalTokenCount": 16
            }
        })
    }

    fn mock_gemini_function_call_response(name: &str, args: Value) -> Value {
        json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": name,
                            "args": args
                        }
                    }]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 15,
                "candidatesTokenCount": 12,
                "totalTokenCount": 27
            }
        })
    }

    fn mock_gemini_stream_body(chunks: &[Value]) -> String {
        let mut body = String::new();
        for chunk in chunks {
            body.push_str(&format!("data: {}\n\n", chunk));
        }
        body
    }

    fn mock_gemini_error_response(message: &str, status_str: &str, code: u32) -> Value {
        json!({
            "error": {
                "message": message,
                "status": status_str,
                "code": code
            }
        })
    }

    #[tokio::test]
    async fn test_gemini_send_simple_message() {
        let server = MockServer::start().await;

        // Gemini uses API key in query param, path includes the model name
        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .and(query_param("key", "test-gemini-key"))
            .and(body_partial_json(json!({
                "contents": [{"role": "user", "parts": [{"text": "Hello"}]}]
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(mock_gemini_response("Hello from Gemini!")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter = GeminiAdapter::from_api_key(test_gemini_config(), "test-gemini-key")
            .base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")]);

        let response = adapter.send(&http, request).await.unwrap();

        assert_eq!(response.model, "gemini-2.0-flash");
        assert_eq!(response.text(), "Hello from Gemini!");
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(response.usage.output_tokens, 6);
        // Gemini generates IDs, so just verify it starts with the expected prefix
        assert!(response.id.starts_with("gemini-"));
    }

    #[tokio::test]
    async fn test_gemini_send_with_function_calling() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                mock_gemini_function_call_response(
                    "get_weather",
                    json!({"location": "Tokyo", "unit": "celsius"}),
                ),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            GeminiAdapter::from_api_key(test_gemini_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();

        let tool = ToolDefinition::new(
            "get_weather",
            "Get weather for a location",
            json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"},
                    "unit": {"type": "string"}
                },
                "required": ["location"]
            }),
        );

        let request = CreateMessageRequest::new(
            "gemini-2.0-flash",
            vec![Message::user("What's the weather in Tokyo?")],
        )
        .tools(vec![tool]);

        let response = adapter.send(&http, request).await.unwrap();

        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        let tool_uses = response.tool_uses();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "get_weather");
        assert_eq!(tool_uses[0].input["location"], "Tokyo");
        assert_eq!(tool_uses[0].input["unit"], "celsius");
        // Gemini generates tool use IDs
        assert!(tool_uses[0].id.starts_with("call_"));
    }

    #[tokio::test]
    async fn test_gemini_streaming() {
        let server = MockServer::start().await;

        let chunks = vec![
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"text": "Hello"}]
                    }
                }]
            }),
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"text": " from"}]
                    }
                }]
            }),
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"text": " Gemini!"}]
                    },
                    "finishReason": "STOP"
                }]
            }),
        ];

        Mock::given(method("POST"))
            .and(path_regex(
                r"/models/gemini-2.0-flash:streamGenerateContent",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(mock_gemini_stream_body(&chunks)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            GeminiAdapter::from_api_key(test_gemini_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")]);

        let response = adapter.send_stream(&http, request).await.unwrap();

        use branchforge::client::StreamParser;
        use futures::StreamExt;

        let stream = StreamParser::with_event_parser(response.bytes_stream(), {
            let parser_adapter = GeminiAdapter::from_api_key(test_gemini_config(), "test-key");
            move |json| parser_adapter.parse_stream_event(json)
        });

        let items: Vec<StreamItem> = stream
            .filter_map(|item| async move { item.ok() })
            .collect()
            .await;

        let text_items: Vec<&str> = items
            .iter()
            .filter_map(|item| match item {
                StreamItem::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(text_items, vec!["Hello", " from", " Gemini!"]);
    }

    #[tokio::test]
    async fn test_gemini_system_instruction() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_gemini_response("Ahoy, matey!")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            GeminiAdapter::from_api_key(test_gemini_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request =
            CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Greet me")])
                .system("You are a pirate. Speak like a pirate.");

        // Verify transform_request uses systemInstruction (not in contents)
        let body = adapter.transform_request(request.clone()).await.unwrap();
        let system_instruction = &body["systemInstruction"];
        assert!(system_instruction.is_object(), "Expected systemInstruction");
        assert_eq!(
            system_instruction["parts"][0]["text"],
            "You are a pirate. Speak like a pirate."
        );

        // System prompt should NOT appear in contents
        let contents = body["contents"].as_array().unwrap();
        for content in contents {
            assert_ne!(
                content["role"], "system",
                "System should not be in contents"
            );
        }

        let response = adapter.send(&http, request).await.unwrap();
        assert_eq!(response.text(), "Ahoy, matey!");
    }

    #[tokio::test]
    async fn test_gemini_structured_output() {
        let server = MockServer::start().await;

        let structured_response = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "{\"name\":\"Bob\",\"age\":25}"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 15,
                "candidatesTokenCount": 8,
                "totalTokenCount": 23
            }
        });

        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(structured_response))
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            GeminiAdapter::from_api_key(test_gemini_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();

        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer"}
            },
            "required": ["name", "age"]
        });

        let request =
            CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Generate a person")])
                .output_format(OutputFormat::json_schema_named("person", schema.clone()));

        // Verify the request body includes responseMimeType and responseSchema
        let body = adapter.transform_request(request.clone()).await.unwrap();
        let gen_config = &body["generationConfig"];
        assert_eq!(gen_config["responseMimeType"], "application/json");
        assert_eq!(gen_config["responseSchema"], schema);

        let response = adapter.send(&http, request).await.unwrap();
        let parsed: Value = serde_json::from_str(&response.text()).unwrap();
        assert_eq!(parsed["name"], "Bob");
        assert_eq!(parsed["age"], 25);
    }

    #[tokio::test]
    async fn test_gemini_error_response() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(mock_gemini_error_response(
                    "API key not valid. Please pass a valid API key.",
                    "INVALID_ARGUMENT",
                    400,
                )),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            GeminiAdapter::from_api_key(test_gemini_config(), "invalid-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")]);

        let err = adapter.send(&http, request).await.unwrap_err();
        match err {
            branchforge::Error::Api {
                message,
                status,
                error_type,
            } => {
                assert_eq!(status, Some(400));
                assert!(message.contains("API key not valid"));
                assert_eq!(error_type, Some("INVALID_ARGUMENT".to_string()));
            }
            other => panic!("Expected Api error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_gemini_error_response_quota() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .respond_with(
                ResponseTemplate::new(429).set_body_json(mock_gemini_error_response(
                    "Quota exceeded for quota metric",
                    "RESOURCE_EXHAUSTED",
                    429,
                )),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            GeminiAdapter::from_api_key(test_gemini_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")]);

        let err = adapter.send(&http, request).await.unwrap_err();
        match err {
            branchforge::Error::Api { status, .. } => {
                assert_eq!(status, Some(429));
            }
            other => panic!("Expected Api error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_gemini_oauth_vs_api_key() {
        // API key mode: key is appended as query parameter
        let server_apikey = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .and(query_param("key", "my-api-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_gemini_response("API key mode")),
            )
            .expect(1)
            .named("api-key-mode")
            .mount(&server_apikey)
            .await;

        let adapter_apikey = GeminiAdapter::from_api_key(test_gemini_config(), "my-api-key")
            .base_url(server_apikey.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")]);

        let response = adapter_apikey.send(&http, request).await.unwrap();
        assert_eq!(response.text(), "API key mode");

        // OAuth mode: key is sent as Bearer token header
        let server_oauth = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .and(header("Authorization", "Bearer my-oauth-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_gemini_response("OAuth mode")),
            )
            .expect(1)
            .named("oauth-mode")
            .mount(&server_oauth)
            .await;

        let adapter_oauth = GeminiAdapter::from_api_key(test_gemini_config(), "my-oauth-token")
            .base_url(server_oauth.uri())
            .oauth();
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")]);

        let response = adapter_oauth.send(&http, request).await.unwrap();
        assert_eq!(response.text(), "OAuth mode");
    }

    #[tokio::test]
    async fn test_gemini_build_url_api_key_vs_oauth() {
        // In API key mode, build_url (public) should NOT include the key (for safe logging)
        let adapter = GeminiAdapter::from_api_key(test_gemini_config(), "secret-key")
            .base_url("https://example.com/v1beta");

        let url = adapter.build_url("gemini-2.0-flash", false).await;
        assert!(
            !url.contains("secret-key"),
            "Public URL should not contain API key: {}",
            url
        );
        assert!(url.contains("generateContent"));

        let stream_url = adapter.build_url("gemini-2.0-flash", true).await;
        assert!(stream_url.contains("streamGenerateContent"));
        assert!(stream_url.contains("alt=sse"));
    }

    #[tokio::test]
    async fn test_gemini_transform_request_with_tools() {
        let adapter = GeminiAdapter::from_api_key(test_gemini_config(), "key");

        let tool = ToolDefinition::new(
            "search_code",
            "Search code in repository",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "language": {"type": "string"}
                },
                "required": ["query"]
            }),
        );

        let request = CreateMessageRequest::new(
            "gemini-2.0-flash",
            vec![Message::user("Find all TODO comments")],
        )
        .tools(vec![tool]);

        let body = adapter.transform_request(request).await.unwrap();

        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        let declarations = tools[0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0]["name"], "search_code");
        assert_eq!(declarations[0]["description"], "Search code in repository");
        assert!(declarations[0]["parameters"].is_object());
    }

    #[tokio::test]
    async fn test_gemini_no_candidates_error() {
        let server = MockServer::start().await;

        let bad_response = json!({
            "candidates": [],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 0,
                "totalTokenCount": 5
            }
        });

        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bad_response))
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            GeminiAdapter::from_api_key(test_gemini_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")]);

        let err = adapter.send(&http, request).await.unwrap_err();
        assert!(
            matches!(err, branchforge::Error::Parse(ref msg) if msg.contains("no candidates")),
            "Expected parse error about no candidates, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_gemini_finish_reason_mapping() {
        let adapter = GeminiAdapter::from_api_key(test_gemini_config(), "key");

        // MAX_TOKENS
        let resp = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "truncated..."}]
                },
                "finishReason": "MAX_TOKENS"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 8192,
                "totalTokenCount": 8202
            }
        });
        let response = adapter.transform_response(resp).unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::MaxTokens));

        // SAFETY -> Refusal
        let resp = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": ""}]
                },
                "finishReason": "SAFETY"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 0,
                "totalTokenCount": 10
            }
        });
        let response = adapter.transform_response(resp).unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::Refusal));
    }

    #[tokio::test]
    async fn test_gemini_streaming_function_call() {
        let server = MockServer::start().await;

        let chunks = vec![json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "read_file",
                            "args": {"path": "main.rs"}
                        }
                    }]
                }
            }]
        })];

        Mock::given(method("POST"))
            .and(path_regex(
                r"/models/gemini-2.0-flash:streamGenerateContent",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(mock_gemini_stream_body(&chunks)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            GeminiAdapter::from_api_key(test_gemini_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request =
            CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Read main.rs")]);

        let response = adapter.send_stream(&http, request).await.unwrap();

        use branchforge::client::StreamParser;
        use futures::StreamExt;

        let stream = StreamParser::with_event_parser(response.bytes_stream(), {
            let parser_adapter = GeminiAdapter::from_api_key(test_gemini_config(), "test-key");
            move |json| parser_adapter.parse_stream_event(json)
        });

        let items: Vec<StreamItem> = stream
            .filter_map(|item| async move { item.ok() })
            .collect()
            .await;

        let tool_items: Vec<_> = items
            .iter()
            .filter_map(|item| match item {
                StreamItem::ToolUseComplete(tu) => Some(tu),
                _ => None,
            })
            .collect();

        assert_eq!(tool_items.len(), 1);
        assert_eq!(tool_items[0].name, "read_file");
        assert_eq!(tool_items[0].input["path"], "main.rs");
        assert!(tool_items[0].id.starts_with("call_"));
    }

    #[tokio::test]
    async fn test_gemini_parse_stream_event_text() {
        let adapter = GeminiAdapter::from_api_key(test_gemini_config(), "key");

        let json_str =
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"World"}]}}]}"#;
        let item = adapter.parse_stream_event(json_str);
        assert!(matches!(item, Some(StreamItem::Text(ref t)) if t == "World"));
    }

    #[tokio::test]
    async fn test_gemini_parse_stream_event_function_call() {
        let adapter = GeminiAdapter::from_api_key(test_gemini_config(), "key");

        let json_str = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"calc","args":{"expr":"1+1"}}}]}}]}"#;
        let item = adapter.parse_stream_event(json_str);
        assert!(
            matches!(item, Some(StreamItem::ToolUseComplete(ref tu)) if tu.name == "calc"),
            "Expected ToolUseComplete, got {:?}",
            item
        );
    }

    #[tokio::test]
    async fn test_gemini_generation_config_parameters() {
        let adapter = GeminiAdapter::from_api_key(test_gemini_config(), "key");

        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hi")])
            .max_tokens(4096)
            .temperature(0.9);

        let body = adapter.transform_request(request).await.unwrap();
        let gen_config = &body["generationConfig"];
        assert_eq!(gen_config["maxOutputTokens"], 4096);
        // Compare as f32 to avoid f32 -> f64 precision issues in JSON roundtrip
        let temp = gen_config["temperature"].as_f64().unwrap();
        assert!((temp - 0.9).abs() < 0.001, "Expected ~0.9, got {}", temp);
    }

    #[tokio::test]
    async fn test_gemini_assistant_role_mapped_to_model() {
        let adapter = GeminiAdapter::from_api_key(test_gemini_config(), "key");

        let request = CreateMessageRequest::new(
            "gemini-2.0-flash",
            vec![
                Message::user("Hello"),
                Message::assistant("Hi there!"),
                Message::user("How are you?"),
            ],
        );

        let body = adapter.transform_request(request).await.unwrap();
        let contents = body["contents"].as_array().unwrap();

        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model"); // assistant -> model
        assert_eq!(contents[2]["role"], "user");
    }

    #[tokio::test]
    async fn test_gemini_custom_base_url() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(mock_gemini_response("Custom endpoint")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let custom_url = server.uri();
        let adapter =
            GeminiAdapter::from_api_key(test_gemini_config(), "key").base_url(&custom_url);

        assert_eq!(ProviderAdapter::base_url(&adapter), custom_url);

        let http = reqwest::Client::new();
        let request = CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Hello")]);

        let response = adapter.send(&http, request).await.unwrap();
        assert_eq!(response.text(), "Custom endpoint");
    }

    #[tokio::test]
    async fn test_gemini_text_and_function_call_combined() {
        let server = MockServer::start().await;

        // Response with both text and a function call in parts
        let response_json = json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        {"text": "I'll check that for you."},
                        {
                            "functionCall": {
                                "name": "lookup",
                                "args": {"query": "weather"}
                            }
                        }
                    ]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 15,
                "totalTokenCount": 25
            }
        });

        Mock::given(method("POST"))
            .and(path_regex(r"/models/gemini-2.0-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_json))
            .expect(1)
            .mount(&server)
            .await;

        let adapter =
            GeminiAdapter::from_api_key(test_gemini_config(), "test-key").base_url(server.uri());
        let http = reqwest::Client::new();
        let request =
            CreateMessageRequest::new("gemini-2.0-flash", vec![Message::user("Check weather")]);

        let response = adapter.send(&http, request).await.unwrap();

        assert_eq!(response.text(), "I'll check that for you.");
        assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
        let tool_uses = response.tool_uses();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "lookup");
    }
}
