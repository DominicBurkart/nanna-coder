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

fn validate_path_within_workspace(path: &Path, workspace_root: &Path) -> ToolResult<PathBuf> {
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

fn validate_path_for_write(path: &Path, workspace_root: &Path) -> ToolResult<PathBuf> {
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

    #[tokio::test]
    async fn test_read_file_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_read");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let test_file = temp_dir.join("test.txt");
        std::fs::write(&test_file, "line 1\nline 2\nline 3\nline 4\nline 5").unwrap();

        let tool = ReadFileTool::new(temp_dir.clone());

        let args = json!({ "path": "test.txt" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["total_lines"], 5);
        assert_eq!(result["lines_shown"], 5);

        let args = json!({ "path": "test.txt", "start_line": 2, "end_line": 4 });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["lines_shown"], 3);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_read_file_path_security() {
        let temp_dir = std::env::temp_dir().join("nanna_test_security");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let tool = ReadFileTool::new(temp_dir.clone());

        let args = json!({ "path": "../../../etc/passwd" });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        match result {
            Err(ToolError::PathSecurityViolation { .. }) => {}
            _ => panic!("Expected PathSecurityViolation error"),
        }

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_write_file_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_write");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let tool = WriteFileTool::new(temp_dir.clone());

        let args = json!({
            "path": "output.txt",
            "content": "Hello, World!"
        });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["bytes_written"], 13);

        let content = std::fs::read_to_string(temp_dir.join("output.txt")).unwrap();
        assert_eq!(content, "Hello, World!");

        let args = json!({
            "path": "subdir/nested.txt",
            "content": "Nested file"
        });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["success"], true);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_list_directory_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_list");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("file1.rs"), "").unwrap();
        std::fs::write(temp_dir.join("file2.txt"), "").unwrap();
        std::fs::create_dir_all(temp_dir.join("subdir")).unwrap();

        let tool = ListDirTool::new(temp_dir.clone());

        let args = json!({});
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 3);

        let args = json!({ "pattern": "*.rs" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 1);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_search_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_search");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(
            temp_dir.join("test.rs"),
            "fn main() {\n    println!(\"hello\");\n}",
        )
        .unwrap();
        std::fs::write(temp_dir.join("other.txt"), "no match here").unwrap();

        let tool = SearchTool::new(temp_dir.clone());

        let args = json!({ "pattern": "println" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 1);

        let args = json!({ "pattern": "fn|println" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 2);

        let args = json!({ "pattern": "println", "file_pattern": "*.txt" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 0);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_git_status_tool() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitStatusTool::new(cwd);

        let args = json!({});
        let result = tool.execute(args).await;

        if let Ok(status) = result {
            assert!(status.get("branch").is_some());
            assert!(status.get("commit").is_some());
            assert!(status.get("is_dirty").is_some());
        }
    }

    #[tokio::test]
    async fn test_git_diff_tool() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitDiffTool::new(cwd);

        let args = json!({});
        let result = tool.execute(args).await;

        if let Ok(diff) = result {
            assert!(diff.get("diff").is_some());
            assert!(diff.get("has_changes").is_some());
        }
    }
}
