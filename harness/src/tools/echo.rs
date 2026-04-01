use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::{Tool, ToolError, ToolResult};

pub struct EchoTool;

impl EchoTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EchoTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "echo".to_string(),
                description: "Echo back the provided message".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "message".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("The message to echo back".to_string()),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["message".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "Missing or invalid 'message' parameter".to_string(),
            })?;

        Ok(json!({
            "echoed": message,
            "timestamp": chrono::Utc::now().to_rfc3339()
        }))
    }

    fn name(&self) -> &str {
        "echo"
    }
}
