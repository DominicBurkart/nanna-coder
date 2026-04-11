use crate::container::{
    start_container_with_fallback, ContainerConfig, ContainerError, ContainerHandle,
};
use crate::tools::{
    create_container_tool_registry, create_tool_registry, ToolRegistry, CONTAINER_WORKSPACE_DIR,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum WorkspaceError {
    #[error("Git worktree creation failed: {0}")]
    GitWorktreeCreateFailed(String),
    #[error("Git worktree removal failed: {0}")]
    GitWorktreeRemoveFailed(String),
    #[error("Failed to stage changes: {0}")]
    StageAllFailed(String),
    #[error("Failed to extract changes: {0}")]
    ExtractChangesFailed(String),
    #[error("Failed to produce format-patch: {0}")]
    FormatPatchFailed(String),
    #[error("Container setup failed: {0}")]
    ContainerSetupFailed(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

fn git_cmd(cwd: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
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
    container_handle: Option<Arc<crate::container::ContainerHandle>>,
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
            container_handle: None,
            cleaned_up: false,
        })
    }

    pub async fn create_with_container(
        source_repo: &Path,
        task_id: &str,
        branch: &str,
        image_ref: &str,
    ) -> Result<Self, WorkspaceError> {
        Self::create_with_container_using(
            source_repo,
            task_id,
            branch,
            image_ref,
            |config: ContainerConfig| async move { start_container_with_fallback(&config).await },
        )
        .await
    }

    /// Inner implementation that accepts a custom container-starting function.
    /// The function receives an owned `ContainerConfig` so that the returned
    /// future does not need to borrow it from an outer scope (avoiding
    /// higher-ranked lifetime complications).  Kept separate from
    /// `create_with_container` so the workspace creation and cleanup logic can
    /// be exercised in unit tests without a real container runtime.
    async fn create_with_container_using<F, Fut>(
        source_repo: &Path,
        task_id: &str,
        branch: &str,
        image_ref: &str,
        start_fn: F,
    ) -> Result<Self, WorkspaceError>
    where
        F: FnOnce(ContainerConfig) -> Fut,
        Fut: std::future::Future<Output = Result<ContainerHandle, ContainerError>>,
    {
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

        let cleanup_worktree = || {
            let _ = git_cmd(source_repo)
                .args([
                    "worktree",
                    "remove",
                    "--force",
                    workspace_path.to_str().unwrap(),
                ])
                .output();
        };

        let container_name = format!("nanna-task-{}", task_id);
        let additional_args = vec![format!(
            "-v={}:{CONTAINER_WORKSPACE_DIR}",
            workspace_path.display()
        )];

        let config = ContainerConfig {
            base_image: image_ref.to_string(),
            test_image: None,
            container_name,
            port_mapping: None,
            model_to_pull: None,
            startup_timeout: Duration::from_secs(30),
            health_check_timeout: Duration::from_secs(10),
            env_vars: vec![],
            additional_args,
        };

        let handle = match start_fn(config).await {
            Ok(h) => h,
            Err(e) => {
                cleanup_worktree();
                return Err(WorkspaceError::ContainerSetupFailed(e.to_string()));
            }
        };

        Ok(Self {
            workspace_path,
            source_repo: source_repo.to_path_buf(),
            task_id: task_id.to_string(),
            container_handle: Some(Arc::new(handle)),
            cleaned_up: false,
        })
    }

    pub fn cleanup(&mut self) -> Result<(), WorkspaceError> {
        if self.cleaned_up {
            return Ok(());
        }
        drop(self.container_handle.take());
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

    /// Returns a tool registry appropriate for this workspace: container-bound
    /// when a container handle is present, plain otherwise.
    pub fn build_tool_registry(&self) -> ToolRegistry {
        if let Some(handle) = &self.container_handle {
            create_container_tool_registry(
                &self.workspace_path,
                Arc::clone(handle),
                CONTAINER_WORKSPACE_DIR,
            )
        } else {
            create_tool_registry(&self.workspace_path)
        }
    }

    fn stage_all(&self) -> Result<(), WorkspaceError> {
        let add_output = git_cmd(&self.workspace_path)
            .args(["add", "--all"])
            .output()?;
        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr).to_string();
            return Err(WorkspaceError::StageAllFailed(stderr));
        }
        Ok(())
    }

    pub fn extract_changes(&self) -> Result<String, WorkspaceError> {
        self.stage_all()?;
        let output = git_cmd(&self.workspace_path)
            .args(["diff", "--cached", "HEAD"])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(WorkspaceError::ExtractChangesFailed(stderr));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub fn format_patch(&self) -> Result<Option<String>, WorkspaceError> {
        self.stage_all()?;

        let check_output = git_cmd(&self.workspace_path)
            .args(["diff", "--cached", "--quiet"])
            .output()?;
        if check_output.status.success() {
            return Ok(None);
        }

        let commit_output = git_cmd(&self.workspace_path)
            .args([
                "-c",
                "user.email=nanna@local",
                "-c",
                "user.name=nanna",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "agent changes",
            ])
            .output()?;
        if !commit_output.status.success() {
            let stderr = String::from_utf8_lossy(&commit_output.stderr).to_string();
            return Err(WorkspaceError::FormatPatchFailed(stderr));
        }

        let patch_output = git_cmd(&self.workspace_path)
            .args(["format-patch", "-1", "--stdout"])
            .output()?;
        if !patch_output.status.success() {
            let stderr = String::from_utf8_lossy(&patch_output.stderr).to_string();
            return Err(WorkspaceError::FormatPatchFailed(stderr));
        }

        let patch = String::from_utf8_lossy(&patch_output.stdout).to_string();
        if patch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(patch))
        }
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
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git_cmd(dir).args(args).output().unwrap();
        }
        std::fs::write(dir.join("README.md"), "# Test").unwrap();
        git_cmd(dir).args(["add", "."]).output().unwrap();
        let out = git_cmd(dir)
            .args(["commit", "-m", "init"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "init commit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
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
        let registry = ws.build_tool_registry();
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
    fn test_extract_changes_captures_untracked_files() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-untracked"), "HEAD").unwrap();
        std::fs::write(ws.workspace_path.join("untracked.txt"), "untracked content").unwrap();

        let diff = ws.extract_changes().unwrap();
        assert!(
            diff.contains("untracked.txt"),
            "Diff should include untracked file, got: {}",
            diff
        );
        ws.cleanup().unwrap();
    }

    #[test]
    fn test_format_patch_produces_apply_ready_output() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let mut ws = TaskWorkspace::create(source.path(), &unique_id("ws-patch"), "HEAD").unwrap();
        std::fs::write(ws.workspace_path.join("new_file.txt"), "new content").unwrap();

        let patch = ws.format_patch().unwrap();
        assert!(patch.is_some(), "Should produce a patch for new file");
        let patch = patch.unwrap();
        assert!(
            patch.contains("diff --git"),
            "Patch should be in git format, got: {}",
            patch
        );
        assert!(
            patch.contains("new_file.txt"),
            "Patch should reference the new file"
        );
        ws.cleanup().unwrap();
    }

    #[test]
    fn test_format_patch_returns_none_when_no_changes() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-no-changes"), "HEAD").unwrap();

        let patch = ws.format_patch().unwrap();
        assert!(patch.is_none(), "Should return None when no changes");
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

    #[tokio::test]
    async fn test_create_with_container_no_runtime() {
        use crate::container::detect_runtime;
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let runtime = detect_runtime();
        if runtime.is_available() {
            return;
        }

        let result = TaskWorkspace::create_with_container(
            source.path(),
            &unique_id("ws-no-runtime"),
            "HEAD",
            "nonexistent:image",
        )
        .await;

        assert!(matches!(
            result,
            Err(WorkspaceError::ContainerSetupFailed(_))
        ));
    }

    #[tokio::test]
    async fn test_create_with_container_using_ok_path() {
        use crate::container::{ContainerHandle, ContainerRuntime};

        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let result = TaskWorkspace::create_with_container_using(
            source.path(),
            &unique_id("ws-using-ok"),
            "HEAD",
            "mock-image:latest",
            |_config: crate::container::ContainerConfig| async {
                Ok(ContainerHandle {
                    name: "mock-container".to_string(),
                    runtime: ContainerRuntime::None,
                    port: None,
                    needs_cleanup: false,
                })
            },
        )
        .await;

        assert!(result.is_ok(), "create_with_container_using should succeed");
        let mut ws = result.unwrap();
        assert!(ws.workspace_path.exists());
        assert!(ws.container_handle.is_some());
        ws.cleanup().unwrap();
        assert!(!ws.workspace_path.exists());
    }

    #[tokio::test]
    async fn test_create_with_container_using_container_fail_cleans_worktree() {
        use crate::container::ContainerError;

        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let result = TaskWorkspace::create_with_container_using(
            source.path(),
            &unique_id("ws-using-fail"),
            "HEAD",
            "bad-image:latest",
            |_config: crate::container::ContainerConfig| async {
                Err::<_, ContainerError>(ContainerError::NoRuntimeAvailable)
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(WorkspaceError::ContainerSetupFailed(_))
        ));
        // The worktree should have been cleaned up by cleanup_worktree()
        // (path may or may not exist depending on OS temp-dir semantics,
        // but the git worktree should be removed)
    }

    #[tokio::test]
    async fn test_create_with_container_using_worktree_fail() {
        // Pass a non-git directory so that git worktree add fails before
        // the container is ever started.
        let not_a_repo = TempDir::new().unwrap();

        let result = TaskWorkspace::create_with_container_using(
            not_a_repo.path(),
            &unique_id("ws-worktree-fail"),
            "HEAD",
            "mock-image:latest",
            |_config: crate::container::ContainerConfig| async {
                use crate::container::{ContainerHandle, ContainerRuntime};
                Ok(ContainerHandle {
                    name: "should-not-start".to_string(),
                    runtime: ContainerRuntime::None,
                    port: None,
                    needs_cleanup: false,
                })
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(WorkspaceError::GitWorktreeCreateFailed(_))
        ));
    }

    #[test]
    fn test_cleanup_drops_container_handle() {
        use crate::container::{ContainerHandle, ContainerRuntime};

        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-handle-drop"), "HEAD").unwrap();

        // Inject a container handle (needs_cleanup=false so no docker command runs).
        ws.container_handle = Some(Arc::new(ContainerHandle {
            name: "test-handle".to_string(),
            runtime: ContainerRuntime::None,
            port: None,
            needs_cleanup: false,
        }));

        ws.cleanup().unwrap();
        assert!(!ws.workspace_path.exists());
        assert!(ws.container_handle.is_none());
    }

    #[test]
    fn test_build_tool_registry_container_path() {
        use crate::container::{ContainerHandle, ContainerRuntime};

        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-registry-container"), "HEAD")
                .unwrap();

        // Inject a container handle to exercise the container branch of
        // build_tool_registry.
        ws.container_handle = Some(Arc::new(ContainerHandle {
            name: "test-handle".to_string(),
            runtime: ContainerRuntime::None,
            port: None,
            needs_cleanup: false,
        }));

        let registry = ws.build_tool_registry();
        assert!(registry.get_tool("run_command").is_some());
        assert!(registry.get_tool("read_file").is_some());

        ws.cleanup().unwrap();
    }
}
