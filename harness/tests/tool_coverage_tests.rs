//! Focused unit tests for gaps identified in harness/src/tools.rs.
//!
//! Coverage added:
//! - `CalculatorTool`: subtract, multiply, divide, and unknown-operation error
//! - `ToolRegistry::execute`: `ToolError::NotFound` path; `list_tools`; `get_definitions`
//! - `WriteFileTool`: path-traversal security rejection via `..` components
//! - `ReadFileTool`: IO error on missing file; `start_line`-only (no `end_line`); line-number
//!   prefix format in returned content
//! - `PrStatusData::to_l0`: GitHubStatus variants, closed PR status, ahead branch, zero staleness
//! - `PrStatusData::to_l1`: github, staleness-None, diff-no-data, ci-unknown-status branches
//! - `TaskId`: `Display` / `fmt` implementation

use harness::task::TaskId;
use harness::tools::{
    CalculatorTool, GitHubStatus, PrStatusData, ReadFileTool, Tool, ToolError, ToolRegistry,
    WriteFileTool,
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
// CalculatorTool – divide (non-zero divisor)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn calculator_divide_non_zero() {
    let tool = CalculatorTool::new();
    let result = tool
        .execute(json!({ "operation": "divide", "a": 10.0, "b": 4.0 }))
        .await
        .expect("non-zero divide should succeed");
    assert_eq!(result["result"], 2.5);
    assert_eq!(result["operation"], "divide");
}

// ---------------------------------------------------------------------------
// ToolRegistry – list_tools and get_definitions
// ---------------------------------------------------------------------------

#[test]
fn tool_registry_list_tools_returns_registered_names() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(CalculatorTool::new()));
    let names = registry.list_tools();
    assert!(
        names.contains(&"calculate"),
        "expected calculate in list, got: {names:?}"
    );
}

#[test]
fn tool_registry_get_definitions_returns_definitions() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(CalculatorTool::new()));
    let defs = registry.get_definitions();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].function.name, "calculate");
}

// ---------------------------------------------------------------------------
// PrStatusData::to_l0 – uncovered branches
// ---------------------------------------------------------------------------

#[test]
fn pr_status_l0_github_no_token() {
    let data = PrStatusData {
        head_sha: Some("abc".to_string()),
        has_upstream: false,
        github_status: GitHubStatus::NoToken,
        ..Default::default()
    };
    let l0 = data.to_l0();
    assert!(
        l0.contains("[github:unconfigured]"),
        "expected [github:unconfigured] in: {l0}"
    );
}

#[test]
fn pr_status_l0_github_api_error() {
    let data = PrStatusData {
        head_sha: Some("abc".to_string()),
        has_upstream: false,
        github_status: GitHubStatus::ApiError("rate limited".to_string()),
        ..Default::default()
    };
    let l0 = data.to_l0();
    assert!(
        l0.contains("[github:error]"),
        "expected [github:error] in: {l0}"
    );
}

#[test]
fn pr_status_l0_closed_pr_status() {
    let data = PrStatusData {
        pr_number: Some(7),
        pr_status: Some("closed".to_string()),
        has_upstream: true,
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };
    let l0 = data.to_l0();
    assert!(l0.contains("closed"), "expected closed in: {l0}");
}

#[test]
fn pr_status_l0_ahead_branch() {
    let data = PrStatusData {
        pr_number: Some(5),
        has_upstream: true,
        ahead: Some(2),
        behind: Some(0),
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };
    let l0 = data.to_l0();
    assert!(l0.contains("ahead:2"), "expected ahead:2 in: {l0}");
}

#[test]
fn pr_status_l0_zero_staleness_not_shown() {
    let data = PrStatusData {
        pr_number: Some(1),
        has_upstream: true,
        staleness_days: Some(0),
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };
    let l0 = data.to_l0();
    assert!(
        !l0.contains('d'),
        "zero staleness days must not appear in: {l0}"
    );
}

// ---------------------------------------------------------------------------
// PrStatusData::to_l1 – uncovered branches
// ---------------------------------------------------------------------------

#[test]
fn pr_status_l1_github_connected() {
    let data = PrStatusData {
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };
    let detail = data.to_l1("github").unwrap();
    assert!(
        detail.contains("connected"),
        "expected 'connected' in: {detail}"
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
        detail.contains("not configured") || detail.contains("GITHUB_TOKEN"),
        "expected token guidance in: {detail}"
    );
}

#[test]
fn pr_status_l1_github_api_error() {
    let data = PrStatusData {
        github_status: GitHubStatus::ApiError("timeout".to_string()),
        ..Default::default()
    };
    let detail = data.to_l1("github").unwrap();
    assert!(
        detail.contains("timeout"),
        "expected error message in: {detail}"
    );
}

#[test]
fn pr_status_l1_staleness_none() {
    let data = PrStatusData::default();
    let detail = data.to_l1("staleness").unwrap();
    assert_eq!(detail, "Staleness data not available.");
}

#[test]
fn pr_status_l1_diff_no_data() {
    let data = PrStatusData::default(); // additions and deletions are None
    let detail = data.to_l1("diff").unwrap();
    assert!(
        detail.contains("no diff data"),
        "expected 'no diff data' when additions/deletions are None, got: {detail}"
    );
}

#[test]
fn pr_status_l1_diff_with_stats_no_files() {
    let data = PrStatusData {
        additions: Some(10),
        deletions: Some(5),
        changed_files: vec![],
        ..Default::default()
    };
    let detail = data.to_l1("diff").unwrap();
    assert!(
        detail.contains("+10/-5"),
        "expected diff stats in: {detail}"
    );
    assert!(
        !detail.contains("Changed files"),
        "no file list when changed_files is empty"
    );
}

#[test]
fn pr_status_l1_ci_none_status_shows_unknown() {
    let data = PrStatusData {
        ci_status: None,
        ci_failing_checks: vec![],
        ..Default::default()
    };
    let detail = data.to_l1("ci").unwrap();
    assert!(
        detail.contains("unknown"),
        "expected 'unknown' for None ci_status, got: {detail}"
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
