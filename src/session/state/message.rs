//! Session message types.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::ids::MessageId;
use crate::session::types::EnvironmentContext;
use crate::types::{ContentBlock, Message, Role, StopReason, TokenUsage, Usage};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExecutionMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_calls: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compactions: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<Decimal>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MessageMetadata {
    pub model: Option<String>,
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<serde_json::Value>,
    pub tool_results: Option<Vec<ToolResultMeta>>,
    pub thinking: Option<ThinkingMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionMetadata>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolResultMeta {
    pub tool_use_id: String,
    pub tool_name: String,
    pub is_error: bool,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThinkingMetadata {
    pub level: String,
    pub disabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMessage {
    pub id: MessageId,
    pub parent_id: Option<MessageId>,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub is_sidechain: bool,
    #[serde(default)]
    pub is_compact_summary: bool,
    pub usage: Option<TokenUsage>,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub metadata: MessageMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentContext>,
}

impl SessionMessage {
    pub fn user(content: Vec<ContentBlock>) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            role: Role::User,
            content,
            is_sidechain: false,
            is_compact_summary: false,
            usage: None,
            timestamp: Utc::now(),
            metadata: MessageMetadata::default(),
            environment: None,
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            role: Role::Assistant,
            content,
            is_sidechain: false,
            is_compact_summary: false,
            usage: None,
            timestamp: Utc::now(),
            metadata: MessageMetadata::default(),
            environment: None,
        }
    }

    pub fn parent(mut self, parent_id: MessageId) -> Self {
        self.parent_id = Some(parent_id);
        self
    }

    pub fn usage(mut self, usage: TokenUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    pub fn as_sidechain(mut self) -> Self {
        self.is_sidechain = true;
        self
    }

    pub fn as_compact_summary(mut self) -> Self {
        self.is_compact_summary = true;
        self
    }

    pub fn environment(mut self, env: EnvironmentContext) -> Self {
        self.environment = Some(env);
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.metadata.model = Some(model.into());
        self
    }

    pub fn request_id(mut self, request_id: impl Into<String>) -> Self {
        self.metadata.request_id = Some(request_id.into());
        self
    }

    pub fn metadata(mut self, metadata: MessageMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn to_api_message(&self) -> Message {
        Message {
            role: self.role,
            content: self.content.clone(),
        }
    }
}
