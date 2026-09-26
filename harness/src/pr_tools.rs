//! PR lifecycle tools for the middle loop: draft, comments, promote, close,
//! plus the issue read/comment tools that close out #187 for this branch.
//!
//! Every tool here is [`EffectClass::Repository`], so each call is reviewed
//! by the action auditor before it runs (see
//! [`crate::tools::ToolRegistry::execute`]); classifying a tool correctly is
//! the whole of its gating story.
//!
//! `git_push_branch` is the first tool in this module. It never calls
//! GitHub: it shells out to the host `git` binary against the task's
//! worktree, matching [`crate::tools::GitStatusTool`]/
//! [`crate::tools::GitDiffTool`]'s existing harness-side pattern rather than
//! running inside the dev container. It refuses to push the repository's
//! default branch, a detached `HEAD`, or any unpushed commit whose message
//! is missing this identity's trailer -- the commit-side half of the
//! identity marker the epic requires (the PR-body half lands with
//! `github_pr_open`).

use crate::effects::EffectClass;
use crate::marker::{parse_identity_from_text, render_trailer};
use crate::tools::{Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(test)]
thread_local! {
    /// Per-thread override for the `git` binary [`git_cmd`] invokes, so a
    /// test can force a process-spawn failure deterministically without
    /// mutating process-global state (`std::env`) that parallel tests share.
    static TEST_GIT_BIN_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

fn git_binary() -> String {
    #[cfg(test)]
    {
        if let Some(bin) = TEST_GIT_BIN_OVERRIDE.with(|cell| cell.borrow().clone()) {
            return bin;
        }
    }
    "git".to_string()
}

/// A `git` invocation against `workspace_root` with no terminal prompt, so a
/// missing credential fails fast instead of hanging the task.
fn git_cmd(workspace_root: &Path) -> Command {
    let mut cmd = Command::new(git_binary());
    cmd.current_dir(workspace_root);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

fn run_git(workspace_root: &Path, args: &[&str]) -> ToolResult<String> {
    let output =
        git_cmd(workspace_root)
            .args(args)
            .output()
            .map_err(|e| ToolError::ExecutionFailed {
                message: format!("failed to run git {}: {e}", args.join(" ")),
            })?;
    if !output.status.success() {
        return Err(ToolError::ExecutionFailed {
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The worktree's current branch. Refuses a detached `HEAD`, since pushing
/// one would push whatever commit `HEAD` happens to point at under no
/// branch name at all.
fn current_branch(workspace_root: &Path) -> ToolResult<String> {
    let branch = run_git(workspace_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch.is_empty() || branch == "HEAD" {
        return Err(ToolError::ExecutionFailed {
            message: "refusing to act from a detached HEAD".to_string(),
        });
    }
    Ok(branch)
}

/// The repository's default branch, resolved from `origin/HEAD` and, when
/// that symbolic ref is absent, from a `main`/`master` fallback. `None` when
/// neither could be determined.
fn default_branch(workspace_root: &Path) -> Option<String> {
    if let Ok(target) = run_git(
        workspace_root,
        &["symbolic-ref", "refs/remotes/origin/HEAD"],
    ) {
        if let Some(name) = target.strip_prefix("refs/remotes/origin/") {
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    for candidate in ["main", "master"] {
        let reference = format!("refs/remotes/origin/{candidate}");
        if run_git(
            workspace_root,
            &["rev-parse", "--verify", "--quiet", &reference],
        )
        .is_ok()
        {
            return Some(candidate.to_string());
        }
    }
    None
}

fn empty_object_schema() -> JsonSchema {
    JsonSchema {
        schema_type: SchemaType::Object,
        properties: Some(HashMap::new()),
        required: Some(vec![]),
    }
}

/// Pushes the task's current branch to its configured `origin` remote. Runs
/// on the harness side, against the worktree's own git configuration and
/// credential helpers, never inside the dev container.
///
/// Refuses to push the repository's default branch (that would be pushing
/// straight to the branch other work merges into) and refuses a detached
/// `HEAD`. Always pushes `HEAD:refs/heads/<branch>` explicitly, never a bare
/// branch name or a force/`+` refspec.
pub struct GitPushBranchTool {
    workspace_root: PathBuf,
    identity_name: String,
}

impl GitPushBranchTool {
    pub fn new(workspace_root: PathBuf, identity_name: impl Into<String>) -> Self {
        Self {
            workspace_root,
            identity_name: identity_name.into(),
        }
    }
}

/// Refuse to push if any commit reachable from `HEAD` but not yet on any
/// `origin` remote-tracking ref is missing this identity's trailer. This is
/// how the identity marker required "on commits" (as well as on PR bodies)
/// is enforced: nothing on this branch authors commits yet, so this check
/// fails closed until whatever does include the trailer itself.
fn ensure_unpushed_commits_carry_the_identity_trailer(
    workspace_root: &Path,
    identity_name: &str,
) -> ToolResult<()> {
    let shas = run_git(
        workspace_root,
        &["rev-list", "HEAD", "--not", "--remotes=origin"],
    )?;
    for sha in shas.lines().filter(|line| !line.is_empty()) {
        let message = run_git(workspace_root, &["log", "-1", "--format=%B", sha])?;
        if parse_identity_from_text(&message).as_deref() != Some(identity_name) {
            return Err(ToolError::ExecutionFailed {
                message: format!(
                    "refusing to push: commit {sha} is missing the identity trailer; add `{}` to its message",
                    render_trailer(identity_name)
                ),
            });
        }
    }
    Ok(())
}

#[async_trait]
impl Tool for GitPushBranchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "git_push_branch".to_string(),
                description:
                    "Push the current branch to its configured 'origin' remote. Refuses the repository's default branch, a detached HEAD, and any unpushed commit missing this identity's trailer."
                        .to_string(),
                parameters: empty_object_schema(),
            },
        }
    }

    async fn execute(&self, _args: Value) -> ToolResult<Value> {
        let branch = current_branch(&self.workspace_root)?;
        if let Some(default) = default_branch(&self.workspace_root) {
            if branch == default {
                return Err(ToolError::InvalidArguments {
                    message: format!(
                        "refusing to push '{branch}': it is the repository's default branch"
                    ),
                });
            }
        }
        ensure_unpushed_commits_carry_the_identity_trailer(
            &self.workspace_root,
            &self.identity_name,
        )?;
        let refspec = format!("HEAD:refs/heads/{branch}");
        run_git(&self.workspace_root, &["push", "origin", &refspec])?;
        Ok(json!({ "pushed": true, "branch": branch, "remote": "origin" }))
    }

    fn name(&self) -> &str {
        "git_push_branch"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

/// Register every PR lifecycle tool against `registry`, scoped to
/// `workspace_root` and `identity_name`.
pub fn register(registry: &mut ToolRegistry, workspace_root: &Path, identity_name: &str) {
    registry.register(Box::new(GitPushBranchTool::new(
        workspace_root.to_path_buf(),
        identity_name,
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git command failed to run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A bare `origin` plus a working checkout on `main` with one commit,
    /// for `git_push_branch` tests.
    struct PushFixture {
        _bare: TempDir,
        work: TempDir,
    }

    fn push_fixture() -> PushFixture {
        let bare = TempDir::new().unwrap();
        git(bare.path(), &["init", "--bare", "-q"]);
        let work = TempDir::new().unwrap();
        git(work.path(), &["init", "-q"]);
        git(work.path(), &["config", "user.email", "test@example.com"]);
        git(work.path(), &["config", "user.name", "Test User"]);
        std::fs::write(work.path().join("README.md"), "hello\n").unwrap();
        git(work.path(), &["add", "."]);
        git(work.path(), &["commit", "-q", "-m", "initial"]);
        git(work.path(), &["branch", "-M", "main"]);
        let bare_path = bare.path().to_str().unwrap().to_string();
        git(work.path(), &["remote", "add", "origin", &bare_path]);
        git(work.path(), &["push", "-q", "origin", "main"]);
        git(
            work.path(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        PushFixture { _bare: bare, work }
    }

    #[tokio::test]
    async fn git_push_branch_definition_and_effect_class() {
        let tool = GitPushBranchTool::new(PathBuf::from("/tmp"), "sdlc-dev");
        assert_eq!(tool.name(), "git_push_branch");
        assert_eq!(tool.effect_class(), EffectClass::Repository);
        assert_eq!(tool.definition().function.name, "git_push_branch");
    }

    #[tokio::test]
    async fn git_push_branch_pushes_a_feature_branch() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-x"]);
        std::fs::write(fixture.work.path().join("f.txt"), "x\n").unwrap();
        git(fixture.work.path(), &["add", "."]);
        git(
            fixture.work.path(),
            &["commit", "-q", "-m", "feature\n\nNanna-Identity: sdlc-dev"],
        );

        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["pushed"], json!(true));
        assert_eq!(result["branch"], json!("feature-x"));

        let refs = StdCommand::new("git")
            .args(["ls-remote", "origin", "refs/heads/feature-x"])
            .current_dir(fixture.work.path())
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&refs.stdout).contains("refs/heads/feature-x"));
    }

    #[tokio::test]
    async fn git_push_branch_refuses_a_commit_missing_the_identity_trailer() {
        let fixture = push_fixture();
        git(
            fixture.work.path(),
            &["checkout", "-q", "-b", "feature-untrailed"],
        );
        std::fs::write(fixture.work.path().join("f.txt"), "x\n").unwrap();
        git(fixture.work.path(), &["add", "."]);
        git(
            fixture.work.path(),
            &["commit", "-q", "-m", "no trailer here"],
        );

        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("missing the identity trailer"));

        let refs = StdCommand::new("git")
            .args(["ls-remote", "origin", "refs/heads/feature-untrailed"])
            .current_dir(fixture.work.path())
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&refs.stdout).is_empty(),
            "must not push when a commit is missing the trailer"
        );
    }

    #[tokio::test]
    async fn git_push_branch_refuses_the_default_branch() {
        let fixture = push_fixture();
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        assert!(err.to_string().contains("default branch"));
    }

    #[tokio::test]
    async fn git_push_branch_refuses_a_detached_head() {
        let fixture = push_fixture();
        let sha = run_git(fixture.work.path(), &["rev-parse", "HEAD"]).unwrap();
        git(fixture.work.path(), &["checkout", "-q", &sha]);
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("detached HEAD"));
    }

    #[tokio::test]
    async fn git_push_branch_reports_a_push_failure() {
        let fixture = push_fixture();
        git(fixture.work.path(), &["checkout", "-q", "-b", "feature-y"]);
        std::fs::write(fixture.work.path().join("f.txt"), "y\n").unwrap();
        git(fixture.work.path(), &["add", "."]);
        git(
            fixture.work.path(),
            &["commit", "-q", "-m", "feature\n\nNanna-Identity: sdlc-dev"],
        );
        git(
            fixture.work.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "/nanna-pr-tools-nonexistent-remote",
            ],
        );
        let tool = GitPushBranchTool::new(fixture.work.path().to_path_buf(), "sdlc-dev");
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn run_git_reports_a_process_spawn_failure() {
        TEST_GIT_BIN_OVERRIDE.with(|cell| {
            *cell.borrow_mut() = Some("nanna-pr-tools-nonexistent-git-binary".to_string())
        });
        let err = run_git(Path::new("."), &["status"]).unwrap_err();
        TEST_GIT_BIN_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(err.to_string().contains("failed to run git status"));
    }

    #[test]
    fn default_branch_falls_back_to_main_when_origin_head_is_unset() {
        let bare = TempDir::new().unwrap();
        git(bare.path(), &["init", "--bare", "-q"]);
        let work = TempDir::new().unwrap();
        git(work.path(), &["init", "-q"]);
        git(work.path(), &["config", "user.email", "test@example.com"]);
        git(work.path(), &["config", "user.name", "Test User"]);
        std::fs::write(work.path().join("f.txt"), "x\n").unwrap();
        git(work.path(), &["add", "."]);
        git(work.path(), &["commit", "-q", "-m", "c"]);
        git(work.path(), &["branch", "-M", "main"]);
        let bare_path = bare.path().to_str().unwrap().to_string();
        git(work.path(), &["remote", "add", "origin", &bare_path]);
        git(work.path(), &["push", "-q", "origin", "main"]);
        git(work.path(), &["fetch", "-q", "origin"]);
        // `git fetch` auto-populates `refs/remotes/origin/HEAD` on some git
        // versions; delete it so this test actually exercises the
        // `main`/`master` rev-parse fallback rather than the symref path.
        let _ = StdCommand::new("git")
            .args(["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"])
            .current_dir(work.path())
            .output();
        assert_eq!(default_branch(work.path()), Some("main".to_string()));
    }
}
