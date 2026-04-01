use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

use super::{Tool, ToolError, ToolResult};

// ---------------------------------------------------------------------------
// GitStatusTool
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// GitDiffTool
// ---------------------------------------------------------------------------

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
