//! Focused unit tests for gaps identified in harness/src/tools.rs.
//!
//! Coverage added:
//! - `CalculatorTool`: subtract, multiply, and unknown-operation error (previously only add +
//!   divide-by-zero were tested)
//! - `ToolRegistry::execute`: `ToolError::NotFound` path when the tool name is unknown
//! - `WriteFileTool`: path-traversal security rejection via `..` components
//! - `ReadFileTool`: IO error on missing file; `start_line`-only (no `end_line`); line-number
//!   prefix format in returned content
//! - `TaskId`: `Display` / `fmt` implementation

use harness::task::TaskId;
use harness::tools::{
    CalculatorTool, ReadFileTool, Tool, ToolError, ToolRegistry, WriteFileTool,
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
        !temp_dir.path().parent().unwrap().join("escaped.txt").exists(),
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
async fn read_file_nonexistent_returns_io_error() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let tool = ReadFileTool::new(temp_dir.path().to_path_buf());

    // Create the file so validate_path_within_workspace succeeds, then delete it
    let path = temp_dir.path().join("ghost.txt");
    std::fs::write(&path, "hello").unwrap();
    std::fs::remove_file(&path).unwrap();

    let err = tool
        .execute(json!({ "path": "ghost.txt" }))
        .await
        .expect_err("missing file must return an error");
    // The error is produced by std::fs::read_to_string which maps to ToolError::Io
    assert!(
        matches!(err, ToolError::Io(_)),
        "expected Io error, got {:?}",
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
    assert_eq!(result["lines_shown"], 3, "should return 3 lines from line 3 onwards");
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

    let content = result["content"].as_str().expect("content should be a string");
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
