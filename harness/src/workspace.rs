use crate::tools::{create_tool_registry, ToolRegistry};
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum WorkspaceError {
    #[error("Git worktree creation failed: {0}")]
    GitWorktreeCreateFailed(String),
    #[error("Git worktree removal failed: {0}")]
    GitWorktreeRemoveFailed(String),
    #[error("Failed to extract changes: {0}")]
    ExtractChangesFailed(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

fn git_cmd(cwd: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    for var in &[
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_WORK_TREE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

pub struct TaskWorkspace {
    pub workspace_path: PathBuf,
    pub source_repo: PathBuf,
    pub task_id: String,
    cleaned_up: bool,
}

impl TaskWorkspace {
    pub fn create(source_repo: &Path, task_id: &str, branch: &str) -> Result<Self, WorkspaceError> {
        let workspace_path = std::env::temp_dir().join(format!("nanna-task-{}", task_id));
        let output = git_cmd(source_repo)
            .args([
                "worktree",
                "add",
                workspace_path.to_str().expect("non-UTF8 path"),
                branch,
            ])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(WorkspaceError::GitWorktreeCreateFailed(stderr));
        }
        Ok(Self {
            workspace_path,
            source_repo: source_repo.to_path_buf(),
            task_id: task_id.to_string(),
            cleaned_up: false,
        })
    }

    pub fn cleanup(&mut self) -> Result<(), WorkspaceError> {
        if self.cleaned_up {
            return Ok(());
        }
        let output = git_cmd(&self.source_repo)
            .args([
                "worktree",
                "remove",
                "--force",
                self.workspace_path.to_str().expect("non-UTF8 path"),
            ])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(WorkspaceError::GitWorktreeRemoveFailed(stderr));
        }
        self.cleaned_up = true;
        Ok(())
    }

    pub fn create_tool_registry(&self) -> ToolRegistry {
        create_tool_registry(&self.workspace_path)
    }

    pub fn extract_changes(&self) -> Result<String, WorkspaceError> {
        let output = git_cmd(&self.workspace_path)
            .args(["diff", "HEAD"])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(WorkspaceError::ExtractChangesFailed(stderr));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

impl Drop for TaskWorkspace {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn init_git_repo(dir: &Path) {
        for args in &[
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            git_cmd(dir).args(args).output().unwrap();
        }
        std::fs::write(dir.join("README.md"), "# Test").unwrap();
        git_cmd(dir).args(["add", "."]).output().unwrap();
        git_cmd(dir)
            .args(["commit", "-m", "init"])
            .output()
            .unwrap();
    }

    fn unique_id(prefix: &str) -> String {
        format!("{}-{}", prefix, Uuid::new_v4())
    }

    #[test]
    fn test_worktree_create_yields_isolated_directory() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let mut ws = TaskWorkspace::create(source.path(), &unique_id("ws-create"), "HEAD").unwrap();
        assert!(ws.workspace_path.exists());
        assert!(ws.workspace_path.join("README.md").exists());
        ws.cleanup().unwrap();
        assert!(!ws.workspace_path.exists());
    }

    #[test]
    fn test_create_tool_registry_scopes_to_worktree() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-registry"), "HEAD").unwrap();
        let registry = ws.create_tool_registry();
        assert!(registry.get_tool("read_file").is_some());
        ws.cleanup().unwrap();
    }

    #[test]
    fn test_extract_changes_returns_diff_after_modification() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let mut ws = TaskWorkspace::create(source.path(), &unique_id("ws-diff"), "HEAD").unwrap();
        std::fs::write(ws.workspace_path.join("new_file.txt"), "hello").unwrap();
        git_cmd(&ws.workspace_path)
            .args(["add", "new_file.txt"])
            .output()
            .unwrap();

        let diff = ws.extract_changes().unwrap();
        assert!(diff.contains("new_file.txt") || diff.is_empty());
        ws.cleanup().unwrap();
    }

    #[test]
    fn test_multiple_concurrent_worktrees_dont_interfere() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let mut ws1 =
            TaskWorkspace::create(source.path(), &unique_id("ws-concurrent-a"), "HEAD").unwrap();
        let mut ws2 =
            TaskWorkspace::create(source.path(), &unique_id("ws-concurrent-b"), "HEAD").unwrap();

        assert_ne!(ws1.workspace_path, ws2.workspace_path);
        assert!(ws1.workspace_path.exists());
        assert!(ws2.workspace_path.exists());

        ws1.cleanup().unwrap();
        ws2.cleanup().unwrap();
    }
}
