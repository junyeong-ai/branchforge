//! Migration bridge between legacy `types::*` / `client::messages::*`
//! types and the new neutral `ir::*` IR.
//!
//! These conversions exist solely to allow the existing `Client` to
//! transparently dispatch to `ProviderClient` (new codec/transport stack)
//! without modifying the agent runtime or session layer. They will be
//! deleted when the agent runtime switches to producing `ir::ModelRequest`
//! directly (Phase 1b completion).
//!
//! **Do not add new dependents on these conversions.** New code should use
//! `ir::*` directly.

use crate::ir;
use crate::types;

// =============================================================================
// CreateMessageRequest → ModelRequest
// =============================================================================

impl From<&crate::client::messages::CreateMessageRequest> for ir::ModelRequest {
    fn from(req: &crate::client::messages::CreateMessageRequest) -> Self {
        let messages = req.messages.iter().map(legacy_message_to_ir).collect();

        let system = req.system.as_ref().map(legacy_system_to_ir);

        let tools: Vec<ir::ToolDefinition> = req
            .tools
            .as_ref()
            .map(|ts| {
                ts.iter()
                    .filter_map(|t| match t {
                        crate::client::messages::ApiTool::Custom(def) => Some(ir::ToolDefinition {
                            name: def.name.clone(),
                            description: Some(def.description.clone()),
                            parameters: def.input_schema.clone(),
                            strict: def.strict.unwrap_or(false),
                        }),
                        // Server-side tools (web_search, web_fetch, tool_search) are
                        // handled by the adapter/codec layer, not by the IR tool array.
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let reasoning = req.thinking.as_ref().and_then(|t| {
            t.budget_tokens.map(|budget| ir::ReasoningSettings {
                budget_tokens: Some(budget as u64),
                effort: None,
                include_thoughts: true,
            })
        });
        let settings = ir::ModelSettings {
            max_output_tokens: Some(req.max_tokens),
            temperature: req.temperature,
            top_p: req.top_p,
            top_k: req.top_k,
            stop_sequences: req.stop_sequences.clone().unwrap_or_default(),
            reasoning,
            ..Default::default()
        };

        ir::ModelRequest {
            model: req.model.clone(),
            messages,
            system,
            tools,
            tool_choice: None,
            settings,
            provider_options: ir::ProviderOptions::default(),
            continuation: None,
            metadata: std::collections::BTreeMap::new(),
            idempotency_key: None,
        }
    }
}

// =============================================================================
// ModelResponse → ApiResponse
// =============================================================================

impl From<ir::ModelResponse> for types::ApiResponse {
    fn from(resp: ir::ModelResponse) -> Self {
        let content = resp.content.iter().map(ir_content_to_legacy).collect();

        let stop_reason = match resp.finish_reason {
            ir::FinishReason::Stop => Some(types::StopReason::EndTurn),
            ir::FinishReason::Length => Some(types::StopReason::MaxTokens),
            ir::FinishReason::StopSequence => Some(types::StopReason::StopSequence),
            ir::FinishReason::ToolCalls => Some(types::StopReason::ToolUse),
            ir::FinishReason::ContentFilter => Some(types::StopReason::Refusal),
            ir::FinishReason::PauseTurn => Some(types::StopReason::EndTurn),
            ir::FinishReason::Error => None,
            ir::FinishReason::Other(_) => None,
        };

        let usage = types::Usage {
            input_tokens: resp.usage.input_tokens as u32,
            output_tokens: resp.usage.output_tokens as u32,
            cache_read_input_tokens: resp.usage.cached_input_tokens.map(|v| v as u32),
            cache_creation_input_tokens: resp.usage.cache_creation_tokens.map(|v| v as u32),
            server_tool_use: None,
        };

        types::ApiResponse {
            id: resp.id,
            response_type: "message".into(),
            role: "assistant".into(),
            content,
            model: resp.model,
            stop_reason,
            stop_sequence: None,
            usage,
            context_management: None,
        }
    }
}

// =============================================================================
// ir::Usage → types::TokenUsage
// =============================================================================

impl From<&ir::Usage> for types::TokenUsage {
    fn from(u: &ir::Usage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_input_tokens: u.cached_input_tokens.unwrap_or(0),
            cache_creation_input_tokens: u.cache_creation_tokens.unwrap_or(0),
            ..Default::default()
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn legacy_message_to_ir(m: &types::Message) -> ir::Message {
    let role = match m.role {
        types::Role::User => ir::Role::User,
        types::Role::Assistant => ir::Role::Assistant,
    };
    let content = m.content.iter().map(legacy_block_to_ir).collect();
    ir::Message { role, content }
}

fn legacy_block_to_ir(block: &types::ContentBlock) -> ir::ContentPart {
    match block {
        types::ContentBlock::Text { text, .. } => ir::ContentPart::Text { text: text.clone() },
        types::ContentBlock::ToolUse(tu) => ir::ContentPart::ToolCall {
            id: tu.id.clone(),
            name: tu.name.clone(),
            arguments: tu.input.clone(),
            origin: ir::ToolOrigin::Local,
        },
        types::ContentBlock::ToolResult(tr) => {
            let content = match &tr.content {
                Some(types::ToolResultContent::Text(s)) => ir::ToolResultContent::Text(s.clone()),
                Some(types::ToolResultContent::Blocks(blocks)) => {
                    let text = blocks
                        .iter()
                        .filter_map(|b| match b {
                            types::ToolResultContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    ir::ToolResultContent::Text(text)
                }
                None => ir::ToolResultContent::Text(String::new()),
            };
            ir::ContentPart::ToolResult {
                tool_call_id: tr.tool_use_id.clone(),
                content,
                is_error: tr.is_error.unwrap_or(false),
            }
        }
        types::ContentBlock::Image { source } => {
            let (media_source, mime) = match source {
                types::ImageSource::Base64 { media_type, data } => (
                    ir::MediaSource::Base64 { data: data.clone() },
                    media_type.clone(),
                ),
                types::ImageSource::Url { url } => (
                    ir::MediaSource::Url { url: url.clone() },
                    "image/png".to_string(),
                ),
                types::ImageSource::File { file_id } => (
                    ir::MediaSource::FileId {
                        id: file_id.clone(),
                    },
                    "image/png".to_string(),
                ),
            };
            ir::ContentPart::Image {
                source: media_source,
                mime,
            }
        }
        types::ContentBlock::Thinking(t) => ir::ContentPart::Reasoning {
            content: ir::ReasoningContent::Visible {
                text: t.thinking.clone(),
            },
            kind: ir::ReasoningKind::FullTrace,
            signature: if t.signature.is_empty() {
                None
            } else {
                Some(ir::ReasoningSignature::new(&t.signature))
            },
        },
        types::ContentBlock::RedactedThinking { data } => ir::ContentPart::Reasoning {
            content: ir::ReasoningContent::Redacted { data: data.clone() },
            kind: ir::ReasoningKind::FullTrace,
            signature: None,
        },
        types::ContentBlock::Document(doc) => {
            let (data, mime) = match &doc.source {
                types::DocumentSource::Base64 { media_type, data } => {
                    (data.clone(), media_type.clone())
                }
                types::DocumentSource::Text { media_type, data } => {
                    (data.clone(), media_type.clone())
                }
                types::DocumentSource::File { file_id } => {
                    (file_id.clone(), "application/octet-stream".to_string())
                }
                types::DocumentSource::Url { url } => {
                    return ir::ContentPart::Document {
                        source: ir::MediaSource::Url { url: url.clone() },
                        mime: "application/pdf".to_string(),
                    };
                }
                types::DocumentSource::Content { content } => {
                    let text = content
                        .iter()
                        .map(|c| match c {
                            types::DocumentContentBlock::Text { text } => text.as_str(),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    (text, "text/plain".to_string())
                }
            };
            ir::ContentPart::Document {
                source: ir::MediaSource::Base64 { data },
                mime,
            }
        }
        _ => {
            // ServerToolUse, WebSearchToolResult, WebFetchToolResult,
            // SearchResult — pass through as Unknown.
            ir::ContentPart::Unknown {
                codec_id: "legacy-compat".to_string(),
                schema_version: 0,
                payload: serde_json::to_value(block).unwrap_or_default(),
            }
        }
    }
}

fn legacy_system_to_ir(sp: &types::SystemPrompt) -> ir::SystemPrompt {
    match sp {
        types::SystemPrompt::Text(s) => ir::SystemPrompt::Text(s.clone()),
        types::SystemPrompt::Blocks(blocks) => ir::SystemPrompt::Blocks(
            blocks
                .iter()
                .map(|b| ir::SystemBlock {
                    text: b.text.clone(),
                    cache_control: b.cache_control.as_ref().map(|_| ir::CacheControl {
                        mode: ir::CacheControlMode::System,
                        ttl: None,
                    }),
                })
                .collect(),
        ),
    }
}

fn ir_content_to_legacy(part: &ir::ContentPart) -> types::ContentBlock {
    match part {
        ir::ContentPart::Text { text } => types::ContentBlock::text(text),
        ir::ContentPart::ToolCall {
            id,
            name,
            arguments,
            ..
        } => types::ContentBlock::ToolUse(types::ToolUseBlock {
            id: id.clone(),
            name: name.clone(),
            input: arguments.clone(),
        }),
        ir::ContentPart::ToolResult {
            tool_call_id,
            content,
            is_error,
        } => {
            let legacy_content = match content {
                ir::ToolResultContent::Text(s) => Some(types::ToolResultContent::Text(s.clone())),
                ir::ToolResultContent::Json(v) => {
                    Some(types::ToolResultContent::Text(v.to_string()))
                }
                ir::ToolResultContent::MultiPart(parts) => {
                    let text = parts
                        .iter()
                        .filter_map(|p| match p {
                            ir::ContentPart::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Some(types::ToolResultContent::Text(text))
                }
            };
            types::ContentBlock::ToolResult(types::ToolResultBlock {
                tool_use_id: tool_call_id.clone(),
                content: legacy_content,
                is_error: Some(*is_error),
            })
        }
        ir::ContentPart::Reasoning {
            content: ir::ReasoningContent::Visible { text },
            signature,
            ..
        } => types::ContentBlock::Thinking(types::ThinkingBlock {
            thinking: text.clone(),
            signature: signature.as_ref().map(|s| s.0.clone()).unwrap_or_default(),
        }),
        ir::ContentPart::Reasoning {
            content: ir::ReasoningContent::Redacted { data },
            ..
        } => types::ContentBlock::RedactedThinking { data: data.clone() },
        ir::ContentPart::Image { source, mime } => {
            let img_source = match source {
                ir::MediaSource::Base64 { data } => types::ImageSource::Base64 {
                    media_type: mime.clone(),
                    data: data.clone(),
                },
                ir::MediaSource::Url { url } => types::ImageSource::Url { url: url.clone() },
                ir::MediaSource::FileId { id } => types::ImageSource::Url { url: id.clone() },
            };
            types::ContentBlock::Image { source: img_source }
        }
        _ => types::ContentBlock::text(format!("[unsupported: {:?}]", part)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::messages::CreateMessageRequest;
    use crate::types::Message;

    #[test]
    fn request_converts_basic_fields() {
        let old = CreateMessageRequest::new("claude-sonnet-4-5", vec![Message::user("hello")]);
        let new: ir::ModelRequest = (&old).into();
        assert_eq!(new.model, "claude-sonnet-4-5");
        assert_eq!(new.messages.len(), 1);
        assert_eq!(new.messages[0].role, ir::Role::User);
        assert!(matches!(
            &new.messages[0].content[0],
            ir::ContentPart::Text { text } if text == "hello"
        ));
    }

    #[test]
    fn response_converts_stop_reason_and_usage() {
        let ir_resp = ir::ModelResponse {
            id: "msg_1".into(),
            model: "claude-sonnet-4-5".into(),
            content: vec![ir::ContentPart::text("hi")],
            finish_reason: ir::FinishReason::ToolCalls,
            usage: ir::Usage {
                input_tokens: 100,
                output_tokens: 50,
                cached_input_tokens: Some(80),
                cache_creation_tokens: Some(10),
                ..Default::default()
            },
            continuation: None,
            warnings: Vec::new(),
            raw: None,
        };
        let old: types::ApiResponse = ir_resp.into();
        assert_eq!(old.id, "msg_1");
        assert_eq!(old.stop_reason, Some(types::StopReason::ToolUse));
        assert_eq!(old.usage.input_tokens, 100);
        assert_eq!(old.usage.output_tokens, 50);
        assert_eq!(old.usage.cache_read_input_tokens, Some(80));
        assert_eq!(old.usage.cache_creation_input_tokens, Some(10));
    }

    #[test]
    fn tool_call_round_trips_through_ir() {
        let old_block = types::ContentBlock::ToolUse(types::ToolUseBlock {
            id: "call_1".into(),
            name: "calc".into(),
            input: serde_json::json!({"a": 1}),
        });
        let ir_part = legacy_block_to_ir(&old_block);
        let back = ir_content_to_legacy(&ir_part);
        assert_eq!(
            serde_json::to_string(&old_block).unwrap(),
            serde_json::to_string(&back).unwrap()
        );
    }

    #[test]
    fn thinking_block_round_trips() {
        let old_block = types::ContentBlock::Thinking(types::ThinkingBlock {
            thinking: "thinking...".into(),
            signature: "sig_1".into(),
        });
        let ir_part = legacy_block_to_ir(&old_block);
        let back = ir_content_to_legacy(&ir_part);
        assert_eq!(
            serde_json::to_string(&old_block).unwrap(),
            serde_json::to_string(&back).unwrap()
        );
    }

    #[test]
    fn usage_to_token_usage() {
        let ir_usage = ir::Usage {
            input_tokens: 100,
            output_tokens: 50,
            cached_input_tokens: Some(80),
            cache_creation_tokens: Some(10),
            ..Default::default()
        };
        let legacy: types::TokenUsage = (&ir_usage).into();
        assert_eq!(legacy.input_tokens, 100);
        assert_eq!(legacy.output_tokens, 50);
        assert_eq!(legacy.cache_read_input_tokens, 80);
        assert_eq!(legacy.cache_creation_input_tokens, 10);
    }
}
