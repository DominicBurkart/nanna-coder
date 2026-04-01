use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

    #[error("Path security violation: {message}")]
    PathSecurityViolation { message: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type ToolResult<T> = Result<T, ToolError>;

/// Extract a required string parameter from JSON args.
fn require_str<'a>(args: &'a Value, name: &str) -> ToolResult<&'a str> {
    args.get(name)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArguments {
            message: format!("Missing or invalid '{}' parameter", name),
        })
}

/// Extract a required f64 parameter from JSON args.
fn require_f64(args: &Value, name: &str) -> ToolResult<f64> {
    args.get(name)
        .and_then(|v| v.as_f64())
        .ok_or_else(|| ToolError::InvalidArguments {
            message: format!("Missing or invalid '{}' parameter", name),
        })
}

/// Extract an optional string parameter with a default value.
fn opt_str<'a>(args: &'a Value, name: &str, default: &'a str) -> &'a str {
    args.get(name).and_then(|v| v.as_str()).unwrap_or(default)
}

/// Extract an optional bool parameter with a default value.
fn opt_bool(args: &Value, name: &str, default: bool) -> bool {
    args.get(name).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// Extract an optional u64 parameter.
fn opt_u64(args: &Value, name: &str) -> Option<u64> {
    args.get(name).and_then(|v| v.as_u64())
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, args: Value) -> ToolResult<Value>;
    fn name(&self) -> &str;
}