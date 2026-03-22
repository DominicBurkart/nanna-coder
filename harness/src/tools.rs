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

/// GitHub API connection status for transparent degradation.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum GitHubStatus {
    /// Successfully connected to GitHub API.
    Connected,
    /// No GITHUB_TOKEN environment variable configured.
    #[default]
    NoToken,
    /// API call failed with an error message.
    ApiError(String),
}

/// GitHub PR status data collected from git and GitHub REST API.
#[derive(Debug, Clone, Default)]
pub struct PrStatusData {
    /// PR number (e.g., "#42")
    pub pr_number: Option<u64>,
    /// Linked issue number (e.g., "#17")
    pub issue_number: Option<u64>,
    /// PR status: "draft", "ready", "merged", "closed"
    pub pr_status: Option<String>,
    /// Review state: "approved", "changes-requested", "review-required"
    pub review_state: Option<String>,
    /// Number of files with merge conflicts
    pub conflict_count: Option<usize>,
    /// List of conflicting file paths
    pub conflict_files: Vec<String>,
    /// Commits ahead of upstream
    pub ahead: Option<usize>,
    /// Commits behind upstream
    pub behind: Option<usize>,
    /// Lines added
    pub additions: Option<usize>,
    /// Lines deleted
    pub deletions: Option<usize>,
    /// CI status: "pass", "fail", "pending"
    pub ci_status: Option<String>,
    /// Failing CI check names
    pub ci_failing_checks: Vec<String>,
    /// Whether automerge is enabled
    pub automerge: bool,
    /// Days since last update (staleness)
    pub staleness_days: Option<u64>,
    /// Current branch name
    pub branch: Option<String>,
    /// Short HEAD commit SHA (fallback when no PR)
    pub head_sha: Option<String>,
    /// Whether this branch has an upstream
    pub has_upstream: bool,
    /// Changed file paths (for diff detail)
    pub changed_files: Vec<String>,
    /// GitHub API connection status
    pub github_status: GitHubStatus,
}

impl PrStatusData {
    /// Format as L0 compact single-line status.
    ///
    /// Only includes salient (non-default) fields. Omitted fields represent
    /// non-salient states (e.g., no conflicts, automerge disabled).
    pub fn to_l0(&self) -> String {
        let mut parts = Vec::new();

        // PR number or commit SHA as context anchor
        if let Some(pr) = self.pr_number {
            parts.push(format!("#{}", pr));
        } else if let Some(ref sha) = self.head_sha {
            parts.push(sha.clone());
        }

        // Linked issue
        if let Some(issue) = self.issue_number {
            parts.push(format!("#{}", issue));
        }

        // PR status (only show draft, since "ready" is implied if reviews exist)
        if let Some(ref status) = self.pr_status {
            match status.as_str() {
                "draft" => parts.push("draft".to_string()),
                "merged" => parts.push("merged".to_string()),
                "closed" => parts.push("closed".to_string()),
                "ready" => parts.push("ready".to_string()),
                _ => {}
            }
        }

        // Review state (only show when salient)
        if let Some(ref review) = self.review_state {
            match review.as_str() {
                "approved" => parts.push("approved".to_string()),
                "changes-requested" => parts.push("changes-requested".to_string()),
                _ => {}
            }
        }

        // Merge conflicts
        if let Some(count) = self.conflict_count {
            if count > 0 {
                parts.push(format!("conflicts:{}", count));
            }
        }

        // Sync state (ahead/behind)
        if !self.has_upstream {
            parts.push("no-upstream".to_string());
        } else {
            if let Some(behind) = self.behind {
                if behind > 0 {
                    parts.push(format!("behind:{}", behind));
                }
            }
            if let Some(ahead) = self.ahead {
                if ahead > 0 {
                    parts.push(format!("ahead:{}", ahead));
                }
            }
        }

        // Diff stats
        if let (Some(a), Some(d)) = (self.additions, self.deletions) {
            parts.push(format!("+{}/-{}", a, d));
        }

        // CI status
        if let Some(ref ci) = self.ci_status {
            match ci.as_str() {
                "pass" => parts.push("ci:pass".to_string()),
                "fail" => parts.push("ci:fail".to_string()),
                "pending" => parts.push("ci:pending".to_string()),
                _ => {}
            }
        }

        // Automerge (only show when enabled, since disabled is default)
        if self.automerge {
            parts.push("automerge".to_string());
        }

        // Staleness
        if let Some(days) = self.staleness_days {
            if days > 0 {
                parts.push(format!("{}d", days));
            }
        }

        // GitHub connection status (visible degradation)
        match &self.github_status {
            GitHubStatus::Connected => {}
            GitHubStatus::NoToken => parts.push("[github:unconfigured]".to_string()),
            GitHubStatus::ApiError(_) => parts.push("[github:error]".to_string()),
        }

        parts.join(" ")
    }

    /// Format L1 detail for a specific field.
    pub fn to_l1(&self, field: &str) -> Result<String, String> {
        match field {
            "conflicts" => {
                if self.conflict_files.is_empty() {
                    Ok("No merge conflicts.".to_string())
                } else {
                    Ok(format!(
                        "Conflicting files ({}):\n{}",
                        self.conflict_files.len(),
                        self.conflict_files
                            .iter()
                            .map(|f| format!("  - {}", f))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ))
                }
            }
            "ci" => {
                let status = self.ci_status.as_deref().unwrap_or("unknown");
                if self.ci_failing_checks.is_empty() {
                    Ok(format!("CI status: {}", status))
                } else {
                    Ok(format!(
                        "CI status: {}\nFailing checks:\n{}",
                        status,
                        self.ci_failing_checks
                            .iter()
                            .map(|c| format!("  - {}", c))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ))
                }
            }
            "diff" => {
                let stats = match (self.additions, self.deletions) {
                    (Some(a), Some(d)) => format!("+{}/-{}", a, d),
                    _ => "no diff data".to_string(),
                };
                if self.changed_files.is_empty() {
                    Ok(format!("Diff: {}", stats))
                } else {
                    Ok(format!(
                        "Diff: {}\nChanged files ({}):\n{}",
                        stats,
                        self.changed_files.len(),
                        self.changed_files
                            .iter()
                            .map(|f| format!("  - {}", f))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ))
                }
            }
            "sync" => {
                if !self.has_upstream {
                    Ok("No upstream tracking branch configured.".to_string())
                } else {
                    let ahead = self.ahead.unwrap_or(0);
                    let behind = self.behind.unwrap_or(0);
                    Ok(format!(
                        "Sync: {} ahead, {} behind upstream",
                        ahead, behind
                    ))
                }
            }
            "review" => {
                let state = self.review_state.as_deref().unwrap_or("none");
                Ok(format!("Review state: {}", state))
            }
            "automerge" => Ok(format!(
                "Automerge: {}",
                if self.automerge {
                    "enabled"
                } else {
                    "disabled"
                }
            )),
            "staleness" => match self.staleness_days {
                Some(days) => Ok(format!("Last updated {} days ago", days)),
                None => Ok("Staleness data not available.".to_string()),
            },
            "github" => match &self.github_status {
                GitHubStatus::Connected => {
                    Ok("GitHub API: connected (token configured)".to_string())
                }
                GitHubStatus::NoToken => Ok(
                    "GitHub API: not configured. Set GITHUB_TOKEN env var with repo:status and read:org scopes to enable PR data, CI status, and review information.".to_string(),
                ),
                GitHubStatus::ApiError(msg) => {
                    Ok(format!("GitHub API: error — {}", msg))
                }
            },
            _ => Err(format!(
                "Unknown field '{}'. Valid fields: conflicts, ci, diff, sync, review, automerge, staleness, github",
                field
            )),
        }
    }
}

/// Collect PR status data from git and (optionally) the GitHub REST API.
fn collect_pr_status(workspace_root: &Path) -> ToolResult<PrStatusData> {
    let mut data = PrStatusData::default();

    // -- Git-based data --

    // Branch and HEAD SHA
    let branch_output = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(workspace_root)
        .output()
        .map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to get branch: {}", e),
        })?;
    if branch_output.status.success() {
        let branch = String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string();
        if !branch.is_empty() {
            data.branch = Some(branch);
        }
    }

    let sha_output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to get HEAD SHA: {}", e),
        })?;
    if sha_output.status.success() {
        data.head_sha = Some(
            String::from_utf8_lossy(&sha_output.stdout)
                .trim()
                .to_string(),
        );
    }

    // Ahead/behind upstream
    let tracking_output = std::process::Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .current_dir(workspace_root)
        .output();

    match tracking_output {
        Ok(ref output) if output.status.success() => {
            data.has_upstream = true;
            let counts = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = counts.trim().split('\t').collect();
            if parts.len() == 2 {
                data.ahead = parts[0].parse().ok();
                data.behind = parts[1].parse().ok();
            }
        }
        _ => {
            data.has_upstream = false;
        }
    }

    // Diff stats against upstream (or default branch)
    let diff_base = if data.has_upstream {
        "@{upstream}".to_string()
    } else {
        // Try origin/main, then origin/master
        let main_check = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "origin/main"])
            .current_dir(workspace_root)
            .output();
        if main_check.map(|o| o.status.success()).unwrap_or(false) {
            "origin/main".to_string()
        } else {
            "origin/master".to_string()
        }
    };

    let diff_stat_output = std::process::Command::new("git")
        .args(["diff", "--stat", &diff_base])
        .current_dir(workspace_root)
        .output();

    if let Ok(ref output) = diff_stat_output {
        if output.status.success() {
            let stat_text = String::from_utf8_lossy(&output.stdout);
            // Parse "X files changed, Y insertions(+), Z deletions(-)" from last line
            if let Some(last_line) = stat_text.lines().last() {
                let mut additions = 0usize;
                let mut deletions = 0usize;
                for part in last_line.split(',') {
                    let part = part.trim();
                    if part.contains("insertion") {
                        if let Some(n) = part.split_whitespace().next() {
                            additions = n.parse().unwrap_or(0);
                        }
                    } else if part.contains("deletion") {
                        if let Some(n) = part.split_whitespace().next() {
                            deletions = n.parse().unwrap_or(0);
                        }
                    }
                }
                data.additions = Some(additions);
                data.deletions = Some(deletions);
            }
        }
    }

    // Changed files list (for L1 diff detail)
    let diff_files_output = std::process::Command::new("git")
        .args(["diff", "--name-only", &diff_base])
        .current_dir(workspace_root)
        .output();

    if let Ok(ref output) = diff_files_output {
        if output.status.success() {
            data.changed_files = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
        }
    }

    // Merge conflicts (check for unmerged paths)
    let conflict_output = std::process::Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .current_dir(workspace_root)
        .output();

    if let Ok(ref output) = conflict_output {
        if output.status.success() {
            let files: Vec<String> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
            if !files.is_empty() {
                data.conflict_count = Some(files.len());
                data.conflict_files = files;
            }
        }
    }

    // -- GitHub REST API data (explicit degradation) --
    let token = std::env::var("GITHUB_TOKEN").ok();

    if let Some(ref token) = token {
        match fetch_github_pr_data(workspace_root, &data, token) {
            Ok(gh_data) => {
                data.pr_number = gh_data.pr_number.or(data.pr_number);
                data.issue_number = gh_data.issue_number.or(data.issue_number);
                data.pr_status = gh_data.pr_status.or(data.pr_status);
                data.review_state = gh_data.review_state.or(data.review_state);
                data.ci_status = gh_data.ci_status.or(data.ci_status);
                data.ci_failing_checks = if gh_data.ci_failing_checks.is_empty() {
                    data.ci_failing_checks
                } else {
                    gh_data.ci_failing_checks
                };
                data.automerge = gh_data.automerge;
                data.staleness_days = gh_data.staleness_days.or(data.staleness_days);
                if let Some(count) = gh_data.conflict_count {
                    if data.conflict_count.is_none() {
                        data.conflict_count = Some(count);
                    }
                }
                data.github_status = GitHubStatus::Connected;
            }
            Err(e) => {
                data.github_status = GitHubStatus::ApiError(e);
            }
        }
    }
    // else: data.github_status remains NoToken (the default)

    Ok(data)
}

/// Parse a GitHub remote URL into (owner, repo).
fn parse_github_remote(url: &str) -> Option<(String, String)> {
    // Handle SSH: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let path = rest.trim_end_matches(".git");
        let (owner, repo) = path.split_once('/')?;
        if !owner.is_empty() && !repo.is_empty() {
            return Some((owner.to_string(), repo.to_string()));
        }
    }
    // Handle HTTPS: https://github.com/owner/repo.git
    if url.contains("github.com") {
        let path = url
            .split("github.com")
            .nth(1)?
            .trim_start_matches('/')
            .trim_start_matches(':')
            .trim_end_matches(".git");
        let (owner, repo) = path.split_once('/')?;
        if !owner.is_empty() && !repo.is_empty() {
            return Some((owner.to_string(), repo.to_string()));
        }
    }
    None
}

/// Fetch PR data from the GitHub REST API. Returns partial data on success,
/// or an error message string on failure.
fn fetch_github_pr_data(
    workspace_root: &Path,
    local_data: &PrStatusData,
    token: &str,
) -> Result<PrStatusData, String> {
    let mut gh_data = PrStatusData::default();

    // Get remote URL
    let remote_output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(workspace_root)
        .output()
        .map_err(|e| format!("failed to get git remote: {}", e))?;

    if !remote_output.status.success() {
        return Err("no 'origin' remote configured".to_string());
    }

    let remote_url = String::from_utf8_lossy(&remote_output.stdout)
        .trim()
        .to_string();
    let (owner, repo) =
        parse_github_remote(&remote_url).ok_or_else(|| "not a GitHub remote".to_string())?;

    let branch = local_data
        .branch
        .as_deref()
        .ok_or_else(|| "no branch detected".to_string())?;

    let client = reqwest::blocking::Client::new();
    let api_base = "https://api.github.com";

    // Find PR for current branch
    let pr_url = format!(
        "{}/repos/{}/{}/pulls?head={}:{}&state=open",
        api_base, owner, repo, owner, branch
    );
    let pr_resp = client
        .get(&pr_url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nanna-coder-harness")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|e| format!("GitHub API request failed: {}", e))?;

    if !pr_resp.status().is_success() {
        let status = pr_resp.status();
        return Err(format!("GitHub API returned {}", status));
    }

    let prs: Vec<Value> = pr_resp
        .json()
        .map_err(|e| format!("failed to parse PR response: {}", e))?;

    let pr_json = match prs.first() {
        Some(pr) => pr,
        None => return Ok(gh_data), // No open PR for this branch — not an error
    };

    // PR number
    gh_data.pr_number = pr_json.get("number").and_then(|v| v.as_u64());

    // PR status (draft/ready/merged/closed)
    let is_draft = pr_json
        .get("draft")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let state = pr_json.get("state").and_then(|v| v.as_str()).unwrap_or("");
    gh_data.pr_status = Some(match (state, is_draft) {
        (_, true) => "draft".to_string(),
        ("closed", _) => "closed".to_string(),
        _ => "ready".to_string(),
    });

    // Merge conflicts from mergeable_state
    if let Some(mergeable_state) = pr_json.get("mergeable_state").and_then(|v| v.as_str()) {
        if mergeable_state == "dirty" {
            gh_data.conflict_count = Some(1);
        }
    }

    // Automerge
    gh_data.automerge = pr_json
        .get("auto_merge")
        .map(|v| !v.is_null())
        .unwrap_or(false);

    // Staleness
    if let Some(updated_at) = pr_json.get("updated_at").and_then(|v| v.as_str()) {
        if let Ok(updated) = chrono::DateTime::parse_from_rfc3339(updated_at) {
            let now = chrono::Utc::now();
            let duration = now.signed_duration_since(updated);
            gh_data.staleness_days = Some(duration.num_days() as u64);
        }
    }

    // Linked issues from body (look for "Closes #N" / "Fixes #N" patterns)
    if let Some(body) = pr_json.get("body").and_then(|v| v.as_str()) {
        let issue_re =
            regex::Regex::new(r"(?i)(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#(\d+)").ok();
        if let Some(re) = issue_re {
            if let Some(caps) = re.captures(body) {
                if let Some(num) = caps.get(1).and_then(|m| m.as_str().parse::<u64>().ok()) {
                    gh_data.issue_number = Some(num);
                }
            }
        }
    }

    let pr_number = match gh_data.pr_number {
        Some(n) => n,
        None => return Ok(gh_data),
    };

    // Fetch reviews for review decision
    let reviews_url = format!(
        "{}/repos/{}/{}/pulls/{}/reviews",
        api_base, owner, repo, pr_number
    );
    if let Ok(reviews_resp) = client
        .get(&reviews_url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nanna-coder-harness")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
    {
        if reviews_resp.status().is_success() {
            if let Ok(reviews) = reviews_resp.json::<Vec<Value>>() {
                // Use the last substantive review state per reviewer
                let mut latest_states: HashMap<String, String> = HashMap::new();
                for review in &reviews {
                    let user = review
                        .get("user")
                        .and_then(|u| u.get("login"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let state = review.get("state").and_then(|v| v.as_str()).unwrap_or("");
                    if state == "APPROVED" || state == "CHANGES_REQUESTED" || state == "DISMISSED" {
                        latest_states.insert(user.to_string(), state.to_string());
                    }
                }
                if latest_states.values().any(|s| s == "CHANGES_REQUESTED") {
                    gh_data.review_state = Some("changes-requested".to_string());
                } else if latest_states.values().any(|s| s == "APPROVED") {
                    gh_data.review_state = Some("approved".to_string());
                } else if !latest_states.is_empty() {
                    gh_data.review_state = Some("review-required".to_string());
                }
            }
        }
    }

    // Fetch CI status via check-runs
    let head_sha = pr_json
        .get("head")
        .and_then(|h| h.get("sha"))
        .and_then(|v| v.as_str());
    if let Some(sha) = head_sha {
        let checks_url = format!(
            "{}/repos/{}/{}/commits/{}/check-runs",
            api_base, owner, repo, sha
        );
        if let Ok(checks_resp) = client
            .get(&checks_url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "nanna-coder-harness")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
        {
            if checks_resp.status().is_success() {
                if let Ok(checks_json) = checks_resp.json::<Value>() {
                    if let Some(check_runs) =
                        checks_json.get("check_runs").and_then(|v| v.as_array())
                    {
                        let mut has_fail = false;
                        let mut has_pending = false;
                        let mut failing_names = Vec::new();

                        for check in check_runs {
                            let conclusion = check
                                .get("conclusion")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let status = check.get("status").and_then(|v| v.as_str()).unwrap_or("");
                            let name = check
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");

                            if conclusion == "failure" || conclusion == "timed_out" {
                                has_fail = true;
                                failing_names.push(name.to_string());
                            } else if status == "queued"
                                || status == "in_progress"
                                || status == "waiting"
                            {
                                has_pending = true;
                            }
                        }

                        gh_data.ci_status = Some(if has_fail {
                            "fail".to_string()
                        } else if has_pending {
                            "pending".to_string()
                        } else {
                            "pass".to_string()
                        });
                        gh_data.ci_failing_checks = failing_names;
                    }
                }
            }
        }
    }

    Ok(gh_data)
}

pub struct GitHubPrStatusTool {
    workspace_root: PathBuf,
}

impl GitHubPrStatusTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for GitHubPrStatusTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "github_pr_status".to_string(),
                description: "Get GitHub PR status. L0: compact single-line status. L1: detailed expansion of a specific field.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "level".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Detail level: 'l0' for compact status line (default), 'l1' for expanded field detail".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "field".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Field to expand (required for l1). Options: conflicts, ci, diff, sync, review, automerge, staleness, github".to_string(),
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
        let level = args.get("level").and_then(|v| v.as_str()).unwrap_or("l0");

        let data = collect_pr_status(&self.workspace_root)?;
        let github_connected = data.github_status == GitHubStatus::Connected;

        match level {
            "l0" => {
                let status_line = data.to_l0();
                Ok(json!({
                    "level": "l0",
                    "status": status_line,
                    "github_connected": github_connected
                }))
            }
            "l1" => {
                let field = args.get("field").and_then(|v| v.as_str()).ok_or_else(|| {
                    ToolError::InvalidArguments {
                        message: "Missing 'field' parameter for l1 query".to_string(),
                    }
                })?;

                let detail = data
                    .to_l1(field)
                    .map_err(|e| ToolError::InvalidArguments { message: e })?;

                Ok(json!({
                    "level": "l1",
                    "field": field,
                    "detail": detail,
                    "github_connected": github_connected
                }))
            }
            _ => Err(ToolError::InvalidArguments {
                message: format!("Invalid level '{}'. Use 'l0' or 'l1'.", level),
            }),
        }
    }

    fn name(&self) -> &str {
        "github_pr_status"
    }
}

pub fn create_tool_registry(workspace_root: &std::path::Path) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    registry.register(Box::new(CalculatorTool::new()));
    registry.register(Box::new(ReadFileTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(WriteFileTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(ListDirTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(SearchTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(GitStatusTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(GitDiffTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(GitHubPrStatusTool::new(
        workspace_root.to_path_buf(),
    )));
    registry
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

    // -- PrStatusData unit tests --

    #[test]
    fn test_pr_status_l0_full() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: Some(17),
            pr_status: Some("draft".to_string()),
            review_state: None,
            conflict_count: Some(3),
            conflict_files: vec![
                "src/a.rs".to_string(),
                "src/b.rs".to_string(),
                "src/c.rs".to_string(),
            ],
            ahead: Some(0),
            behind: Some(0),
            additions: Some(66),
            deletions: Some(233),
            ci_status: Some("fail".to_string()),
            ci_failing_checks: vec!["lint".to_string()],
            automerge: false,
            staleness_days: None,
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#42 #17 draft conflicts:3 +66/-233 ci:fail");
    }

    #[test]
    fn test_pr_status_l0_ready_approved() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: Some(17),
            pr_status: Some("ready".to_string()),
            review_state: Some("approved".to_string()),
            conflict_count: None,
            conflict_files: vec![],
            ahead: Some(0),
            behind: Some(0),
            additions: Some(12),
            deletions: Some(5),
            ci_status: Some("pass".to_string()),
            ci_failing_checks: vec![],
            automerge: true,
            staleness_days: Some(2),
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#42 #17 ready approved +12/-5 ci:pass automerge 2d");
    }

    #[test]
    fn test_pr_status_l0_no_upstream() {
        let data = PrStatusData {
            pr_number: None,
            issue_number: None,
            pr_status: None,
            review_state: None,
            conflict_count: None,
            conflict_files: vec![],
            ahead: None,
            behind: None,
            additions: Some(5),
            deletions: Some(2),
            ci_status: None,
            ci_failing_checks: vec![],
            automerge: false,
            staleness_days: None,
            branch: None,
            head_sha: Some("abc123".to_string()),
            has_upstream: false,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "abc123 no-upstream +5/-2");
    }

    #[test]
    fn test_pr_status_l0_behind() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: None,
            pr_status: Some("ready".to_string()),
            review_state: None,
            conflict_count: None,
            conflict_files: vec![],
            ahead: Some(0),
            behind: Some(3),
            additions: Some(66),
            deletions: Some(233),
            ci_status: None,
            ci_failing_checks: vec![],
            automerge: false,
            staleness_days: None,
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#42 ready behind:3 +66/-233");
    }

    #[test]
    fn test_pr_status_l0_changes_requested_with_conflicts() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: Some(17),
            pr_status: Some("ready".to_string()),
            review_state: Some("changes-requested".to_string()),
            conflict_count: Some(1),
            conflict_files: vec!["src/main.rs".to_string()],
            ahead: Some(0),
            behind: Some(0),
            additions: Some(100),
            deletions: Some(50),
            ci_status: Some("pending".to_string()),
            ci_failing_checks: vec![],
            automerge: false,
            staleness_days: None,
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(
            l0,
            "#42 #17 ready changes-requested conflicts:1 +100/-50 ci:pending"
        );
    }

    #[test]
    fn test_pr_status_l0_merged() {
        let data = PrStatusData {
            pr_number: Some(99),
            pr_status: Some("merged".to_string()),
            additions: Some(0),
            deletions: Some(0),
            has_upstream: true,
            ci_status: Some("pass".to_string()),
            github_status: GitHubStatus::Connected,
            ..Default::default()
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#99 merged +0/-0 ci:pass");
    }

    #[test]
    fn test_pr_status_l0_minimal() {
        // Minimal: just a commit SHA, no upstream, no diff
        let data = PrStatusData {
            head_sha: Some("def456".to_string()),
            has_upstream: false,
            github_status: GitHubStatus::Connected,
            ..Default::default()
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "def456 no-upstream");
    }

    #[test]
    fn test_pr_status_l1_conflicts() {
        let data = PrStatusData {
            conflict_count: Some(2),
            conflict_files: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            ..Default::default()
        };

        let detail = data.to_l1("conflicts").unwrap();
        assert!(detail.contains("Conflicting files (2)"));
        assert!(detail.contains("src/a.rs"));
        assert!(detail.contains("src/b.rs"));
    }

    #[test]
    fn test_pr_status_l1_conflicts_none() {
        let data = PrStatusData::default();
        let detail = data.to_l1("conflicts").unwrap();
        assert_eq!(detail, "No merge conflicts.");
    }

    #[test]
    fn test_pr_status_l1_ci_failing() {
        let data = PrStatusData {
            ci_status: Some("fail".to_string()),
            ci_failing_checks: vec!["lint".to_string(), "test-unit".to_string()],
            ..Default::default()
        };

        let detail = data.to_l1("ci").unwrap();
        assert!(detail.contains("CI status: fail"));
        assert!(detail.contains("lint"));
        assert!(detail.contains("test-unit"));
    }

    #[test]
    fn test_pr_status_l1_ci_passing() {
        let data = PrStatusData {
            ci_status: Some("pass".to_string()),
            ..Default::default()
        };

        let detail = data.to_l1("ci").unwrap();
        assert_eq!(detail, "CI status: pass");
    }

    #[test]
    fn test_pr_status_l1_diff() {
        let data = PrStatusData {
            additions: Some(50),
            deletions: Some(20),
            changed_files: vec!["src/main.rs".to_string(), "Cargo.toml".to_string()],
            ..Default::default()
        };

        let detail = data.to_l1("diff").unwrap();
        assert!(detail.contains("+50/-20"));
        assert!(detail.contains("Changed files (2)"));
        assert!(detail.contains("src/main.rs"));
        assert!(detail.contains("Cargo.toml"));
    }

    #[test]
    fn test_pr_status_l1_sync_no_upstream() {
        let data = PrStatusData {
            has_upstream: false,
            ..Default::default()
        };

        let detail = data.to_l1("sync").unwrap();
        assert_eq!(detail, "No upstream tracking branch configured.");
    }

    #[test]
    fn test_pr_status_l1_sync_with_upstream() {
        let data = PrStatusData {
            has_upstream: true,
            ahead: Some(3),
            behind: Some(1),
            ..Default::default()
        };

        let detail = data.to_l1("sync").unwrap();
        assert_eq!(detail, "Sync: 3 ahead, 1 behind upstream");
    }

    #[test]
    fn test_pr_status_l1_review() {
        let data = PrStatusData {
            review_state: Some("approved".to_string()),
            ..Default::default()
        };

        let detail = data.to_l1("review").unwrap();
        assert_eq!(detail, "Review state: approved");
    }

    #[test]
    fn test_pr_status_l1_automerge() {
        let data = PrStatusData {
            automerge: true,
            ..Default::default()
        };

        let detail = data.to_l1("automerge").unwrap();
        assert_eq!(detail, "Automerge: enabled");

        let data2 = PrStatusData::default();
        let detail2 = data2.to_l1("automerge").unwrap();
        assert_eq!(detail2, "Automerge: disabled");
    }

    #[test]
    fn test_pr_status_l1_staleness() {
        let data = PrStatusData {
            staleness_days: Some(5),
            ..Default::default()
        };

        let detail = data.to_l1("staleness").unwrap();
        assert_eq!(detail, "Last updated 5 days ago");
    }

    #[test]
    fn test_pr_status_l1_unknown_field() {
        let data = PrStatusData::default();
        let result = data.to_l1("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown field"));
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_definition() {
        let tool = GitHubPrStatusTool::new(PathBuf::from("/tmp"));
        let def = tool.definition();
        assert_eq!(def.function.name, "github_pr_status");
        assert!(def.function.description.contains("PR status"));

        let params = &def.function.parameters;
        let props = params.properties.as_ref().unwrap();
        assert!(props.contains_key("level"));
        assert!(props.contains_key("field"));
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_name() {
        let tool = GitHubPrStatusTool::new(PathBuf::from("/tmp"));
        assert_eq!(tool.name(), "github_pr_status");
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_l0() {
        // Test the tool executes in the current repo (git-based data only, gh may not be available)
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({});
        let result = tool.execute(args).await;

        if let Ok(status) = result {
            assert_eq!(status["level"], "l0");
            assert!(status.get("status").is_some());
            let status_str = status["status"].as_str().unwrap();
            // Should contain at least a SHA or PR number
            assert!(!status_str.is_empty());
        }
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_l1() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({ "level": "l1", "field": "sync" });
        let result = tool.execute(args).await;

        if let Ok(detail) = result {
            assert_eq!(detail["level"], "l1");
            assert_eq!(detail["field"], "sync");
            assert!(detail.get("detail").is_some());
        }
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_l1_missing_field() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({ "level": "l1" });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_invalid_level() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({ "level": "l2" });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_github_pr_status_tool_in_registry() {
        let cwd = std::env::current_dir().unwrap();
        let registry = create_tool_registry(&cwd);
        assert!(registry.get_tool("github_pr_status").is_some());
    }
}
