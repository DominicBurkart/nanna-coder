//! End-to-end integration tests for the GitHub PR Status tool (Issue #82)
//!
//! These tests exercise the full tool lifecycle: registration, definition,
//! execution at both L0 and L1 levels, and error handling.
//! Tests gracefully handle environments where git or GITHUB_TOKEN may be unavailable.

use harness::tools::{
    create_tool_registry, GitHubPrStatusTool, GitHubStatus, PrStatusData, Tool, ToolError,
};
use serde_json::json;
use std::path::PathBuf;
use tempfile::TempDir;

// ── Helper ───────────────────────────────────────────────────────────────────

/// Create a temporary git repository for testing.
fn create_temp_git_repo() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");
    let path = dir.path();

    // Initialize a git repo
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("Failed to git init");

    // Configure git user for commits
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("Failed to set git email");

    std::process::Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .expect("Failed to set git name");

    // Create initial commit so HEAD exists
    std::fs::write(path.join("README.md"), "# Test repo\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .expect("Failed to git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(path)
        .output()
        .expect("Failed to git commit");

    dir
}

/// Check if git is available
fn has_git() -> bool {
    std::process::Command::new("git")
        .args(["--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── Tool registration tests ──────────────────────────────────────────────────

#[test]
fn test_pr_status_tool_registered_in_default_registry() {
    let cwd = std::env::current_dir().unwrap();
    let registry = create_tool_registry(&cwd);

    let tool = registry.get_tool("github_pr_status");
    assert!(
        tool.is_some(),
        "github_pr_status should be in default registry"
    );
}

#[test]
fn test_pr_status_tool_definition_schema() {
    let tool = GitHubPrStatusTool::new(PathBuf::from("/tmp"));
    let def = tool.definition();

    assert_eq!(def.function.name, "github_pr_status");
    assert!(!def.function.description.is_empty());

    let props = def.function.parameters.properties.as_ref().unwrap();
    assert!(props.contains_key("level"), "Should have 'level' parameter");
    assert!(props.contains_key("field"), "Should have 'field' parameter");

    // level and field are optional (not in required list)
    let required = def.function.parameters.required.as_ref().unwrap();
    assert!(
        !required.contains(&"level".to_string()),
        "level should be optional"
    );
    assert!(
        !required.contains(&"field".to_string()),
        "field should be optional"
    );
}

#[test]
fn test_pr_status_tool_listed_with_other_tools() {
    let cwd = std::env::current_dir().unwrap();
    let registry = create_tool_registry(&cwd);

    let tools = registry.list_tools();
    assert!(tools.contains(&"github_pr_status"));
    assert!(tools.contains(&"git_status")); // coexists with existing git tools
    assert!(tools.contains(&"git_diff"));
}

// ── L0 format E2E tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_l0_default_in_git_repo() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool.execute(json!({})).await;
    assert!(result.is_ok(), "L0 should succeed in a git repo");

    let val = result.unwrap();
    assert_eq!(val["level"], "l0");

    let status = val["status"].as_str().unwrap();
    // In a fresh repo with no upstream, should contain the SHA and "no-upstream"
    assert!(
        !status.is_empty(),
        "Status line should not be empty: '{}'",
        status
    );
    assert!(
        status.contains("no-upstream"),
        "Fresh repo has no upstream: '{}'",
        status
    );
}

#[tokio::test]
async fn test_l0_explicit_level() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    // Explicit "l0" should behave same as default
    let result = tool.execute(json!({ "level": "l0" })).await.unwrap();
    assert_eq!(result["level"], "l0");
    assert!(result["status"].as_str().unwrap().contains("no-upstream"));
}

#[tokio::test]
async fn test_l0_with_local_changes() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let path = repo.path();

    // Make changes on a branch
    std::process::Command::new("git")
        .args(["checkout", "-b", "feature-branch"])
        .current_dir(path)
        .output()
        .unwrap();

    std::fs::write(path.join("new_file.rs"), "fn main() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "Add feature"])
        .current_dir(path)
        .output()
        .unwrap();

    let tool = GitHubPrStatusTool::new(path.to_path_buf());
    let result = tool.execute(json!({})).await.unwrap();

    let status = result["status"].as_str().unwrap();
    // Should have a SHA and no-upstream (no remote configured)
    assert!(!status.is_empty());
}

// ── L1 detail E2E tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_l1_conflicts_field() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool
        .execute(json!({ "level": "l1", "field": "conflicts" }))
        .await
        .unwrap();

    assert_eq!(result["level"], "l1");
    assert_eq!(result["field"], "conflicts");
    let detail = result["detail"].as_str().unwrap();
    // Fresh repo should have no conflicts
    assert!(
        detail.contains("No merge conflicts"),
        "Fresh repo should have no conflicts: '{}'",
        detail
    );
}

#[tokio::test]
async fn test_l1_sync_field() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool
        .execute(json!({ "level": "l1", "field": "sync" }))
        .await
        .unwrap();

    let detail = result["detail"].as_str().unwrap();
    assert!(
        detail.contains("No upstream") || detail.contains("ahead"),
        "Should report sync status: '{}'",
        detail
    );
}

#[tokio::test]
async fn test_l1_diff_field() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool
        .execute(json!({ "level": "l1", "field": "diff" }))
        .await
        .unwrap();

    let detail = result["detail"].as_str().unwrap();
    assert!(
        detail.contains("Diff"),
        "Should contain diff info: '{}'",
        detail
    );
}

#[tokio::test]
async fn test_l1_ci_field() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool
        .execute(json!({ "level": "l1", "field": "ci" }))
        .await
        .unwrap();

    let detail = result["detail"].as_str().unwrap();
    // Without gh CLI, CI status will be "unknown"
    assert!(
        detail.contains("CI status"),
        "Should report CI status: '{}'",
        detail
    );
}

#[tokio::test]
async fn test_l1_review_field() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool
        .execute(json!({ "level": "l1", "field": "review" }))
        .await
        .unwrap();

    let detail = result["detail"].as_str().unwrap();
    assert!(
        detail.contains("Review state"),
        "Should report review state: '{}'",
        detail
    );
}

#[tokio::test]
async fn test_l1_automerge_field() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool
        .execute(json!({ "level": "l1", "field": "automerge" }))
        .await
        .unwrap();

    let detail = result["detail"].as_str().unwrap();
    assert_eq!(detail, "Automerge: disabled");
}

#[tokio::test]
async fn test_l1_staleness_field() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool
        .execute(json!({ "level": "l1", "field": "staleness" }))
        .await
        .unwrap();

    let detail = result["detail"].as_str().unwrap();
    // Without gh, staleness is not available
    assert!(
        detail.contains("not available") || detail.contains("days ago"),
        "Should report staleness: '{}'",
        detail
    );
}

// ── Error handling E2E tests ─────────────────────────────────────────────────

#[tokio::test]
async fn test_l1_missing_field_returns_error() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool.execute(json!({ "level": "l1" })).await;
    assert!(result.is_err(), "L1 without field should error");

    match result {
        Err(ToolError::InvalidArguments { message }) => {
            assert!(
                message.contains("field"),
                "Error should mention 'field': {}",
                message
            );
        }
        other => panic!("Expected InvalidArguments, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_l1_invalid_field_returns_error() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool
        .execute(json!({ "level": "l1", "field": "bogus" }))
        .await;
    assert!(result.is_err(), "Invalid field should error");

    match result {
        Err(ToolError::InvalidArguments { message }) => {
            assert!(
                message.contains("Unknown field"),
                "Error should mention unknown field: {}",
                message
            );
        }
        other => panic!("Expected InvalidArguments, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_invalid_level_returns_error() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool.execute(json!({ "level": "l2" })).await;
    assert!(result.is_err(), "Invalid level should error");

    match result {
        Err(ToolError::InvalidArguments { message }) => {
            assert!(message.contains("Invalid level"), "Error: {}", message);
        }
        other => panic!("Expected InvalidArguments, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_not_a_git_repo() {
    let dir = TempDir::new().unwrap();
    let tool = GitHubPrStatusTool::new(dir.path().to_path_buf());

    let result = tool.execute(json!({})).await;
    // The tool uses git CLI which will fail in a non-git directory,
    // but it should still produce some output (just with fewer fields)
    // OR it might error gracefully
    match result {
        Ok(val) => {
            // If it returns, the status should still have some content
            assert_eq!(val["level"], "l0");
        }
        Err(_) => {
            // Also acceptable - tool reports failure
        }
    }
}

// ── PrStatusData L0 format specification tests ──────────────────────────────
// These test the exact output format specified in the issue

#[test]
fn test_l0_format_draft_with_conflicts() {
    let data = PrStatusData {
        pr_number: Some(42),
        issue_number: Some(17),
        pr_status: Some("draft".to_string()),
        conflict_count: Some(3),
        conflict_files: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        additions: Some(66),
        deletions: Some(233),
        ci_status: Some("fail".to_string()),
        has_upstream: true,
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };

    assert_eq!(data.to_l0(), "#42 #17 draft conflicts:3 +66/-233 ci:fail");
}

#[test]
fn test_l0_format_ready_approved_automerge() {
    let data = PrStatusData {
        pr_number: Some(42),
        issue_number: Some(17),
        pr_status: Some("ready".to_string()),
        review_state: Some("approved".to_string()),
        additions: Some(12),
        deletions: Some(5),
        ci_status: Some("pass".to_string()),
        automerge: true,
        staleness_days: Some(2),
        has_upstream: true,
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };

    assert_eq!(
        data.to_l0(),
        "#42 #17 ready approved +12/-5 ci:pass automerge 2d"
    );
}

#[test]
fn test_l0_format_behind_no_pr() {
    let data = PrStatusData {
        pr_number: Some(42),
        behind: Some(3),
        additions: Some(66),
        deletions: Some(233),
        has_upstream: true,
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };

    assert_eq!(data.to_l0(), "#42 behind:3 +66/-233");
}

#[test]
fn test_l0_format_commit_no_upstream() {
    let data = PrStatusData {
        head_sha: Some("abc123".to_string()),
        additions: Some(5),
        deletions: Some(2),
        has_upstream: false,
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };

    assert_eq!(data.to_l0(), "abc123 no-upstream +5/-2");
}

#[test]
fn test_l0_format_changes_requested() {
    let data = PrStatusData {
        pr_number: Some(42),
        issue_number: Some(17),
        pr_status: Some("ready".to_string()),
        review_state: Some("changes-requested".to_string()),
        conflict_count: Some(1),
        conflict_files: vec!["src/main.rs".to_string()],
        additions: Some(100),
        deletions: Some(50),
        ci_status: Some("pending".to_string()),
        has_upstream: true,
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };

    assert_eq!(
        data.to_l0(),
        "#42 #17 ready changes-requested conflicts:1 +100/-50 ci:pending"
    );
}

#[test]
fn test_l0_format_clean_pr() {
    let data = PrStatusData {
        pr_number: Some(42),
        pr_status: Some("ready".to_string()),
        additions: Some(0),
        deletions: Some(0),
        ci_status: Some("pass".to_string()),
        has_upstream: true,
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };

    assert_eq!(data.to_l0(), "#42 ready +0/-0 ci:pass");
}

// ── Registry-level E2E tests ────────────────────────────────────────────────

#[tokio::test]
async fn test_execute_via_registry() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let registry = create_tool_registry(repo.path());

    // Execute L0 through the registry interface
    let result = registry.execute("github_pr_status", json!({})).await;
    assert!(result.is_ok(), "Should execute via registry");

    let val = result.unwrap();
    assert_eq!(val["level"], "l0");
}

#[tokio::test]
async fn test_execute_l1_via_registry() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let registry = create_tool_registry(repo.path());

    let result = registry
        .execute(
            "github_pr_status",
            json!({ "level": "l1", "field": "conflicts" }),
        )
        .await;
    assert!(result.is_ok(), "L1 should execute via registry");

    let val = result.unwrap();
    assert_eq!(val["level"], "l1");
    assert!(val["detail"]
        .as_str()
        .unwrap()
        .contains("No merge conflicts"));
}

// ── L1 detail accuracy tests ────────────────────────────────────────────────

#[test]
fn test_l1_all_valid_fields() {
    let data = PrStatusData {
        has_upstream: true,
        ahead: Some(2),
        behind: Some(1),
        additions: Some(10),
        deletions: Some(5),
        ci_status: Some("pass".to_string()),
        review_state: Some("approved".to_string()),
        automerge: true,
        staleness_days: Some(3),
        changed_files: vec!["file.rs".to_string()],
        ..Default::default()
    };

    // All valid L1 fields should succeed
    for field in &[
        "conflicts",
        "ci",
        "diff",
        "sync",
        "review",
        "automerge",
        "staleness",
        "github",
    ] {
        let result = data.to_l1(field);
        assert!(result.is_ok(), "Field '{}' should be valid", field);
        assert!(
            !result.unwrap().is_empty(),
            "Field '{}' should return content",
            field
        );
    }
}

#[test]
fn test_l1_diff_with_files() {
    let data = PrStatusData {
        additions: Some(100),
        deletions: Some(50),
        changed_files: vec![
            "src/tools.rs".to_string(),
            "src/main.rs".to_string(),
            "Cargo.toml".to_string(),
        ],
        ..Default::default()
    };

    let detail = data.to_l1("diff").unwrap();
    assert!(detail.contains("+100/-50"));
    assert!(detail.contains("Changed files (3)"));
    assert!(detail.contains("src/tools.rs"));
    assert!(detail.contains("src/main.rs"));
    assert!(detail.contains("Cargo.toml"));
}

#[test]
fn test_l1_ci_with_multiple_failures() {
    let data = PrStatusData {
        ci_status: Some("fail".to_string()),
        ci_failing_checks: vec![
            "lint".to_string(),
            "test-unit".to_string(),
            "test-integration".to_string(),
        ],
        ..Default::default()
    };

    let detail = data.to_l1("ci").unwrap();
    assert!(detail.contains("CI status: fail"));
    assert!(detail.contains("Failing checks:"));
    assert!(detail.contains("lint"));
    assert!(detail.contains("test-unit"));
    assert!(detail.contains("test-integration"));
}

// ── GitHub status degradation tests ─────────────────────────────────────────

#[test]
fn test_l0_shows_unconfigured_when_no_token() {
    let data = PrStatusData {
        head_sha: Some("abc123".to_string()),
        has_upstream: false,
        github_status: GitHubStatus::NoToken,
        ..Default::default()
    };

    let l0 = data.to_l0();
    assert!(
        l0.contains("[github:unconfigured]"),
        "L0 should show unconfigured hint: '{}'",
        l0
    );
}

#[test]
fn test_l0_shows_error_when_api_fails() {
    let data = PrStatusData {
        head_sha: Some("abc123".to_string()),
        has_upstream: false,
        github_status: GitHubStatus::ApiError("401 Unauthorized".to_string()),
        ..Default::default()
    };

    let l0 = data.to_l0();
    assert!(
        l0.contains("[github:error]"),
        "L0 should show error hint: '{}'",
        l0
    );
}

#[test]
fn test_l0_no_hint_when_connected() {
    let data = PrStatusData {
        pr_number: Some(42),
        has_upstream: true,
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };

    let l0 = data.to_l0();
    assert!(
        !l0.contains("[github:"),
        "L0 should not show github hint when connected: '{}'",
        l0
    );
}

#[test]
fn test_l1_github_field_no_token() {
    let data = PrStatusData {
        github_status: GitHubStatus::NoToken,
        ..Default::default()
    };

    let detail = data.to_l1("github").unwrap();
    assert!(
        detail.contains("not configured"),
        "Should explain how to configure: '{}'",
        detail
    );
    assert!(
        detail.contains("GITHUB_TOKEN"),
        "Should mention GITHUB_TOKEN: '{}'",
        detail
    );
}

#[test]
fn test_l1_github_field_connected() {
    let data = PrStatusData {
        github_status: GitHubStatus::Connected,
        ..Default::default()
    };

    let detail = data.to_l1("github").unwrap();
    assert!(
        detail.contains("connected"),
        "Should show connected status: '{}'",
        detail
    );
}

#[test]
fn test_l1_github_field_api_error() {
    let data = PrStatusData {
        github_status: GitHubStatus::ApiError("403 Forbidden".to_string()),
        ..Default::default()
    };

    let detail = data.to_l1("github").unwrap();
    assert!(detail.contains("error"), "Should show error: '{}'", detail);
    assert!(
        detail.contains("403 Forbidden"),
        "Should include error message: '{}'",
        detail
    );
}

#[tokio::test]
async fn test_execute_returns_github_connected_field() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool.execute(json!({})).await.unwrap();
    // github_connected should be present in response
    assert!(
        result.get("github_connected").is_some(),
        "Response should include github_connected field"
    );
}

#[tokio::test]
async fn test_l1_github_field_via_tool() {
    if !has_git() {
        eprintln!("Skipping: git not available");
        return;
    }

    let repo = create_temp_git_repo();
    let tool = GitHubPrStatusTool::new(repo.path().to_path_buf());

    let result = tool
        .execute(json!({ "level": "l1", "field": "github" }))
        .await
        .unwrap();

    assert_eq!(result["level"], "l1");
    assert_eq!(result["field"], "github");
    let detail = result["detail"].as_str().unwrap();
    // Without GITHUB_TOKEN set, should show unconfigured message
    assert!(
        detail.contains("not configured") || detail.contains("connected"),
        "Should report GitHub status: '{}'",
        detail
    );
}
