use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::{Tool, ToolError, ToolResult};

pub struct CalculatorTool;

impl CalculatorTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CalculatorTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CalculatorTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "calculate".to_string(),
                description: "Perform basic arithmetic calculations".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "operation".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "The operation: add, subtract, multiply, divide".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "a".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Number,
                                description: Some("First number".to_string()),
                                items: None,
                            },
                        );
                        props.insert(
                            "b".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Number,
                                description: Some("Second number".to_string()),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec![
                        "operation".to_string(),
                        "a".to_string(),
                        "b".to_string(),
                    ]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let operation = args
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "Missing or invalid 'operation' parameter".to_string(),
            })?;

        let a =
            args.get("a")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| ToolError::InvalidArguments {
                    message: "Missing or invalid 'a' parameter".to_string(),
                })?;

        let b =
            args.get("b")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| ToolError::InvalidArguments {
                    message: "Missing or invalid 'b' parameter".to_string(),
                })?;

        let result = match operation {
            "add" => a + b,
            "subtract" => a - b,
            "multiply" => a * b,
            "divide" => {
                if b == 0.0 {
                    return Err(ToolError::ExecutionFailed {
                        message: "Division by zero".to_string(),
                    });
                }
                a / b
            }
            _ => {
                return Err(ToolError::InvalidArguments {
                    message: format!("Unknown operation: {}", operation),
                });
            }
        };

        Ok(json!({
            "operation": operation,
            "operands": [a, b],
            "result": result
        }))
    }

    fn name(&self) -> &str {
        "calculate"
    }
}
