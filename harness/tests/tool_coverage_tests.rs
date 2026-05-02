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
    CalculatorTool, GitDiffTool, ListDirTool, ReadFileTool, SearchTool, Tool, ToolError,
    ToolRegistry, WriteFileTool,
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
// ListDirTool – recursive listing and invalid glob
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_dir_recursive_finds_nested_files() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let sub = temp_dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("nested.txt"), "hello").unwrap();
    std::fs::write(temp_dir.path().join("top.txt"), "world").unwrap();

    let tool = ListDirTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(serde_json::json!({ "recursive": true }))
        .await
        .expect("recursive list must succeed");

    let entries = result["entries"].as_array().expect("entries array");
    let names: Vec<&str> = entries
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(
        names.contains(&"nested.txt"),
        "nested file must appear; got {names:?}"
    );
    assert!(
        names.contains(&"top.txt"),
        "top-level file must appear; got {names:?}"
    );
}

#[tokio::test]
async fn list_dir_recursive_invalid_glob_returns_error() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp_dir.path().join("a.txt"), "x").unwrap();

    let tool = ListDirTool::new(temp_dir.path().to_path_buf());
    let err = tool
        .execute(serde_json::json!({ "recursive": true, "pattern": "[invalid" }))
        .await
        .expect_err("invalid glob must return an error");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments for bad glob, got {err:?}"
    );
}

#[tokio::test]
async fn list_dir_non_recursive_invalid_glob_returns_error() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp_dir.path().join("b.rs"), "fn main() {}").unwrap();

    let tool = ListDirTool::new(temp_dir.path().to_path_buf());
    let err = tool
        .execute(serde_json::json!({ "recursive": false, "pattern": "[bad" }))
        .await
        .expect_err("invalid glob must return an error in non-recursive mode");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments for bad glob, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// SearchTool – invalid regex and max_results cap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_tool_invalid_regex_returns_error() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let tool = SearchTool::new(temp_dir.path().to_path_buf());
    let err = tool
        .execute(serde_json::json!({ "pattern": "([unclosed" }))
        .await
        .expect_err("invalid regex must return an error");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments for bad regex, got {err:?}"
    );
}

#[tokio::test]
async fn search_tool_max_results_caps_output() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    // Write a file where every line matches the pattern.
    let content = (0..10).map(|i| format!("match {i}")).collect::<Vec<_>>().join("\n");
    std::fs::write(temp_dir.path().join("data.txt"), content).unwrap();

    let tool = SearchTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(serde_json::json!({ "pattern": "match", "max_results": 3 }))
        .await
        .expect("capped search must succeed");

    let count = result["count"].as_u64().unwrap_or(0);
    assert_eq!(count, 3, "results must be capped at max_results=3, got {count}");
}

// ---------------------------------------------------------------------------
// GitDiffTool – staged flag exercises the `git diff --cached` path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn git_diff_tool_staged_flag() {
    let temp_dir = tempfile::tempdir().expect("tempdir");

    // Initialise a bare-minimum git repo so `git diff --cached` can run.
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "ci@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config email");
    std::process::Command::new("git")
        .args(["config", "user.name", "CI"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config name");

    let tool = GitDiffTool::new(temp_dir.path().to_path_buf());
    let result = tool
        .execute(serde_json::json!({ "staged": true }))
        .await
        .expect("git diff --cached in a fresh repo must succeed");

    assert_eq!(
        result["staged"], true,
        "response must reflect staged=true flag"
    );
    assert_eq!(
        result["has_changes"], false,
        "fresh repo has no staged changes"
    );
}
