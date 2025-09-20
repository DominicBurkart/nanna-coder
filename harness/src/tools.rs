use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{Value, json};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ToolError {
    #[error("Invalid arguments: {message}")]
    InvalidArguments { message: String },

    #[error("Execution failed: {message}")]
    ExecutionFailed { message: String },

    #[error("Tool not found: {name}")]
    NotFound { name: String },

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type ToolResult<T> = Result<T, ToolError>;

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, args: Value) -> ToolResult<Value>;
    fn name(&self) -> &str;
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.insert(name, tool);
    }

    pub fn get_tool(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn list_tools(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    pub fn get_definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|tool| tool.definition()).collect()
    }

    pub async fn execute(&self, name: &str, args: Value) -> ToolResult<Value> {
        match self.tools.get(name) {
            Some(tool) => tool.execute(args).await,
            None => Err(ToolError::NotFound {
                name: name.to_string(),
            }),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_echo_tool() {
        let tool = EchoTool::new();
        let args = json!({ "message": "Hello, World!" });
        let result = tool.execute(args).await.unwrap();

        assert_eq!(result["echoed"], "Hello, World!");
        assert!(result["timestamp"].is_string());
    }

    #[tokio::test]
    async fn test_calculator_tool() {
        let tool = CalculatorTool::new();

        let args = json!({
            "operation": "add",
            "a": 5.0,
            "b": 3.0
        });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["result"], 8.0);

        let args = json!({
            "operation": "divide",
            "a": 10.0,
            "b": 0.0
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_registry() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        registry.register(Box::new(CalculatorTool::new()));

        assert_eq!(registry.list_tools().len(), 2);
        assert!(registry.get_tool("echo").is_some());
        assert!(registry.get_tool("calculate").is_some());
        assert!(registry.get_tool("nonexistent").is_none());

        let definitions = registry.get_definitions();
        assert_eq!(definitions.len(), 2);

        let result = registry
            .execute("echo", json!({ "message": "test" }))
            .await
            .unwrap();
        assert_eq!(result["echoed"], "test");
    }
}
