//! Extended unit tests for tools.rs covering previously untested tools.
//!
//! Adds coverage for:
//! - `EchoTool`: basic echo, missing message parameter
//! - `ListDirTool`: flat listing, recursive listing, glob pattern filtering
//! - `SearchTool`: pattern match, file_pattern filter, max_results cap
//! - `GitStatusTool`: against a real git repo
//! - `GitDiffTool`: unstaged and staged diffs
//! - `ToolRegistry::list_tools()` and `get_definitions()`
//! - `PrStatusData::to_l0()` field formatting
//! - `PrStatusData::to_l1()` field detail formatting
//! - `create_tool_registry` helper

use harness::tools::{
    create_tool_registry, CalculatorTool, EchoTool, GitDiffTool, GitStatusTool, GitHubStatus,
    ListDirTool, PrStatusData, SearchTool, Tool, ToolError, ToolRegistry,
};
use serde_json::json;
use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn init_git_repo(dir: &Path) {
    for args in &[
        vec!["init"],
        vec!["config", "user.email", "test@test.com"],
        vec!["config", "user.name", "Test"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
    }
    std::fs::write(dir.join("README.md"), "# Test repo").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();
}

// ---------------------------------------------------------------------------
// EchoTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn echo_tool_returns_message() {
    let tool = EchoTool::new();
    let result = tool
        .execute(json!({ "message": "hello world" }))
        .await
        .expect("echo should succeed");
    assert_eq!(result["echoed"], "hello world");
    assert!(result["timestamp"].is_string());
}

#[tokio::test]
async fn echo_tool_missing_message_returns_error() {
    let tool = EchoTool::new();
    let err = tool
        .execute(json!({}))
        .await
        .expect_err("missing message must error");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments, got {:?}",
        err
    );
}

#[tokio::test]
async fn echo_tool_name_is_echo() {
    let tool = EchoTool::new();
    assert_eq!(tool.name(), "echo");
}

// ---------------------------------------------------------------------------
// ListDirTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_dir_flat_lists_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "").unwrap();
    std::fs::write(dir.path().join("b.txt"), "").unwrap();

    let tool = ListDirTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": "." }))
        .await
        .expect("list_dir should succeed");

    let count = result["count"].as_u64().unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn list_dir_recursive_finds_nested_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("subdir")).unwrap();
    std::fs::write(dir.path().join("top.txt"), "").unwrap();
    std::fs::write(dir.path().join("subdir").join("nested.txt"), "").unwrap();

    let tool = ListDirTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": ".", "recursive": true }))
        .await
        .expect("recursive list should succeed");

    let count = result["count"].as_u64().unwrap();
    assert!(count >= 2, "should find at least 2 files, got {}", count);
}

#[tokio::test]
async fn list_dir_glob_pattern_filters() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "").unwrap();
    std::fs::write(dir.path().join("main.txt"), "").unwrap();

    let tool = ListDirTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "path": ".", "pattern": "*.rs" }))
        .await
        .expect("glob filter should succeed");

    let count = result["count"].as_u64().unwrap();
    assert_eq!(count, 1, "only .rs files should match");
    let entries = result["entries"].as_array().unwrap();
    assert_eq!(entries[0]["name"], "main.rs");
}

#[tokio::test]
async fn list_dir_name_is_list_directory() {
    let dir = tempfile::tempdir().unwrap();
    let tool = ListDirTool::new(dir.path().to_path_buf());
    assert_eq!(tool.name(), "list_directory");
}

// ---------------------------------------------------------------------------
// SearchTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_finds_matching_lines() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("code.rs"), "fn main() {\n    println!(\"hello\");\n}\n")
        .unwrap();

    let tool = SearchTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "pattern": "println" }))
        .await
        .expect("search should succeed");

    let count = result["count"].as_u64().unwrap();
    assert_eq!(count, 1);
    let results = result["results"].as_array().unwrap();
    assert!(results[0]["content"]
        .as_str()
        .unwrap()
        .contains("println"));
}

#[tokio::test]
async fn search_file_pattern_filters_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("code.rs"), "needle").unwrap();
    std::fs::write(dir.path().join("data.txt"), "needle").unwrap();

    let tool = SearchTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "pattern": "needle", "file_pattern": "*.rs" }))
        .await
        .expect("search with file_pattern should succeed");

    let count = result["count"].as_u64().unwrap();
    assert_eq!(count, 1, "only .rs files should be searched");
    let results = result["results"].as_array().unwrap();
    assert!(results[0]["file"].as_str().unwrap().ends_with(".rs"));
}

#[tokio::test]
async fn search_max_results_caps_output() {
    let dir = tempfile::tempdir().unwrap();
    // Write 20 lines all matching "match"
    let content = (0..20).map(|_| "match\n").collect::<String>();
    std::fs::write(dir.path().join("many.txt"), content).unwrap();

    let tool = SearchTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "pattern": "match", "max_results": 5 }))
        .await
        .expect("search should succeed");

    let count = result["count"].as_u64().unwrap();
    assert_eq!(count, 5, "max_results should cap at 5");
}

#[tokio::test]
async fn search_invalid_regex_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let tool = SearchTool::new(dir.path().to_path_buf());
    let err = tool
        .execute(json!({ "pattern": "[invalid" }))
        .await
        .expect_err("invalid regex must error");
    assert!(
        matches!(err, ToolError::InvalidArguments { .. }),
        "expected InvalidArguments, got {:?}",
        err
    );
}

#[tokio::test]
async fn search_name_is_search() {
    let dir = tempfile::tempdir().unwrap();
    let tool = SearchTool::new(dir.path().to_path_buf());
    assert_eq!(tool.name(), "search");
}

// ---------------------------------------------------------------------------
// GitStatusTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn git_status_detects_branch_in_repo() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    let tool = GitStatusTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(json!({}))
        .await
        .expect("git_status should succeed in a git repo");

    // Should at least have a branch and commit
    assert!(result.get("branch").is_some() || result.get("commit").is_some());
    assert!(result.get("is_dirty").is_some());
}

#[tokio::test]
async fn git_status_not_a_repo_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    // Not a git repo

    let tool = GitStatusTool::new(dir.path().to_path_buf());
    let err = tool
        .execute(json!({}))
        .await
        .expect_err("non-repo should error");
    assert!(
        matches!(err, ToolError::ExecutionFailed { .. }),
        "expected ExecutionFailed, got {:?}",
        err
    );
}

#[tokio::test]
async fn git_status_name_is_git_status() {
    let dir = tempfile::tempdir().unwrap();
    let tool = GitStatusTool::new(dir.path().to_path_buf());
    assert_eq!(tool.name(), "git_status");
}

// ---------------------------------------------------------------------------
// GitDiffTool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn git_diff_returns_empty_when_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    let tool = GitDiffTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(json!({}))
        .await
        .expect("git_diff should succeed in a git repo");

    assert_eq!(result["has_changes"], false);
}

#[tokio::test]
async fn git_diff_detects_unstaged_change() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    // Modify a tracked file
    std::fs::write(dir.path().join("README.md"), "# Changed").unwrap();

    let tool = GitDiffTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "staged": false }))
        .await
        .expect("git_diff should detect unstaged change");

    assert_eq!(result["has_changes"], true);
    let diff = result["diff"].as_str().unwrap();
    assert!(diff.contains("README.md") || diff.contains("Changed"));
}

#[tokio::test]
async fn git_diff_staged_shows_staged_changes() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());

    std::fs::write(dir.path().join("new.txt"), "new content").unwrap();
    Command::new("git")
        .args(["add", "new.txt"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let tool = GitDiffTool::new(dir.path().to_path_buf());
    let result = tool
        .execute(json!({ "staged": true }))
        .await
        .expect("staged git_diff should succeed");

    assert_eq!(result["has_changes"], true);
    assert_eq!(result["staged"], true);
}

#[tokio::test]
async fn git_diff_name_is_git_diff() {
    let dir = tempfile::tempdir().unwrap();
    let tool = GitDiffTool::new(dir.path().to_path_buf());
    assert_eq!(tool.name(), "git_diff");
}

// ---------------------------------------------------------------------------
// ToolRegistry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_registry_list_tools_returns_names() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    registry.register(Box::new(CalculatorTool::new()));

    let tools = registry.list_tools();
    assert!(tools.contains(&"echo"));
    assert!(tools.contains(&"calculator"));
}

#[tokio::test]
async fn tool_registry_get_definitions_returns_all() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    registry.register(Box::new(CalculatorTool::new()));

    let defs = registry.get_definitions();
    assert_eq!(defs.len(), 2);
    let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"calculator"));
}

#[tokio::test]
async fn create_tool_registry_has_standard_tools() {
    let dir = tempfile::tempdir().unwrap();
    let registry = create_tool_registry(dir.path());

    let tools = registry.list_tools();
    // The standard registry should include the most common tools
    assert!(
        tools.contains(&"echo") || tools.contains(&"read_file"),
        "standard registry should include common tools, got: {:?}",
        tools
    );
}

// ---------------------------------------------------------------------------
// PrStatusData::to_l0
// ---------------------------------------------------------------------------

fn default_pr_status() -> PrStatusData {
    PrStatusData {
        pr_number: None,
        issue_number: None,
        pr_status: None,
        review_state: None,
        conflict_count: None,
        conflict_files: vec![],
        ahead: None,
        behind: None,
        additions: None,
        deletions: None,
        ci_status: None,
        ci_failing_checks: vec![],
        automerge: false,
        staleness_days: None,
        branch: None,
        head_sha: None,
        has_upstream: false,
        changed_files: vec![],
        github_status: GitHubStatus::Connected,
    }
}

#[test]
fn pr_status_to_l0_shows_pr_number() {
    let data = PrStatusData {
        pr_number: Some(42),
        head_sha: Some("abc1234".to_string()),
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(l0.contains("#42"), "L0 should show PR number, got: {l0}");
}

#[test]
fn pr_status_to_l0_uses_sha_when_no_pr() {
    let data = PrStatusData {
        head_sha: Some("abc1234".to_string()),
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(l0.contains("abc1234"), "L0 should fall back to SHA, got: {l0}");
}

#[test]
fn pr_status_to_l0_shows_ci_pass() {
    let data = PrStatusData {
        ci_status: Some("pass".to_string()),
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(l0.contains("ci:pass"), "L0 should show ci:pass, got: {l0}");
}

#[test]
fn pr_status_to_l0_shows_ci_fail() {
    let data = PrStatusData {
        ci_status: Some("fail".to_string()),
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(l0.contains("ci:fail"), "L0 should show ci:fail, got: {l0}");
}

#[test]
fn pr_status_to_l0_shows_conflicts() {
    let data = PrStatusData {
        conflict_count: Some(3),
        conflict_files: vec!["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()],
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(
        l0.contains("conflicts:3"),
        "L0 should show conflicts count, got: {l0}"
    );
}

#[test]
fn pr_status_to_l0_shows_diff_stats() {
    let data = PrStatusData {
        additions: Some(100),
        deletions: Some(20),
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(
        l0.contains("+100/-20"),
        "L0 should show diff stats, got: {l0}"
    );
}

#[test]
fn pr_status_to_l0_shows_automerge_when_enabled() {
    let data = PrStatusData {
        automerge: true,
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(
        l0.contains("automerge"),
        "L0 should show automerge, got: {l0}"
    );
}

#[test]
fn pr_status_to_l0_shows_draft_status() {
    let data = PrStatusData {
        pr_status: Some("draft".to_string()),
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(l0.contains("draft"), "L0 should show draft, got: {l0}");
}

#[test]
fn pr_status_to_l0_shows_approved_review() {
    let data = PrStatusData {
        review_state: Some("approved".to_string()),
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(l0.contains("approved"), "L0 should show approved, got: {l0}");
}

#[test]
fn pr_status_to_l0_shows_github_unconfigured() {
    let data = PrStatusData {
        github_status: GitHubStatus::NoToken,
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(
        l0.contains("[github:unconfigured]"),
        "L0 should show github:unconfigured, got: {l0}"
    );
}

#[test]
fn pr_status_to_l0_shows_github_error() {
    let data = PrStatusData {
        github_status: GitHubStatus::ApiError("rate limit".to_string()),
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(
        l0.contains("[github:error]"),
        "L0 should show github:error, got: {l0}"
    );
}

#[test]
fn pr_status_to_l0_shows_behind_count() {
    let data = PrStatusData {
        behind: Some(5),
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(
        l0.contains("behind:5"),
        "L0 should show behind count, got: {l0}"
    );
}

#[test]
fn pr_status_to_l0_shows_staleness() {
    let data = PrStatusData {
        staleness_days: Some(7),
        has_upstream: true,
        ..default_pr_status()
    };
    let l0 = data.to_l0();
    assert!(l0.contains("7d"), "L0 should show staleness, got: {l0}");
}

// ---------------------------------------------------------------------------
// PrStatusData::to_l1
// ---------------------------------------------------------------------------

#[test]
fn pr_status_to_l1_conflicts_no_conflicts() {
    let data = default_pr_status();
    let detail = data.to_l1("conflicts").unwrap();
    assert!(detail.contains("No merge conflicts"));
}

#[test]
fn pr_status_to_l1_conflicts_with_files() {
    let data = PrStatusData {
        conflict_files: vec!["src/main.rs".to_string(), "Cargo.toml".to_string()],
        ..default_pr_status()
    };
    let detail = data.to_l1("conflicts").unwrap();
    assert!(detail.contains("src/main.rs"));
    assert!(detail.contains("Cargo.toml"));
}

#[test]
fn pr_status_to_l1_ci_no_failing() {
    let data = PrStatusData {
        ci_status: Some("pass".to_string()),
        ..default_pr_status()
    };
    let detail = data.to_l1("ci").unwrap();
    assert!(detail.contains("pass"));
}

#[test]
fn pr_status_to_l1_ci_with_failing() {
    let data = PrStatusData {
        ci_status: Some("fail".to_string()),
        ci_failing_checks: vec!["build".to_string(), "tests".to_string()],
        ..default_pr_status()
    };
    let detail = data.to_l1("ci").unwrap();
    assert!(detail.contains("build"));
    assert!(detail.contains("tests"));
}

#[test]
fn pr_status_to_l1_diff_no_data() {
    let data = default_pr_status();
    let detail = data.to_l1("diff").unwrap();
    assert!(detail.contains("no diff data"));
}

#[test]
fn pr_status_to_l1_diff_with_data() {
    let data = PrStatusData {
        additions: Some(50),
        deletions: Some(10),
        changed_files: vec!["foo.rs".to_string()],
        ..default_pr_status()
    };
    let detail = data.to_l1("diff").unwrap();
    assert!(detail.contains("+50/-10"));
    assert!(detail.contains("foo.rs"));
}

#[test]
fn pr_status_to_l1_sync_no_upstream() {
    let data = default_pr_status();
    let detail = data.to_l1("sync").unwrap();
    assert!(detail.contains("No upstream"));
}

#[test]
fn pr_status_to_l1_sync_with_upstream() {
    let data = PrStatusData {
        has_upstream: true,
        ahead: Some(2),
        behind: Some(1),
        ..default_pr_status()
    };
    let detail = data.to_l1("sync").unwrap();
    assert!(detail.contains("2 ahead"));
    assert!(detail.contains("1 behind"));
}

#[test]
fn pr_status_to_l1_review() {
    let data = PrStatusData {
        review_state: Some("approved".to_string()),
        ..default_pr_status()
    };
    let detail = data.to_l1("review").unwrap();
    assert!(detail.contains("approved"));
}

#[test]
fn pr_status_to_l1_automerge_enabled() {
    let data = PrStatusData {
        automerge: true,
        ..default_pr_status()
    };
    let detail = data.to_l1("automerge").unwrap();
    assert!(detail.contains("enabled"));
}

#[test]
fn pr_status_to_l1_automerge_disabled() {
    let data = default_pr_status();
    let detail = data.to_l1("automerge").unwrap();
    assert!(detail.contains("disabled"));
}

#[test]
fn pr_status_to_l1_staleness_with_days() {
    let data = PrStatusData {
        staleness_days: Some(14),
        ..default_pr_status()
    };
    let detail = data.to_l1("staleness").unwrap();
    assert!(detail.contains("14 days"));
}

#[test]
fn pr_status_to_l1_staleness_no_data() {
    let data = default_pr_status();
    let detail = data.to_l1("staleness").unwrap();
    assert!(detail.contains("not available"));
}

#[test]
fn pr_status_to_l1_github_connected() {
    let data = default_pr_status();
    let detail = data.to_l1("github").unwrap();
    assert!(detail.contains("connected"));
}

#[test]
fn pr_status_to_l1_github_no_token() {
    let data = PrStatusData {
        github_status: GitHubStatus::NoToken,
        ..default_pr_status()
    };
    let detail = data.to_l1("github").unwrap();
    assert!(detail.contains("not configured") || detail.contains("GITHUB_TOKEN"));
}

#[test]
fn pr_status_to_l1_unknown_field_returns_error() {
    let data = default_pr_status();
    let err = data.to_l1("invalid_field");
    assert!(err.is_err(), "unknown field should return Err");
    let msg = err.unwrap_err();
    assert!(
        msg.contains("invalid_field"),
        "error should name the invalid field"
    );
}
