//! Tool trait definitions.

use std::any::Any;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use super::context::ExecutionContext;
use crate::types::{ToolDefinition, ToolResult};

/// Core tool trait for all tool implementations.
#[async_trait]
pub trait Tool: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn execute(&self, input: serde_json::Value, context: &ExecutionContext) -> ToolResult;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(self.name(), self.description(), self.input_schema())
    }
}

/// Schema-based tool trait with automatic JSON schema generation.
///
/// Provides a higher-level abstraction over `Tool` with typed inputs
/// and automatic schema derivation via schemars.
#[async_trait]
pub trait SchemaTool: Send + Sync {
    type Input: JsonSchema + DeserializeOwned + Send;
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const STRICT: bool = false;

    async fn handle(&self, input: Self::Input, context: &ExecutionContext) -> ToolResult;

    /// Override to provide a dynamic description instead of the static DESCRIPTION constant.
    ///
    /// When this returns `Some(desc)`, the blanket `Tool` impl uses it in `definition()`.
    /// The default returns `None`, which falls back to `DESCRIPTION`.
    fn custom_description(&self) -> Option<String> {
        None
    }

    fn input_schema() -> serde_json::Value {
        let schema = schemars::schema_for!(Self::Input);
        let mut value =
            serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({"type": "object"}));

        if let Some(obj) = value.as_object_mut() {
            if !obj.contains_key("properties") {
                obj.insert(
                    "properties".to_string(),
                    serde_json::Value::Object(serde_json::Map::new()),
                );
            }
            if !obj.contains_key("additionalProperties") {
                obj.insert(
                    "additionalProperties".to_string(),
                    serde_json::Value::Bool(!Self::STRICT),
                );
            }
        }

        value
    }
}

#[async_trait]
impl<T: SchemaTool + 'static> Tool for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        T::NAME
    }

    fn description(&self) -> &str {
        T::DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        T::input_schema()
    }

    fn definition(&self) -> ToolDefinition {
        let desc = self
            .custom_description()
            .unwrap_or_else(|| T::DESCRIPTION.to_string());
        let mut definition = ToolDefinition::new(T::NAME, &desc, T::input_schema());
        if T::STRICT {
            definition = definition.strict(true);
        }
        definition
    }

    async fn execute(&self, input: serde_json::Value, context: &ExecutionContext) -> ToolResult {
        match serde_json::from_value::<T::Input>(input.clone()) {
            Ok(typed) => SchemaTool::handle(self, typed, context).await,
            Err(e) => {
                let provided: Vec<&str> = input
                    .as_object()
                    .map(|obj| obj.keys().map(|k| k.as_str()).collect())
                    .unwrap_or_default();
                let hint = if provided.is_empty() {
                    String::new()
                } else {
                    format!("\n  Provided fields: [{}]", provided.join(", "))
                };
                ToolResult::error(format!("Invalid input for {}: {}{}", T::NAME, e, hint))
            }
        }
    }
}
