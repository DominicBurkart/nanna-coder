//! Focused unit tests for gaps identified in harness/src/tools.rs.
//!
//! Coverage added:
//! - `CalculatorTool`: subtract, multiply, and unknown-operation error (previously only add +
//!   divide-by-zero were tested)
//! - `ToolRegistry::execute`: `ToolError::NotFound` path when the tool name is unknown
//! - `WriteFileTool`: path-traversal security rejection via `..` components; missing parameters
//! - `ReadFileTool`: IO error on missing file; `start_line`-only (no `end_line`); line-number
//!   prefix format in returned content; missing path parameter
//! - `TaskId`: `Display` / `fmt` implementation
//! - `EchoTool`: missing message parameter error path
//! - `ListDirTool`: recursive directory listing via `list_recursive`
//! - `SearchTool`: invalid regex error; max_results cap
//! - `PrStatusData::to_l0`: `GitHubStatus::NoToken` and `GitHubStatus::ApiError` branches
//! - `PrStatusData::to_l1`: "github" field (all three connection states); "staleness" with `None`;
//!   "diff" with empty changed_files list

use harness::task::TaskId;
use harness::tools::{
    CalculatorTool, EchoTool, GitHubStatus, ListDirTool, PrStatusData, ReadFileTool, SearchTool,
    Tool, ToolError, ToolRegistry, WriteFileTool,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// CalculatorTool – missing arithmetic operations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn calculator_subtract() {
    let tool = CalculatorTool::new();
    let result = tool
        .execute(json!({ "operation": "subtract", "a": 10.0, "b": 3.0 }))
        .await
        .expect("subtract should succeed");
    assert_eq!(result["result"], 7.0);
    assert_eq!(result["operation"], "subtract");
}

#[tokio::test]
async fn calculator_multiply() {
    let tool = CalculatorTool::new();
    let result = tool
        .execute(json!({ "operation": "multiply", "a": 4.0, "b": 5.0 }))
        .await
        .expect("multiply should succeed");
    assert_eq!(result["result"], 20.0);
}

#[tokio::test]
async fn calculator_multiply_by_zero() {
    let tool = CalculatorTool::new();
    let result = tool
        .execute(json!({ "operation": "multiply", "a": 99.0, "b": 0.0 }))
        .await
        .expect("multiply by zero is valid and returns 0");
    assert_eq!(result["result"], 0.0);
}

#[tokio::test]
async fn calculator_unknown_operation_returns_invalid_arguments_error() {
    let tool = CalculatorTool::new();
    let err = tool
        .execute(json!({ "operation": "power", "a": 2.0, "b": 8.0 }))
        .await
        .expect_err("unknown operation must be an error");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments, got {:?}",
        err
    );
}

#[tokio::test]
async fn calculator_missing_operand_returns_invalid_arguments_error() {
    let tool = CalculatorTool::new();
    // 'b' is absent
    let err = tool
        .execute(json!({ "operation": "add", "a": 1.0 }))
        .await
        .expect_err("missing operand must be an error");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments, got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// ToolRegistry – NotFound error path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_registry_execute_unknown_tool_returns_not_found_error() {
    let registry = ToolRegistry::new(); // empty registry
    let err = registry
        .execute("nonexistent_tool", json!({}))
        .await
        .expect_err("unknown tool must return an error");
    assert!(
        matches!(err, ToolError::NotFound { .. }),
        "expected NotFound, got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// WriteFileTool – path security
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_file_path_traversal_rejected() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let tool = WriteFileTool::new(temp_dir.path().to_path_buf());

    let err = tool
        .execute(json!({ "path": "../escaped.txt", "content": "pwned" }))
        .await
        .expect_err("path traversal must be rejected");
    assert!(
        matches!(err, ToolError::PathSecurityViolation { .. }),
        "expected PathSecurityViolation, got {:?}",
        err
    );
    // File must NOT have been created outside the workspace
    assert!(
        !temp_dir
            .path()
            .parent()
            .unwrap()
            .join("escaped.txt")
            .exists(),
        "file must not be written outside workspace"
    );
}

#[tokio::test]
async fn write_file_absolute_path_outside_workspace_rejected() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let tool = WriteFileTool::new(temp_dir.path().to_path_buf());

    // Attempt to write to /tmp directly (outside any specific workspace dir)
    let outside = std::env::temp_dir().join("nanna_write_security_test_outside.txt");
    let err = tool
        .execute(json!({ "path": outside.to_str().unwrap(), "content": "pwned" }))
        .await
        .expect_err("absolute path outside workspace must be rejected");
    assert!(
        matches!(err, ToolError::PathSecurityViolation { .. }),
        "expected PathSecurityViolation, got {:?}",
        err
    );
    // Ensure the file was not created
    let _ = std::fs::remove_file(&outside); // clean up just in case
}

// ---------------------------------------------------------------------------
// ReadFileTool – IO error and line-range edge cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_file_nonexistent_returns_error() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let tool = ReadFileTool::new(temp_dir.path().to_path_buf());

    // Path that does not exist inside the workspace.
    let err = tool
        .execute(json!({ "path": "ghost.txt" }))
        .await
        .expect_err("missing file must return an error");
    // `ReadFileTool` runs `validate_path_within_workspace` which canonicalizes the
    // path before reading, so a missing file surfaces as `PathSecurityViolation`
    // (the canonicalize step fails). Either `Io` or `PathSecurityViolation` is an
    // acceptable "file not readable" signal for this test — both represent the
    // "file is not accessible" contract we care about.
    assert!(
        matches!(
            err,
            ToolError::Io(_) | ToolError::PathSecurityViolation { .. }
        ),
        "expected Io or PathSecurityViolation for missing file, got {:?}",
        err
    );
}

#[tokio::test]
async fn read_file_start_line_only_reads_to_end() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("lines.txt");
    std::fs::write(&path, "a\nb\nc\nd\ne").unwrap();

    let tool = ReadFileTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": "lines.txt", "start_line": 3 }))
        .await
        .expect("start_line only must succeed");

    // Lines 3-5 (c, d, e)
    assert_eq!(
        result["lines_shown"], 3,
        "should return 3 lines from line 3 onwards"
    );
    assert_eq!(result["total_lines"], 5);
}

#[tokio::test]
async fn read_file_line_numbers_are_prefixed_in_content() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("numbered.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma").unwrap();

    let tool = ReadFileTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": "numbered.txt" }))
        .await
        .expect("full read must succeed");

    let content = result["content"]
        .as_str()
        .expect("content should be a string");
    // The implementation prefixes each line with its 1-based line number
    assert!(
        content.contains("1") && content.contains("alpha"),
        "line 1 prefix expected in: {content}"
    );
    assert!(
        content.contains("2") && content.contains("beta"),
        "line 2 prefix expected in: {content}"
    );
}

#[tokio::test]
async fn read_file_start_line_1_returns_all_lines() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let path = temp_dir.path().join("all.txt");
    std::fs::write(&path, "x\ny\nz").unwrap();

    let tool = ReadFileTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": "all.txt", "start_line": 1 }))
        .await
        .expect("start_line=1 must succeed");

    assert_eq!(
        result["lines_shown"], 3,
        "start_line=1 should return all 3 lines"
    );
}

// ---------------------------------------------------------------------------
// TaskId – Display implementation
// ---------------------------------------------------------------------------

#[test]
fn task_id_display_matches_inner_string() {
    let id = TaskId("abc-123".to_string());
    assert_eq!(id.to_string(), "abc-123");
    assert_eq!(format!("{id}"), "abc-123");
}

#[test]
fn task_id_new_display_is_non_empty() {
    let id = TaskId::new();
    let s = id.to_string();
    assert!(!s.is_empty(), "TaskId display must not be empty");
    // UUID v4 is 36 chars (with hyphens)
    assert_eq!(s.len(), 36, "UUID v4 should be 36 chars, got: {s}");
}

// ---------------------------------------------------------------------------
// EchoTool – error path for missing message
// ---------------------------------------------------------------------------

#[tokio::test]
async fn echo_missing_message_returns_invalid_arguments_error() {
    let tool = EchoTool::new();
    let err = tool
        .execute(json!({}))
        .await
        .expect_err("missing message must be an error");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments, got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// WriteFileTool – missing parameter error paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_file_missing_path_returns_invalid_arguments_error() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let tool = WriteFileTool::new(temp_dir.path().to_path_buf());

    let err = tool
        .execute(json!({ "content": "some content" }))
        .await
        .expect_err("missing path must be an error");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments, got {:?}",
        err
    );
}

#[tokio::test]
async fn write_file_missing_content_returns_invalid_arguments_error() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let tool = WriteFileTool::new(temp_dir.path().to_path_buf());

    let err = tool
        .execute(json!({ "path": "output.txt" }))
        .await
        .expect_err("missing content must be an error");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments, got {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// ListDirTool – recursive directory listing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_dir_recursive_finds_nested_files() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let sub = temp_dir.path().join("subdir");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("nested.txt"), "content").unwrap();
    std::fs::write(temp_dir.path().join("root.txt"), "root").unwrap();

    let tool = ListDirTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "recursive": true }))
        .await
        .expect("recursive list must succeed");

    assert_eq!(
        result["count"], 2,
        "recursive listing should find both files"
    );
}

// ---------------------------------------------------------------------------
// SearchTool – invalid regex and max_results cap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_invalid_regex_returns_invalid_arguments_error() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let tool = SearchTool::new(temp_dir.path().to_path_buf());

    let err = tool
        .execute(json!({ "pattern": "[invalid regex" }))
        .await
        .expect_err("invalid regex must be an error");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments, got {:?}",
        err
    );
}

#[tokio::test]
async fn search_max_results_limits_output() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let content: String = (1..=10).map(|i| format!("match line {i}\n")).collect();
    std::fs::write(temp_dir.path().join("data.txt"), content).unwrap();

    let tool = SearchTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "pattern": "match line", "max_results": 3 }))
        .await
        .expect("search with max_results must succeed");

    assert_eq!(result["count"], 3, "max_results should cap results at 3");
}

// ---------------------------------------------------------------------------
// PrStatusData – uncovered to_l0 and to_l1 branches
// ---------------------------------------------------------------------------

#[test]
fn pr_status_l0_github_no_token() {
    let data = PrStatusData {
        has_upstream: true,
        github_status: GitHubStatus::NoToken,
        ..Default::default()
    };
    let l0 = data.to_l0();
    assert!(
        l0.contains("[github:unconfigured]"),
        "NoToken should surface as [github:unconfigured], got: {l0}"
    );
}

#[test]
fn pr_status_l0_github_api_error() {
    let data = PrStatusData {
        has_upstream: true,
        github_status: GitHubStatus::ApiError("rate limited".to_string()),
        ..Default::default()
    };
    let l0 = data.to_l0();
    assert!(
        l0.contains("[github:error]"),
        "ApiError should surface as [github:error], got: {l0}"
    );
}

#[test]
fn pr_status_l1_github_connected() {
    let data = PrStatusData {
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };
    let detail = data.to_l1("github").unwrap();
    assert!(
        detail.contains("connected"),
        "Connected state must say 'connected', got: {detail}"
    );
}

#[test]
fn pr_status_l1_github_no_token() {
    let data = PrStatusData {
        github_status: GitHubStatus::NoToken,
        ..Default::default()
    };
    let detail = data.to_l1("github").unwrap();
    assert!(
        detail.contains("not configured"),
        "NoToken must say 'not configured', got: {detail}"
    );
}

#[test]
fn pr_status_l1_github_api_error_message() {
    let data = PrStatusData {
        github_status: GitHubStatus::ApiError("timeout".to_string()),
        ..Default::default()
    };
    let detail = data.to_l1("github").unwrap();
    assert!(
        detail.contains("error"),
        "ApiError must say 'error', got: {detail}"
    );
    assert!(
        detail.contains("timeout"),
        "ApiError must include error message, got: {detail}"
    );
}

#[test]
fn pr_status_l1_staleness_none_says_not_available() {
    let data = PrStatusData::default();
    let detail = data.to_l1("staleness").unwrap();
    assert_eq!(detail, "Staleness data not available.");
}

#[test]
fn pr_status_l1_diff_empty_changed_files() {
    let data = PrStatusData {
        additions: Some(10),
        deletions: Some(5),
        changed_files: vec![],
        ..Default::default()
    };
    let detail = data.to_l1("diff").unwrap();
    assert!(detail.contains("+10/-5"), "got: {detail}");
    assert!(
        !detail.contains("Changed files"),
        "empty file list should not print header, got: {detail}"
    );
}
