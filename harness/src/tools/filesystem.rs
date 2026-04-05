use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::registry::{Tool, ToolError, ToolResult};

pub(crate) fn validate_path_within_workspace(path: &Path, workspace_root: &Path) -> ToolResult<PathBuf> {
    let canonical_root =
        workspace_root
            .canonicalize()
            .map_err(|e| ToolError::PathSecurityViolation {
                message: format!("Cannot resolve workspace root: {}", e),
            })?;

    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };

    let canonical_path = resolved
        .canonicalize()
        .map_err(|e| ToolError::PathSecurityViolation {
            message: format!("Cannot resolve path '{}': {}", path.display(), e),
        })?;

    if !canonical_path.starts_with(&canonical_root) {
        return Err(ToolError::PathSecurityViolation {
            message: format!("Path '{}' is outside workspace root", path.display()),
        });
    }

    Ok(canonical_path)
}

pub(crate) fn validate_path_for_write(path: &Path, workspace_root: &Path) -> ToolResult<PathBuf> {
    let canonical_root =
        workspace_root
            .canonicalize()
            .map_err(|e| ToolError::PathSecurityViolation {
                message: format!("Cannot resolve workspace root: {}", e),
            })?;

    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };

    let mut check_path = resolved.as_path();
    loop {
        if let Ok(canonical) = check_path.canonicalize() {
            if !canonical.starts_with(&canonical_root) {
                return Err(ToolError::PathSecurityViolation {
                    message: format!("Path '{}' is outside workspace root", path.display()),
                });
            }
            break;
        }
        match check_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                check_path = parent;
            }
            _ => break,
        }
    }

    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(ToolError::PathSecurityViolation {
            message: "Path contains '..' components".to_string(),
        });
    }

    Ok(resolved)
}

pub struct ReadFileTool {
    workspace_root: PathBuf,
}

impl ReadFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "read_file".to_string(),
                description:
                    "Read the contents of a file. Returns the file content with line numbers."
                        .to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "path".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Path to the file (relative to workspace root)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "start_line".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some(
                                    "Starting line number (1-indexed, optional)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "end_line".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some(
                                    "Ending line number (inclusive, optional)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["path".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::InvalidArguments {
                message: "Missing or invalid 'path' parameter".to_string(),
            }
        })?;

        let path = Path::new(path_str);
        let safe_path = validate_path_within_workspace(path, &self.workspace_root)?;

        let content = std::fs::read_to_string(&safe_path)?;
        let lines: Vec<&str> = content.lines().collect();

        let start = args
            .get("start_line")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).saturating_sub(1))
            .unwrap_or(0);

        let end = args
            .get("end_line")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(lines.len());

        let selected_lines: Vec<String> = lines
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(|(i, line)| format!("{:>6}  {}", i + 1, line))
            .collect();

        Ok(json!({
            "path": path_str,
            "content": selected_lines.join("\n"),
            "total_lines": lines.len(),
            "lines_shown": selected_lines.len()
        }))
    }

    fn name(&self) -> &str {
        "read_file"
    }
}

pub struct WriteFileTool {
    workspace_root: PathBuf,
}

impl WriteFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "write_file".to_string(),
                description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "path".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("Path to the file (relative to workspace root)".to_string()),
                                items: None,
                            },
                        );
                        props.insert(
                            "content".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("Content to write to the file".to_string()),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["path".to_string(), "content".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::InvalidArguments {
                message: "Missing or invalid 'path' parameter".to_string(),
            }
        })?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "Missing or invalid 'content' parameter".to_string(),
            })?;

        let path = Path::new(path_str);
        let safe_path = validate_path_for_write(path, &self.workspace_root)?;

        if let Some(parent) = safe_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&safe_path, content)?;

        Ok(json!({
            "path": path_str,
            "bytes_written": content.len(),
            "success": true
        }))
    }

    fn name(&self) -> &str {
        "write_file"
    }
}

pub struct ListDirTool {
    workspace_root: PathBuf,
}

impl ListDirTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    #[allow(clippy::only_used_in_recursion)]
    fn list_recursive(
        &self,
        dir: &Path,
        root: &Path,
        pattern: Option<&str>,
        entries: &mut Vec<Value>,
    ) -> ToolResult<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            let name = entry.file_name().to_string_lossy().to_string();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();

            if file_type.is_dir() {
                self.list_recursive(&path, root, pattern, entries)?;
            } else {
                if let Some(pat) = pattern {
                    if !glob::Pattern::new(pat)
                        .map_err(|e| ToolError::InvalidArguments {
                            message: format!("Invalid glob pattern: {}", e),
                        })?
                        .matches(&name)
                    {
                        continue;
                    }
                }

                entries.push(json!({
                    "name": name,
                    "path": relative,
                    "is_dir": false,
                    "is_file": true,
                }));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "list_directory".to_string(),
                description: "List files and directories in a path.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "path".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Path to list (relative to workspace root, defaults to '.')"
                                        .to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "recursive".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Boolean,
                                description: Some(
                                    "Whether to list recursively (default: false)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "pattern".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Glob pattern to filter files (e.g., '*.rs')".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec![]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let path = Path::new(path_str);
        let safe_path = validate_path_within_workspace(path, &self.workspace_root)?;

        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let pattern = args.get("pattern").and_then(|v| v.as_str());

        let mut entries = Vec::new();

        if recursive {
            self.list_recursive(&safe_path, &self.workspace_root, pattern, &mut entries)?;
        } else {
            for entry in std::fs::read_dir(&safe_path)? {
                let entry = entry?;
                let file_type = entry.file_type()?;
                let name = entry.file_name().to_string_lossy().to_string();

                if let Some(pat) = pattern {
                    if !glob::Pattern::new(pat)
                        .map_err(|e| ToolError::InvalidArguments {
                            message: format!("Invalid glob pattern: {}", e),
                        })?
                        .matches(&name)
                    {
                        continue;
                    }
                }

                entries.push(json!({
                    "name": name,
                    "is_dir": file_type.is_dir(),
                    "is_file": file_type.is_file(),
                }));
            }
        }

        Ok(json!({
            "path": path_str,
            "entries": entries,
            "count": entries.len()
        }))
    }

    fn name(&self) -> &str {
        "list_directory"
    }
}
