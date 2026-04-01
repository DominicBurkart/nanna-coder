use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::{Tool, ToolError, ToolResult};

pub struct RunCommandTool {
    container_handle: std::sync::Arc<crate::container::ContainerHandle>,
    working_dir: Option<String>,
}

impl RunCommandTool {
    pub fn new(
        container_handle: std::sync::Arc<crate::container::ContainerHandle>,
        working_dir: Option<String>,
    ) -> Self {
        Self {
            container_handle,
            working_dir,
        }
    }
}

#[async_trait]
impl Tool for RunCommandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "run_command".to_string(),
                description: "Run a shell command in the dev container workspace.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "command".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "The shell command to run (passed to sh -c)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["command".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "Missing or invalid 'command' parameter".to_string(),
            })?;

        let result = crate::container::exec_in_container(
            &self.container_handle,
            &["sh", "-c", command],
            self.working_dir.as_deref(),
        )
        .map_err(|e| ToolError::ExecutionFailed {
            message: e.to_string(),
        })?;

        Ok(json!({
            "stdout": result.stdout,
            "stderr": result.stderr,
            "success": result.success,
        }))
    }

    fn name(&self) -> &str {
        "run_command"
    }
}
