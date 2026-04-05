use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

use super::registry::{Tool, ToolError, ToolResult};

pub struct GitStatusTool {
    workspace_root: PathBuf,
}

impl GitStatusTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for GitStatusTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "git_status".to_string(),
                description: "Get the current git repository status including branch, staged files, and modified files.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some(HashMap::new()),
                    required: Some(vec![]),
                },
            },
        }
    }

    async fn execute(&self, _args: Value) -> ToolResult<Value> {
        use crate::entities::git::GitRepository;

        let repo = GitRepository::detect(&self.workspace_root).ok_or_else(|| {
            ToolError::ExecutionFailed {
                message: "Not a git repository".to_string(),
            }
        })?;

        Ok(json!({
            "branch": repo.current_branch,
            "commit": repo.head_commit,
            "is_dirty": repo.is_dirty,
            "staged_files": repo.staged_files,
            "modified_files": repo.modified_files,
            "untracked_files": repo.untracked_files,
            "summary": repo.summary()
        }))
    }

    fn name(&self) -> &str {
        "git_status"
    }
}

pub struct GitDiffTool {
    workspace_root: PathBuf,
}

impl GitDiffTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for GitDiffTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "git_diff".to_string(),
                description: "Show git diff for files. Can show staged or unstaged changes."
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
                                    "Path to diff (optional, defaults to all files)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "staged".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Boolean,
                                description: Some(
                                    "Show staged changes instead of unstaged (default: false)"
                                        .to_string(),
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
        let path = args.get("path").and_then(|v| v.as_str());
        let staged = args
            .get("staged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(&self.workspace_root);

        if staged {
            cmd.args(["diff", "--cached"]);
        } else {
            cmd.arg("diff");
        }

        if let Some(p) = path {
            cmd.arg("--").arg(p);
        }

        let output = cmd.output().map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to run git diff: {}", e),
        })?;

        if !output.status.success() {
            return Err(ToolError::ExecutionFailed {
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        let diff = String::from_utf8_lossy(&output.stdout).to_string();

        Ok(json!({
            "diff": diff,
            "staged": staged,
            "path": path.unwrap_or("(all files)"),
            "has_changes": !diff.is_empty()
        }))
    }

    fn name(&self) -> &str {
        "git_diff"
    }
}

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
