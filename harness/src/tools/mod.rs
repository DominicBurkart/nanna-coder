//! Tool infrastructure: trait, registry, and all tool implementations.
//!
//! Sub-modules are grouped by concern:
//! - [`registry`]: `Tool` trait, `ToolError`, `ToolResult`, and `ToolRegistry`
//! - [`utility`]: `EchoTool`, `CalculatorTool`
//! - [`filesystem`]: `ReadFileTool`, `WriteFileTool`, `ListDirTool`
//! - [`search`]: `SearchTool`
//! - [`git`]: `GitStatusTool`, `GitDiffTool`, `RunCommandTool`
//! - [`github`]: `GitHubPrStatusTool`, `PrStatusData`, `GitHubStatus`

pub mod filesystem;
pub mod git;
pub mod github;
pub mod registry;
pub mod search;
pub mod utility;

// Re-export the public surface so that callers using `crate::tools::Foo` continue to work.
pub use filesystem::{ListDirTool, ReadFileTool, WriteFileTool};
pub use git::{GitDiffTool, GitStatusTool, RunCommandTool};
pub use github::{GitHubPrStatusTool, GitHubStatus, PrStatusData};
pub use registry::{Tool, ToolError, ToolRegistry, ToolResult};
pub use search::SearchTool;
pub use utility::{CalculatorTool, EchoTool};

use serde_json::json;
use std::path::Path;

pub fn create_tool_registry(workspace_root: &Path) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    registry.register(Box::new(CalculatorTool::new()));
    registry.register(Box::new(ReadFileTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(WriteFileTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(ListDirTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(SearchTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(GitStatusTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(GitDiffTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(GitHubPrStatusTool::new(
        workspace_root.to_path_buf(),
    )));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_echo_tool() {
        let tool = EchoTool::new();
        let args = json!({ "message": "Hello, World!" });
        let result = tool.execute(args).await.unwrap();

        assert_eq!(result["echoed"], "Hello, World!");
        assert!(result["timestamp"].is_string());
    }

    #[tokio::test]
    async fn test_calculator_tool() {
        let tool = CalculatorTool::new();

        let args = json!({
            "operation": "add",
            "a": 5.0,
            "b": 3.0
        });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["result"], 8.0);

        let args = json!({
            "operation": "divide",
            "a": 10.0,
            "b": 0.0
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_registry() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        registry.register(Box::new(CalculatorTool::new()));

        assert_eq!(registry.list_tools().len(), 2);
        assert!(registry.get_tool("echo").is_some());
        assert!(registry.get_tool("calculate").is_some());
        assert!(registry.get_tool("nonexistent").is_none());

        let definitions = registry.get_definitions();
        assert_eq!(definitions.len(), 2);

        let result = registry
            .execute("echo", json!({ "message": "test" }))
            .await
            .unwrap();
        assert_eq!(result["echoed"], "test");
    }

    #[tokio::test]
    async fn test_read_file_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_read");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let test_file = temp_dir.join("test.txt");
        std::fs::write(&test_file, "line 1\nline 2\nline 3\nline 4\nline 5").unwrap();

        let tool = ReadFileTool::new(temp_dir.clone());

        let args = json!({ "path": "test.txt" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["total_lines"], 5);
        assert_eq!(result["lines_shown"], 5);

        let args = json!({ "path": "test.txt", "start_line": 2, "end_line": 4 });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["lines_shown"], 3);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_read_file_path_security() {
        let temp_dir = std::env::temp_dir().join("nanna_test_security");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let tool = ReadFileTool::new(temp_dir.clone());

        let args = json!({ "path": "../../../etc/passwd" });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        match result {
            Err(ToolError::PathSecurityViolation { .. }) => {}
            _ => panic!("Expected PathSecurityViolation error"),
        }

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_write_file_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_write");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let tool = WriteFileTool::new(temp_dir.clone());

        let args = json!({
            "path": "output.txt",
            "content": "Hello, World!"
        });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["bytes_written"], 13);

        let content = std::fs::read_to_string(temp_dir.join("output.txt")).unwrap();
        assert_eq!(content, "Hello, World!");

        let args = json!({
            "path": "subdir/nested.txt",
            "content": "Nested file"
        });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["success"], true);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_list_directory_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_list");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("file1.rs"), "").unwrap();
        std::fs::write(temp_dir.join("file2.txt"), "").unwrap();
        std::fs::create_dir_all(temp_dir.join("subdir")).unwrap();

        let tool = ListDirTool::new(temp_dir.clone());

        let args = json!({});
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 3);

        let args = json!({ "pattern": "*.rs" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 1);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_search_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_search");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(
            temp_dir.join("test.rs"),
            "fn main() {\n    println!(\"hello\");\n}",
        )
        .unwrap();
        std::fs::write(temp_dir.join("other.txt"), "no match here").unwrap();

        let tool = SearchTool::new(temp_dir.clone());

        let args = json!({ "pattern": "println" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 1);

        let args = json!({ "pattern": "fn|println" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 2);

        let args = json!({ "pattern": "println", "file_pattern": "*.txt" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 0);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_git_status_tool() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitStatusTool::new(cwd);

        let args = json!({});
        let result = tool.execute(args).await;

        if let Ok(status) = result {
            assert!(status.get("branch").is_some());
            assert!(status.get("commit").is_some());
            assert!(status.get("is_dirty").is_some());
        }
    }

    #[tokio::test]
    async fn test_git_diff_tool() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitDiffTool::new(cwd);

        let args = json!({});
        let result = tool.execute(args).await;

        if let Ok(diff) = result {
            assert!(diff.get("diff").is_some());
            assert!(diff.get("has_changes").is_some());
        }
    }

    // -- PrStatusData unit tests --

    #[test]
    fn test_pr_status_l0_full() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: Some(17),
            pr_status: Some("draft".to_string()),
            review_state: None,
            conflict_count: Some(3),
            conflict_files: vec![
                "src/a.rs".to_string(),
                "src/b.rs".to_string(),
                "src/c.rs".to_string(),
            ],
            ahead: Some(0),
            behind: Some(0),
            additions: Some(66),
            deletions: Some(233),
            ci_status: Some("fail".to_string()),
            ci_failing_checks: vec!["lint".to_string()],
            automerge: false,
            staleness_days: None,
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#42 #17 draft conflicts:3 +66/-233 ci:fail");
    }

    #[test]
    fn test_pr_status_l0_ready_approved() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: Some(17),
            pr_status: Some("ready".to_string()),
            review_state: Some("approved".to_string()),
            conflict_count: None,
            conflict_files: vec![],
            ahead: Some(0),
            behind: Some(0),
            additions: Some(12),
            deletions: Some(5),
            ci_status: Some("pass".to_string()),
            ci_failing_checks: vec![],
            automerge: true,
            staleness_days: Some(2),
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#42 #17 ready approved +12/-5 ci:pass automerge 2d");
    }

    #[test]
    fn test_pr_status_l0_no_upstream() {
        let data = PrStatusData {
            pr_number: None,
            issue_number: None,
            pr_status: None,
            review_state: None,
            conflict_count: None,
            conflict_files: vec![],
            ahead: None,
            behind: None,
            additions: Some(5),
            deletions: Some(2),
            ci_status: None,
            ci_failing_checks: vec![],
            automerge: false,
            staleness_days: None,
            branch: None,
            head_sha: Some("abc123".to_string()),
            has_upstream: false,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "abc123 no-upstream +5/-2");
    }

    #[test]
    fn test_pr_status_l0_behind() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: None,
            pr_status: Some("ready".to_string()),
            review_state: None,
            conflict_count: None,
            conflict_files: vec![],
            ahead: Some(0),
            behind: Some(3),
            additions: Some(66),
            deletions: Some(233),
            ci_status: None,
            ci_failing_checks: vec![],
            automerge: false,
            staleness_days: None,
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#42 ready behind:3 +66/-233");
    }

    #[test]
    fn test_pr_status_l0_changes_requested_with_conflicts() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: Some(17),
            pr_status: Some("ready".to_string()),
            review_state: Some("changes-requested".to_string()),
            conflict_count: Some(1),
            conflict_files: vec!["src/main.rs".to_string()],
            ahead: Some(0),
            behind: Some(0),
            additions: Some(100),
            deletions: Some(50),
            ci_status: Some("pending".to_string()),
            ci_failing_checks: vec![],
            automerge: false,
            staleness_days: None,
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(
            l0,
            "#42 #17 ready changes-requested conflicts:1 +100/-50 ci:pending"
        );
    }

    #[test]
    fn test_pr_status_l0_merged() {
        let data = PrStatusData {
            pr_number: Some(99),
            pr_status: Some("merged".to_string()),
            additions: Some(0),
            deletions: Some(0),
            has_upstream: true,
            ci_status: Some("pass".to_string()),
            github_status: GitHubStatus::Connected,
            ..Default::default()
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#99 merged +0/-0 ci:pass");
    }

    #[test]
    fn test_pr_status_l0_minimal() {
        // Minimal: just a commit SHA, no upstream, no diff
        let data = PrStatusData {
            head_sha: Some("def456".to_string()),
            has_upstream: false,
            github_status: GitHubStatus::Connected,
            ..Default::default()
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "def456 no-upstream");
    }

    #[test]
    fn test_pr_status_l1_conflicts() {
        let data = PrStatusData {
            conflict_count: Some(2),
            conflict_files: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            ..Default::default()
        };

        let detail = data.to_l1("conflicts").unwrap();
        assert!(detail.contains("Conflicting files (2)"));
        assert!(detail.contains("src/a.rs"));
        assert!(detail.contains("src/b.rs"));
    }

    #[test]
    fn test_pr_status_l1_conflicts_none() {
        let data = PrStatusData::default();
        let detail = data.to_l1("conflicts").unwrap();
        assert_eq!(detail, "No merge conflicts.");
    }

    #[test]
    fn test_pr_status_l1_ci_failing() {
        let data = PrStatusData {
            ci_status: Some("fail".to_string()),
            ci_failing_checks: vec!["lint".to_string(), "test-unit".to_string()],
            ..Default::default()
        };

        let detail = data.to_l1("ci").unwrap();
        assert!(detail.contains("CI status: fail"));
        assert!(detail.contains("lint"));
        assert!(detail.contains("test-unit"));
    }

    #[test]
    fn test_pr_status_l1_ci_passing() {
        let data = PrStatusData {
            ci_status: Some("pass".to_string()),
            ..Default::default()
        };

        let detail = data.to_l1("ci").unwrap();
        assert_eq!(detail, "CI status: pass");
    }

    #[test]
    fn test_pr_status_l1_diff() {
        let data = PrStatusData {
            additions: Some(50),
            deletions: Some(20),
            changed_files: vec!["src/main.rs".to_string(), "Cargo.toml".to_string()],
            ..Default::default()
        };

        let detail = data.to_l1("diff").unwrap();
        assert!(detail.contains("+50/-20"));
        assert!(detail.contains("Changed files (2)"));
        assert!(detail.contains("src/main.rs"));
        assert!(detail.contains("Cargo.toml"));
    }

    #[test]
    fn test_pr_status_l1_sync_no_upstream() {
        let data = PrStatusData {
            has_upstream: false,
            ..Default::default()
        };

        let detail = data.to_l1("sync").unwrap();
        assert_eq!(detail, "No upstream tracking branch configured.");
    }

    #[test]
    fn test_pr_status_l1_sync_with_upstream() {
        let data = PrStatusData {
            has_upstream: true,
            ahead: Some(3),
            behind: Some(1),
            ..Default::default()
        };

        let detail = data.to_l1("sync").unwrap();
        assert_eq!(detail, "Sync: 3 ahead, 1 behind upstream");
    }

    #[test]
    fn test_pr_status_l1_review() {
        let data = PrStatusData {
            review_state: Some("approved".to_string()),
            ..Default::default()
        };

        let detail = data.to_l1("review").unwrap();
        assert_eq!(detail, "Review state: approved");
    }

    #[test]
    fn test_pr_status_l1_automerge() {
        let data = PrStatusData {
            automerge: true,
            ..Default::default()
        };

        let detail = data.to_l1("automerge").unwrap();
        assert_eq!(detail, "Automerge: enabled");

        let data2 = PrStatusData::default();
        let detail2 = data2.to_l1("automerge").unwrap();
        assert_eq!(detail2, "Automerge: disabled");
    }

    #[test]
    fn test_pr_status_l1_staleness() {
        let data = PrStatusData {
            staleness_days: Some(5),
            ..Default::default()
        };

        let detail = data.to_l1("staleness").unwrap();
        assert_eq!(detail, "Last updated 5 days ago");
    }

    #[test]
    fn test_pr_status_l1_unknown_field() {
        let data = PrStatusData::default();
        let result = data.to_l1("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown field"));
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_definition() {
        let tool = GitHubPrStatusTool::new(std::path::PathBuf::from("/tmp"));
        let def = tool.definition();
        assert_eq!(def.function.name, "github_pr_status");
        assert!(def.function.description.contains("PR status"));

        let params = &def.function.parameters;
        let props = params.properties.as_ref().unwrap();
        assert!(props.contains_key("level"));
        assert!(props.contains_key("field"));
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_name() {
        let tool = GitHubPrStatusTool::new(std::path::PathBuf::from("/tmp"));
        assert_eq!(tool.name(), "github_pr_status");
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_l0() {
        // Test the tool executes in the current repo (git-based data only, gh may not be available)
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({});
        let result = tool.execute(args).await;

        if let Ok(status) = result {
            assert_eq!(status["level"], "l0");
            assert!(status.get("status").is_some());
            let status_str = status["status"].as_str().unwrap();
            // Should contain at least a SHA or PR number
            assert!(!status_str.is_empty());
        }
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_l1() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({ "level": "l1", "field": "sync" });
        let result = tool.execute(args).await;

        if let Ok(detail) = result {
            assert_eq!(detail["level"], "l1");
            assert_eq!(detail["field"], "sync");
            assert!(detail.get("detail").is_some());
        }
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_l1_missing_field() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({ "level": "l1" });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_invalid_level() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({ "level": "l2" });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_github_pr_status_tool_in_registry() {
        let cwd = std::env::current_dir().unwrap();
        let registry = create_tool_registry(&cwd);
        assert!(registry.get_tool("github_pr_status").is_some());
    }
}
