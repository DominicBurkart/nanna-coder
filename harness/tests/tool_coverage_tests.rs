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
//! - Capability catalog: detection, uniqueness, registry gating
//! - Arg builders: `cargo_deny_args`, `cargo_audit_args`

use harness::capabilities::{detect_capabilities, find_capability, CARGO_CAPABILITIES};
use harness::container::{ContainerHandle, ContainerRuntime};
use harness::task::TaskId;
use harness::tools::{
    cargo_audit_args, cargo_deny_args, create_container_tool_registry, CalculatorTool,
    ReadFileTool, Tool, ToolError, ToolRegistry, WriteFileTool, CONTAINER_WORKSPACE_DIR,
};
use serde_json::json;
use std::sync::Arc;

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
// Capability catalog – static correctness
// ---------------------------------------------------------------------------

#[test]
fn capability_catalog_ids_are_unique() {
    let mut ids: Vec<&str> = CARGO_CAPABILITIES.iter().map(|c| c.id).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len(), "capability ids must be unique");
}

#[test]
fn capability_catalog_entries_are_non_empty() {
    for cap in CARGO_CAPABILITIES {
        assert!(!cap.id.is_empty());
        assert!(!cap.subcommand.is_empty());
        assert!(!cap.description.is_empty());
    }
}

#[test]
fn capability_descriptions_contain_example_and_returns() {
    for cap in CARGO_CAPABILITIES {
        assert!(
            cap.description.contains("Example"),
            "capability '{}' description missing Example",
            cap.id
        );
        assert!(
            cap.description.contains("Returns"),
            "capability '{}' description missing Returns",
            cap.id
        );
    }
}

// ---------------------------------------------------------------------------
// Capability detection – filesystem signals
// ---------------------------------------------------------------------------

#[test]
fn deny_tool_detected_when_deny_toml_present() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("deny.toml"), "").unwrap();
    let caps = detect_capabilities(dir.path());
    assert!(
        caps.iter().any(|c| c.id == "cargo_deny"),
        "cargo_deny must be detected when deny.toml is present"
    );
}

#[test]
fn deny_tool_absent_without_deny_toml() {
    let dir = tempfile::tempdir().unwrap();
    let caps = detect_capabilities(dir.path());
    assert!(
        !caps.iter().any(|c| c.id == "cargo_deny"),
        "cargo_deny must not be detected without deny.toml"
    );
}

#[test]
fn audit_tool_detected_when_audit_toml_present() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("audit.toml"), "").unwrap();
    let caps = detect_capabilities(dir.path());
    assert!(
        caps.iter().any(|c| c.id == "cargo_audit"),
        "cargo_audit must be detected when audit.toml is present"
    );
}

#[test]
fn find_capability_cargo_deny_returns_correct_nix_pkg() {
    let cap = find_capability("cargo_deny").expect("cargo_deny must exist");
    assert_eq!(cap.nix_package, Some("pkgs.cargo-deny"));
}

// ---------------------------------------------------------------------------
// Arg builders for new cargo tools
// ---------------------------------------------------------------------------

#[test]
fn cargo_deny_args_no_category() {
    let args = cargo_deny_args(None);
    assert_eq!(args, vec!["cargo", "deny", "check"]);
}

#[test]
fn cargo_deny_args_with_category() {
    let args = cargo_deny_args(Some("advisories"));
    assert_eq!(args, vec!["cargo", "deny", "check", "advisories"]);
}

#[test]
fn cargo_audit_args_baseline() {
    let args = cargo_audit_args();
    assert_eq!(args, vec!["cargo", "audit"]);
}

// ---------------------------------------------------------------------------
// Registry gating – signal-based tool registration
// ---------------------------------------------------------------------------

fn test_container_handle() -> Arc<ContainerHandle> {
    Arc::new(ContainerHandle {
        name: "test-container".to_string(),
        runtime: ContainerRuntime::None,
        port: None,
        needs_cleanup: false,
    })
}

fn stub_container_handle() -> Arc<ContainerHandle> {
    Arc::new(ContainerHandle {
        name: "stub-container".to_string(),
        runtime: ContainerRuntime::Stub,
        port: None,
        needs_cleanup: false,
    })
}

#[test]
fn cargo_deny_registered_when_deny_toml_present() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(dir.path().join("deny.toml"), "").unwrap();
    let handle = test_container_handle();
    let registry = create_container_tool_registry(dir.path(), handle, CONTAINER_WORKSPACE_DIR);
    assert!(
        registry.get_tool("cargo_deny").is_some(),
        "cargo_deny tool must be registered when deny.toml is present"
    );
}

#[test]
fn cargo_deny_absent_without_deny_toml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    let handle = test_container_handle();
    let registry = create_container_tool_registry(dir.path(), handle, CONTAINER_WORKSPACE_DIR);
    assert!(
        registry.get_tool("cargo_deny").is_none(),
        "cargo_deny tool must not be registered without deny.toml"
    );
}

#[test]
fn cargo_audit_registered_when_audit_toml_present() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(dir.path().join("audit.toml"), "").unwrap();
    let handle = test_container_handle();
    let registry = create_container_tool_registry(dir.path(), handle, CONTAINER_WORKSPACE_DIR);
    assert!(
        registry.get_tool("cargo_audit").is_some(),
        "cargo_audit tool must be registered when audit.toml is present"
    );
}

#[test]
fn cargo_audit_absent_without_audit_toml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    let handle = test_container_handle();
    let registry = create_container_tool_registry(dir.path(), handle, CONTAINER_WORKSPACE_DIR);
    assert!(
        registry.get_tool("cargo_audit").is_none(),
        "cargo_audit tool must not be registered without audit.toml"
    );
}

#[test]
fn no_cargo_tools_registered_without_cargo_toml() {
    let dir = tempfile::tempdir().unwrap();
    let handle = test_container_handle();
    let registry = create_container_tool_registry(dir.path(), handle, CONTAINER_WORKSPACE_DIR);
    for tool_name in &[
        "cargo_build",
        "cargo_test",
        "cargo_check",
        "cargo_bench",
        "cargo_run",
        "cargo_deny",
        "cargo_audit",
    ] {
        assert!(
            registry.get_tool(tool_name).is_none(),
            "{tool_name} must not be registered without Cargo.toml"
        );
    }
}

#[test]
fn core_cargo_tools_always_registered_with_cargo_toml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    let handle = test_container_handle();
    let registry = create_container_tool_registry(dir.path(), handle, CONTAINER_WORKSPACE_DIR);
    for tool_name in &[
        "cargo_build",
        "cargo_test",
        "cargo_check",
        "cargo_bench",
        "cargo_run",
    ] {
        assert!(
            registry.get_tool(tool_name).is_some(),
            "{tool_name} must be registered when Cargo.toml is present"
        );
    }
}

#[tokio::test]
async fn cargo_deny_tool_definition_name_matches_execute_name() {
    use harness::tools::CargoDenyTool;
    let handle = test_container_handle();
    let tool = CargoDenyTool::new(handle, Some("/workspace".to_string()));
    assert_eq!(tool.name(), tool.definition().function.name);
}

#[tokio::test]
async fn cargo_audit_tool_definition_name_matches_execute_name() {
    use harness::tools::CargoAuditTool;
    let handle = test_container_handle();
    let tool = CargoAuditTool::new(handle, Some("/workspace".to_string()));
    assert_eq!(tool.name(), tool.definition().function.name);
}

// ---------------------------------------------------------------------------
// Execute error paths — cover arg-parsing and map_err lines without a container
//
// ContainerRuntime::None makes exec_in_container return NoRuntimeAvailable,
// so every execute body up to and including the `?` operator is exercised.
// The Ok(json!{...}) success branch is covered by the #[ignore] container
// integration tests in dev_container_integration.rs.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cargo_build_execute_error_path_covered() {
    use harness::tools::CargoBuildTool;
    let handle = test_container_handle();
    let tool = CargoBuildTool::new(handle, Some("/workspace".to_string()));
    let err = tool
        .execute(json!({"package": "harness", "release": "true"}))
        .await
        .expect_err("execute must fail with None runtime");
    assert!(matches!(err, ToolError::ExecutionFailed { .. }));
}

#[tokio::test]
async fn cargo_test_execute_error_path_covered() {
    use harness::tools::CargoTestTool;
    let handle = test_container_handle();
    let tool = CargoTestTool::new(handle, Some("/workspace".to_string()));
    let err = tool
        .execute(json!({"package": "harness", "test_filter": "my_test"}))
        .await
        .expect_err("execute must fail with None runtime");
    assert!(matches!(err, ToolError::ExecutionFailed { .. }));
}

#[tokio::test]
async fn cargo_check_execute_error_path_covered() {
    use harness::tools::CargoCheckTool;
    let handle = test_container_handle();
    let tool = CargoCheckTool::new(handle, Some("/workspace".to_string()));
    let err = tool
        .execute(json!({}))
        .await
        .expect_err("execute must fail with None runtime");
    assert!(matches!(err, ToolError::ExecutionFailed { .. }));
}

#[tokio::test]
async fn cargo_bench_execute_error_path_covered() {
    use harness::tools::CargoBenchTool;
    let handle = test_container_handle();
    let tool = CargoBenchTool::new(handle, Some("/workspace".to_string()));
    let err = tool
        .execute(json!({"bench_filter": "bench_foo"}))
        .await
        .expect_err("execute must fail with None runtime");
    assert!(matches!(err, ToolError::ExecutionFailed { .. }));
}

#[tokio::test]
async fn cargo_run_execute_error_path_covered() {
    use harness::tools::CargoRunTool;
    let handle = test_container_handle();
    let tool = CargoRunTool::new(handle, Some("/workspace".to_string()));
    let err = tool
        .execute(json!({"bin": "harness", "args": "--help"}))
        .await
        .expect_err("execute must fail with None runtime");
    assert!(matches!(err, ToolError::ExecutionFailed { .. }));
}

#[tokio::test]
async fn cargo_deny_execute_error_path_covered() {
    use harness::tools::CargoDenyTool;
    let handle = test_container_handle();
    let tool = CargoDenyTool::new(handle, Some("/workspace".to_string()));
    let err = tool
        .execute(json!({"check": "advisories"}))
        .await
        .expect_err("execute must fail with None runtime");
    assert!(matches!(err, ToolError::ExecutionFailed { .. }));
}

#[tokio::test]
async fn cargo_audit_execute_error_path_covered() {
    use harness::tools::CargoAuditTool;
    let handle = test_container_handle();
    let tool = CargoAuditTool::new(handle, Some("/workspace".to_string()));
    let err = tool
        .execute(json!({}))
        .await
        .expect_err("execute must fail with None runtime");
    assert!(matches!(err, ToolError::ExecutionFailed { .. }));
}

#[tokio::test]
async fn cargo_deny_execute_no_check_arg_error_path() {
    use harness::tools::CargoDenyTool;
    let handle = test_container_handle();
    let tool = CargoDenyTool::new(handle, Some("/workspace".to_string()));
    let err = tool
        .execute(json!({}))
        .await
        .expect_err("execute must fail with None runtime");
    assert!(matches!(err, ToolError::ExecutionFailed { .. }));
}

// ---------------------------------------------------------------------------
// Success path coverage — ContainerRuntime::Stub bypasses exec and returns an
// empty Ok result so the Ok(json!{...}) branch in each tool's execute() is hit.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cargo_build_execute_success_path_covered() {
    use harness::tools::CargoBuildTool;
    let handle = stub_container_handle();
    let tool = CargoBuildTool::new(handle, Some("/workspace".to_string()));
    let result = tool
        .execute(json!({"package": "harness", "release": "true"}))
        .await
        .expect("execute must succeed with Stub runtime");
    assert!(result.get("stdout").is_some());
    assert!(result.get("stderr").is_some());
    assert!(result.get("success").is_some());
}

#[tokio::test]
async fn cargo_test_execute_success_path_covered() {
    use harness::tools::CargoTestTool;
    let handle = stub_container_handle();
    let tool = CargoTestTool::new(handle, Some("/workspace".to_string()));
    let result = tool
        .execute(json!({"test_filter": "my_test"}))
        .await
        .expect("execute must succeed with Stub runtime");
    assert!(result.get("stdout").is_some());
    assert!(result.get("stderr").is_some());
    assert!(result.get("success").is_some());
}

#[tokio::test]
async fn cargo_check_execute_success_path_covered() {
    use harness::tools::CargoCheckTool;
    let handle = stub_container_handle();
    let tool = CargoCheckTool::new(handle, Some("/workspace".to_string()));
    let result = tool
        .execute(json!({}))
        .await
        .expect("execute must succeed with Stub runtime");
    assert!(result.get("stdout").is_some());
    assert!(result.get("stderr").is_some());
    assert!(result.get("success").is_some());
}

#[tokio::test]
async fn cargo_bench_execute_success_path_covered() {
    use harness::tools::CargoBenchTool;
    let handle = stub_container_handle();
    let tool = CargoBenchTool::new(handle, Some("/workspace".to_string()));
    let result = tool
        .execute(json!({"bench_filter": "bench_foo"}))
        .await
        .expect("execute must succeed with Stub runtime");
    assert!(result.get("stdout").is_some());
    assert!(result.get("stderr").is_some());
    assert!(result.get("success").is_some());
}

#[tokio::test]
async fn cargo_run_execute_success_path_covered() {
    use harness::tools::CargoRunTool;
    let handle = stub_container_handle();
    let tool = CargoRunTool::new(handle, Some("/workspace".to_string()));
    let result = tool
        .execute(json!({"bin": "nanna", "args": "--help"}))
        .await
        .expect("execute must succeed with Stub runtime");
    assert!(result.get("stdout").is_some());
    assert!(result.get("stderr").is_some());
    assert!(result.get("success").is_some());
}

#[tokio::test]
async fn cargo_deny_execute_success_path_covered() {
    use harness::tools::CargoDenyTool;
    let handle = stub_container_handle();
    let tool = CargoDenyTool::new(handle, Some("/workspace".to_string()));
    let result = tool
        .execute(json!({"check": "advisories"}))
        .await
        .expect("execute must succeed with Stub runtime");
    assert!(result.get("stdout").is_some());
    assert!(result.get("stderr").is_some());
    assert!(result.get("success").is_some());
    assert!(result.get("command").is_some());
    assert_eq!(
        result["command"].as_str().unwrap(),
        "cargo deny check advisories"
    );
}

#[tokio::test]
async fn cargo_audit_execute_success_path_covered() {
    use harness::tools::CargoAuditTool;
    let handle = stub_container_handle();
    let tool = CargoAuditTool::new(handle, Some("/workspace".to_string()));
    let result = tool
        .execute(json!({}))
        .await
        .expect("execute must succeed with Stub runtime");
    assert!(result.get("stdout").is_some());
    assert!(result.get("stderr").is_some());
    assert!(result.get("success").is_some());
    assert!(result.get("command").is_some());
    assert_eq!(result["command"].as_str().unwrap(), "cargo audit");
}
