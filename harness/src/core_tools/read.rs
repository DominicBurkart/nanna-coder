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

const DEFAULT_WINDOW_LINES: usize = 200;
const MAX_CONTENT_CHARS: usize = 12_000;

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
        "Read a text file. The content is the raw file text with no line-number gutter; pass it to replace exactly as returned. Returns at most 200 lines unless end_line is given, and never more than 12000 characters; when truncated is true, call again with start_line set to next_start_line, or use grep to find the lines you need."
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
        let wanted_end = args.end_line.map_or_else(
            || total.min(start.saturating_add(DEFAULT_WINDOW_LINES - 1)),
            |end| (end as usize).min(total),
        );
        let window = if total == 0 {
            Window::default()
        } else {
            window(&lines, start, wanted_end)
        };
        let truncated = window.line_cut || window.end < args.end_line.map_or(total, |_| wanted_end);
        let mut data = json!({
            "path": args.path,
            "total_lines": total,
            "start_line": start,
            "end_line": window.end,
            "content": window.content,
            "truncated": truncated,
        });
        if window.line_cut {
            data["line_cut"] = json!(true);
        }
        if truncated && window.end < total {
            data["next_start_line"] = json!(window.end + 1);
        }
        ToolOutcome::success(data)
    }
}

#[derive(Default)]
struct Window {
    content: String,
    end: usize,
    line_cut: bool,
}

fn window(lines: &[&str], start: usize, end: usize) -> Window {
    let mut taken = Window::default();
    let mut used = 0;
    for (index, line) in lines.iter().enumerate().take(end).skip(start - 1) {
        let separator = usize::from(index >= start);
        let length = line.chars().count();
        if used + separator + length > MAX_CONTENT_CHARS {
            if index < start {
                taken.content = line.chars().take(MAX_CONTENT_CHARS).collect();
                taken.end = index + 1;
                taken.line_cut = true;
            }
            break;
        }
        if separator == 1 {
            taken.content.push('\n');
        }
        taken.content.push_str(line);
        used += separator + length;
        taken.end = index + 1;
    }
    taken
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

    fn numbered_lines(count: usize, width: usize) -> String {
        (1..=count)
            .map(|line| format!("{line:0>width$}\n"))
            .collect()
    }

    #[tokio::test]
    async fn a_long_file_returns_a_default_window_and_says_where_to_continue() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("long.rs"), numbered_lines(980, 8)).unwrap();

        let result = run(&tool(&dir, None), json!({"path": "long.rs"})).await;

        assert_eq!(result["data"]["total_lines"], 980);
        assert_eq!(result["data"]["start_line"], 1);
        assert_eq!(result["data"]["end_line"], 200);
        assert_eq!(result["data"]["truncated"], true);
        assert_eq!(result["data"]["next_start_line"], 201);
    }

    #[tokio::test]
    async fn a_requested_range_is_still_held_to_the_character_cap() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("wide.rs"), numbered_lines(300, 200)).unwrap();

        let result = run(
            &tool(&dir, None),
            json!({"path": "wide.rs", "start_line": 10, "end_line": 300}),
        )
        .await;

        let content = result["data"]["content"].as_str().unwrap();
        let end = result["data"]["end_line"].as_u64().unwrap();
        assert!(content.chars().count() <= MAX_CONTENT_CHARS);
        assert!(end < 300);
        assert_eq!(result["data"]["truncated"], true);
        assert_eq!(result["data"]["next_start_line"], end + 1);
        assert!(content.ends_with(&format!("{end:0>200}")));
    }

    #[tokio::test]
    async fn a_single_oversized_line_is_cut_and_flagged() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("min.js"), "é".repeat(50_000)).unwrap();

        let result = run(&tool(&dir, None), json!({"path": "min.js"})).await;

        let content = result["data"]["content"].as_str().unwrap();
        assert_eq!(content.chars().count(), MAX_CONTENT_CHARS);
        assert_eq!(result["data"]["truncated"], true);
        assert_eq!(result["data"]["line_cut"], true);
    }

    #[tokio::test]
    async fn a_short_file_is_not_flagged_as_truncated() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\n").unwrap();

        let result = run(&tool(&dir, None), json!({"path": "a.txt"})).await;

        assert_eq!(result["data"]["truncated"], false);
        assert!(result["data"].get("next_start_line").is_none());
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
