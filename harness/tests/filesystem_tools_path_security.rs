//! Path-security invariants for the workspace filesystem tools.
//!
//! The agent's [`ReadFileTool`], [`WriteFileTool`], and [`ListDirTool`] are
//! exposed to model output. A sandbox escape — reading `/etc/passwd`, writing
//! over `~/.ssh/authorized_keys`, or enumerating arbitrary filesystem state
//! — would let an untrusted model compromise the host that runs the agent.
//!
//! The harness defends against this by rooting every tool in a
//! `workspace_root` and validating every caller-supplied path with the
//! internal `validate_path_within_workspace` / `validate_path_for_write`
//! helpers (`harness/src/tools.rs`). The in-module test suite exercises
//! exactly one escape (`../../../etc/passwd` via `ReadFileTool`).
//!
//! This integration suite covers the full invariant surface:
//!
//! 1. Relative `..` traversal on every read-capable tool.
//! 2. Absolute paths pointing outside the workspace.
//! 3. Symlinks inside the workspace whose target escapes the workspace
//!    (the canonicalize step must follow the link before comparing roots).
//! 4. Write-path traversal via `..` components in a yet-to-exist path
//!    (the write validator must reject `..` regardless of whether the
//!    target exists, because `canonicalize` cannot resolve non-existent
//!    intermediate paths).
//! 5. Absolute-path writes outside the workspace.
//! 6. Legitimate reads/writes to deep but in-workspace paths still succeed.
//!
//! These tests rely only on `tempfile` (already a harness dev-dep) and run
//! in parallel — each uses its own `tempfile::tempdir()` so there is no
//! cross-test filesystem contention.

use harness::tools::{ListDirTool, ReadFileTool, Tool, ToolError, WriteFileTool};
use serde_json::json;
use std::fs;
use tempfile::tempdir;

/// Helper: assert that a tool result is a PathSecurityViolation.
#[track_caller]
fn assert_path_security_violation<T: std::fmt::Debug>(result: Result<T, ToolError>, context: &str) {
    match result {
        Err(ToolError::PathSecurityViolation { .. }) => {}
        other => panic!("{context}: expected PathSecurityViolation, got {:?}", other),
    }
}

// -----------------------------------------------------------------------------
// ReadFileTool
// -----------------------------------------------------------------------------

#[tokio::test]
async fn read_file_rejects_relative_parent_traversal() {
    let workspace = tempdir().unwrap();
    let tool = ReadFileTool::new(workspace.path().to_path_buf());

    // Standard escape attempt: climb out of the workspace into /etc/passwd.
    // This file may or may not exist on the test host; canonicalize either
    // way should land outside the canonical workspace root.
    let result = tool.execute(json!({ "path": "../../../etc/passwd" })).await;
    assert_path_security_violation(result, "read_file '..' traversal");
}

#[tokio::test]
async fn read_file_rejects_absolute_path_outside_workspace() {
    let workspace = tempdir().unwrap();
    // /etc exists on Unix hosts, so canonicalize() will succeed and
    // return a real path outside the workspace root. The validator must
    // then reject it via the starts_with check.
    #[cfg(unix)]
    {
        let tool = ReadFileTool::new(workspace.path().to_path_buf());
        let result = tool.execute(json!({ "path": "/etc" })).await;
        assert_path_security_violation(result, "read_file absolute outside workspace");
    }
    // Swallow unused warning on non-Unix.
    let _ = workspace;
}

#[cfg(unix)]
#[tokio::test]
async fn read_file_rejects_symlink_escaping_workspace() {
    // Invariant: even if the symlink itself is inside the workspace, the
    // canonicalized target must still live inside the workspace. This is
    // the main reason the validator canonicalizes *after* joining the
    // workspace root rather than only checking textual prefixes.
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    fs::write(&secret, "top secret").unwrap();

    let link = workspace.path().join("escape_link");
    std::os::unix::fs::symlink(&secret, &link).unwrap();

    let tool = ReadFileTool::new(workspace.path().to_path_buf());
    let result = tool.execute(json!({ "path": "escape_link" })).await;
    assert_path_security_violation(result, "read_file symlink escape");
}

#[tokio::test]
async fn read_file_accepts_legitimate_nested_path() {
    // Positive control: a deeply nested but in-workspace path must succeed.
    // Without this we can't distinguish "secure" from "always rejects".
    let workspace = tempdir().unwrap();
    let nested = workspace.path().join("a").join("b").join("c");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("file.txt"), "hello\nworld").unwrap();

    let tool = ReadFileTool::new(workspace.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": "a/b/c/file.txt" }))
        .await
        .unwrap();
    assert_eq!(result["total_lines"], 2);
    assert!(
        result["content"].as_str().unwrap().contains("hello"),
        "expected content to contain 'hello', got {:?}",
        result["content"]
    );
}

// -----------------------------------------------------------------------------
// WriteFileTool
// -----------------------------------------------------------------------------

#[tokio::test]
async fn write_file_rejects_parent_dir_components_even_when_target_missing() {
    // Invariant: `validate_path_for_write` has to reject `..` based on the
    // path components alone, because the target file does not yet exist
    // and `canonicalize()` cannot follow a non-existent path. A silent
    // traversal here would let the agent clobber files outside the
    // workspace (e.g. `~/.ssh/authorized_keys`).
    let workspace = tempdir().unwrap();
    let tool = WriteFileTool::new(workspace.path().to_path_buf());

    let result = tool
        .execute(json!({
            "path": "../escape.txt",
            "content": "pwned"
        }))
        .await;
    assert_path_security_violation(result, "write_file '..' traversal");

    // Also verify the file was never created in the parent directory.
    let parent = workspace.path().parent().unwrap();
    assert!(
        !parent.join("escape.txt").exists(),
        "escape.txt should not exist in {}",
        parent.display()
    );
}

#[tokio::test]
async fn write_file_rejects_nested_parent_traversal() {
    // Subtler escape: a nested path whose only `..` appears mid-string.
    // The validator's component-level check must still fire.
    let workspace = tempdir().unwrap();
    let tool = WriteFileTool::new(workspace.path().to_path_buf());

    let result = tool
        .execute(json!({
            "path": "subdir/../../escape.txt",
            "content": "pwned"
        }))
        .await;
    assert_path_security_violation(result, "write_file nested '..' traversal");
}

#[cfg(unix)]
#[tokio::test]
async fn write_file_rejects_absolute_path_outside_workspace() {
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let target = outside.path().join("external.txt");

    let tool = WriteFileTool::new(workspace.path().to_path_buf());
    let result = tool
        .execute(json!({
            "path": target.to_string_lossy(),
            "content": "pwned"
        }))
        .await;
    assert_path_security_violation(result, "write_file absolute outside workspace");
    assert!(
        !target.exists(),
        "file was written outside workspace to {}",
        target.display()
    );
}

#[tokio::test]
async fn write_file_accepts_new_deep_subdirectory() {
    // Positive control: writing to a not-yet-existing deeply nested path
    // inside the workspace must succeed. This is the normal case for
    // agent-initiated file creation and is what makes the non-existent-
    // path handling in `validate_path_for_write` load-bearing.
    let workspace = tempdir().unwrap();
    let tool = WriteFileTool::new(workspace.path().to_path_buf());

    let result = tool
        .execute(json!({
            "path": "new/nested/path/hello.txt",
            "content": "hi there"
        }))
        .await
        .unwrap();
    assert_eq!(result["success"], true);
    assert_eq!(result["bytes_written"], 8);

    let written = workspace.path().join("new/nested/path/hello.txt");
    assert_eq!(fs::read_to_string(written).unwrap(), "hi there");
}

// -----------------------------------------------------------------------------
// ListDirTool
// -----------------------------------------------------------------------------

#[tokio::test]
async fn list_dir_rejects_relative_parent_traversal() {
    let workspace = tempdir().unwrap();
    let tool = ListDirTool::new(workspace.path().to_path_buf());

    let result = tool.execute(json!({ "path": "../" })).await;
    assert_path_security_violation(result, "list_directory '..' traversal");
}

#[cfg(unix)]
#[tokio::test]
async fn list_dir_rejects_absolute_path_outside_workspace() {
    let workspace = tempdir().unwrap();
    let tool = ListDirTool::new(workspace.path().to_path_buf());

    let result = tool.execute(json!({ "path": "/etc" })).await;
    assert_path_security_violation(result, "list_directory absolute outside workspace");
}

#[tokio::test]
async fn list_dir_default_lists_workspace_root() {
    // Positive control: without any `path` argument, the tool must
    // resolve to the workspace root itself (not error out, not escape).
    let workspace = tempdir().unwrap();
    fs::write(workspace.path().join("visible.txt"), "x").unwrap();

    let tool = ListDirTool::new(workspace.path().to_path_buf());
    let result = tool.execute(json!({})).await.unwrap();

    let entries = result["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["name"].as_str() == Some("visible.txt")),
        "expected 'visible.txt' in listing, got {:?}",
        entries
    );
}

#[cfg(unix)]
#[tokio::test]
async fn list_dir_rejects_symlink_target_outside_workspace() {
    // Same invariant as the read-file case but for directory listings:
    // a symlinked directory pointing outside the workspace must not
    // reveal anything to the agent.
    let workspace = tempdir().unwrap();
    let outside = tempdir().unwrap();
    fs::write(outside.path().join("leaked.txt"), "leak").unwrap();

    let link = workspace.path().join("peek");
    std::os::unix::fs::symlink(outside.path(), &link).unwrap();

    let tool = ListDirTool::new(workspace.path().to_path_buf());
    let result = tool.execute(json!({ "path": "peek" })).await;
    assert_path_security_violation(result, "list_directory symlink escape");
}
