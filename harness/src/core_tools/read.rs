use super::{error_outcome, io_outcome, read_text, too_large, MAX_FILE_BYTES};
use crate::effects::EffectClass;
use crate::scope::{resolve_path, PathAccess, PathScope};
use crate::tool_contract::{ErrorCode, ToolOutcome, TypedTool};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadFileArgs {
    #[schemars(description = "Path to the file, relative to the workspace root")]
    pub path: String,
    #[schemars(description = "First line to return, 1-based (default 1)")]
    pub start_line: Option<u32>,
    #[schemars(description = "Last line to return, inclusive (default end of file)")]
    pub end_line: Option<u32>,
}

pub struct ReadFileTool {
    root: PathBuf,
    scope: Option<PathScope>,
}

impl ReadFileTool {
    pub fn new(root: PathBuf, scope: Option<PathScope>) -> Self {
        Self { root, scope }
    }
}

#[async_trait]
impl TypedTool for ReadFileTool {
    type Args = ReadFileArgs;

    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a text file. The content is the raw file text with no line-number gutter; pass it to replace exactly as returned. Use start_line and end_line to read a range."
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }

    async fn run(&self, args: ReadFileArgs) -> ToolOutcome {
        let safe = match resolve_path(
            self.scope.as_ref(),
            "read_file",
            PathAccess::Read,
            Path::new(&args.path),
            &self.root,
        ) {
            Ok(path) => path,
            Err(error) => return error_outcome(error, &self.root, &args.path),
        };
        let metadata = match std::fs::metadata(&safe) {
            Ok(metadata) => metadata,
            Err(error) => return io_outcome(&error, &args.path),
        };
        if metadata.is_dir() {
            return ToolOutcome::failure(
                ErrorCode::InvalidArgs,
                format!("{} is a directory, not a file", args.path),
            )
            .with_hint("use list_directory to see its entries");
        }
        if metadata.len() > MAX_FILE_BYTES {
            return too_large(&args.path);
        }
        let mut file = match std::fs::File::open(&safe) {
            Ok(file) => file,
            Err(error) => return io_outcome(&error, &args.path),
        };
        let content = match read_text(&mut file, &args.path) {
            Ok(content) => content,
            Err(outcome) => return outcome,
        };
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start = args.start_line.unwrap_or(1).max(1) as usize;
        if start > total.max(1) {
            return ToolOutcome::failure(
                ErrorCode::InvalidArgs,
                format!(
                    "start_line {start} is past the end of {} ({total} lines)",
                    args.path
                ),
            );
        }
        if let Some(end) = args.end_line {
            if (end as usize) < start {
                return ToolOutcome::failure(
                    ErrorCode::InvalidArgs,
                    format!("end_line {end} is before start_line {start}"),
                );
            }
        }
        let end = args.end_line.map_or(total, |end| (end as usize).min(total));
        let selected = if total == 0 {
            String::new()
        } else {
            lines[start - 1..end].join("\n")
        };
        ToolOutcome::success(json!({
            "path": args.path,
            "total_lines": total,
            "start_line": start,
            "end_line": end,
            "content": selected,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::PathScope;
    use crate::tool_contract::Typed;
    use crate::tools::Tool;
    use serde_json::{json, Value};
    use tempfile::TempDir;

    fn tool(dir: &TempDir, scope: Option<PathScope>) -> Typed<ReadFileTool> {
        Typed(ReadFileTool::new(dir.path().to_path_buf(), scope))
    }

    async fn run(tool: &Typed<ReadFileTool>, args: Value) -> Value {
        tool.execute(args).await.unwrap()
    }

    #[tokio::test]
    async fn reads_content_without_a_line_number_gutter() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();

        let result = run(&tool(&dir, None), json!({"path": "a.txt"})).await;

        assert_eq!(result["ok"], true);
        assert_eq!(result["data"]["content"], "alpha\nbeta\ngamma");
        assert_eq!(result["data"]["total_lines"], 3);
        assert_eq!(result["data"]["start_line"], 1);
        assert_eq!(result["data"]["path"], "a.txt");
    }

    #[tokio::test]
    async fn reads_an_inclusive_line_range() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "1\n2\n3\n4\n5\n").unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "start_line": 2, "end_line": 4}),
        )
        .await;

        assert_eq!(result["data"]["content"], "2\n3\n4");
        assert_eq!(result["data"]["start_line"], 2);
        assert_eq!(result["data"]["total_lines"], 5);
    }

    #[tokio::test]
    async fn an_end_line_past_the_end_is_clamped() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "1\n2\n").unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "start_line": 2, "end_line": 99}),
        )
        .await;

        assert_eq!(result["data"]["content"], "2");
    }

    #[tokio::test]
    async fn a_start_line_after_the_end_line_is_invalid() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "1\n2\n3\n").unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "a.txt", "start_line": 3, "end_line": 1}),
        )
        .await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn a_start_line_beyond_the_file_is_invalid_and_reports_the_length() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "1\n2\n").unwrap();

        let result = run(&tool(&dir, None), json!({"path": "a.txt", "start_line": 9})).await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("2 lines"));
    }

    #[tokio::test]
    async fn a_directory_is_a_failure_pointing_at_list_directory() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        let result = run(&tool(&dir, None), json!({"path": "sub"})).await;

        assert_eq!(result["ok"], false);
        assert!(result["hint"].as_str().unwrap().contains("list_directory"));
    }

    #[tokio::test]
    async fn a_missing_file_is_not_found() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"path": "nope.txt"})).await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "not_found");
    }

    #[tokio::test]
    async fn a_path_escaping_the_workspace_is_out_of_scope() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"path": "/etc/hostname"})).await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "out_of_scope");
    }

    #[tokio::test]
    async fn read_scope_denies_files_outside_read_paths() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::create_dir(dir.path().join("secret")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "ok").unwrap();
        std::fs::write(dir.path().join("secret/b.rs"), "no").unwrap();
        let scope =
            PathScope::new("t", &["src/**".to_string()], Some(&["src/**".to_string()])).unwrap();
        let tool = tool(&dir, Some(scope));

        let allowed = run(&tool, json!({"path": "src/a.rs"})).await;
        let denied = run(&tool, json!({"path": "secret/b.rs"})).await;

        assert_eq!(allowed["ok"], true);
        assert_eq!(denied["ok"], false);
        assert_eq!(denied["error_code"], "out_of_scope");
    }

    #[tokio::test]
    async fn binary_content_is_invalid_args() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("b.bin"), [0xff, 0xfe, 0x00]).unwrap();

        let result = run(&tool(&dir, None), json!({"path": "b.bin"})).await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
    }

    #[tokio::test]
    async fn crlf_files_read_without_carriage_returns() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\r\nb\r\n").unwrap();

        let result = run(&tool(&dir, None), json!({"path": "a.txt"})).await;

        assert_eq!(result["data"]["content"], "a\nb");
    }

    #[tokio::test]
    async fn an_unknown_argument_is_rejected_by_name() {
        let dir = TempDir::new().unwrap();

        let result = run(&tool(&dir, None), json!({"path": "a", "offset": 3})).await;

        assert_eq!(result["ok"], false);
        assert_eq!(result["error_code"], "invalid_args");
        assert!(result["message"].as_str().unwrap().contains("offset"));
    }

    #[test]
    fn the_schema_requires_only_path() {
        let schema = Typed(ReadFileTool::new(std::path::PathBuf::from("."), None))
            .definition()
            .function
            .raw_parameters
            .unwrap();

        assert_eq!(schema["required"], json!(["path"]));
        assert_eq!(schema["additionalProperties"], false);
    }
}
