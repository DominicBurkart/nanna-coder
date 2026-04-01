use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{Tool, ToolError, ToolResult};
use super::filesystem::validate_path_within_workspace;

pub struct SearchTool {
    workspace_root: PathBuf,
}

impl SearchTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    fn search_recursive(
        &self,
        dir: &Path,
        regex: &regex::Regex,
        file_pattern: Option<&str>,
        max_results: usize,
        results: &mut Vec<Value>,
    ) -> ToolResult<()> {
        if results.len() >= max_results {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                self.search_recursive(&path, regex, file_pattern, max_results, results)?;
            } else if file_type.is_file() {
                let name = entry.file_name().to_string_lossy().to_string();

                if let Some(pat) = file_pattern {
                    if !glob::Pattern::new(pat)
                        .map_err(|e| ToolError::InvalidArguments {
                            message: format!("Invalid glob pattern: {}", e),
                        })?
                        .matches(&name)
                    {
                        continue;
                    }
                }

                if let Ok(content) = std::fs::read_to_string(&path) {
                    let relative = path
                        .strip_prefix(&self.workspace_root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();

                    for (line_num, line) in content.lines().enumerate() {
                        if results.len() >= max_results {
                            return Ok(());
                        }

                        if regex.is_match(line) {
                            results.push(json!({
                                "file": relative,
                                "line": line_num + 1,
                                "content": line,
                            }));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "search".to_string(),
                description: "Search for a pattern in files. Returns matching lines with context."
                    .to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "pattern".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("Regex pattern to search for".to_string()),
                                items: None,
                            },
                        );
                        props.insert(
                            "path".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Path to search in (defaults to workspace root)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "file_pattern".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Glob pattern to filter files (e.g., '*.rs')".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "max_results".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some(
                                    "Maximum number of results to return (default: 50)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["pattern".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let pattern_str = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "Missing or invalid 'pattern' parameter".to_string(),
            })?;

        let regex = regex::Regex::new(pattern_str).map_err(|e| ToolError::InvalidArguments {
            message: format!("Invalid regex pattern: {}", e),
        })?;

        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let path = Path::new(path_str);
        let safe_path = validate_path_within_workspace(path, &self.workspace_root)?;

        let file_pattern = args.get("file_pattern").and_then(|v| v.as_str());
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(50) as usize;

        let mut results = Vec::new();
        self.search_recursive(&safe_path, &regex, file_pattern, max_results, &mut results)?;

        Ok(json!({
            "pattern": pattern_str,
            "results": results,
            "count": results.len()
        }))
    }

    fn name(&self) -> &str {
        "search"
    }
}
