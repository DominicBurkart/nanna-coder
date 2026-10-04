use crate::container::{
    start_container_with_fallback, ContainerConfig, ContainerError, ContainerHandle, NetworkPolicy,
    ReadOnlyMount,
};
use crate::identity::AgentIdentity;
use crate::protected::{AuditHook, NoopAuditHook, ProtectedPathViolation, ProtectedPaths};
use crate::scope::ScopeError;
use crate::tools::{
    create_container_tool_registry, create_container_tool_registry_for, create_tool_registry,
    create_tool_registry_for, ToolRegistry, CONTAINER_WORKSPACE_DIR,
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
    #[error("Refusing to produce a patch: {0}")]
    ProtectedPath(ProtectedPathViolation),
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
    protected: ProtectedPaths,
    audit: Arc<dyn AuditHook>,
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
        let protected = ProtectedPaths::for_repo(&workspace_path);
        Ok(Self {
            workspace_path,
            source_repo: source_repo.to_path_buf(),
            task_id: task_id.to_string(),
            container_handle: None,
            cleaned_up: false,
            protected,
            audit: Arc::new(NoopAuditHook),
        })
    }

    /// Deliver protected-path violations to `hook` instead of dropping them.
    pub fn with_audit_hook(mut self, hook: Arc<dyn AuditHook>) -> Self {
        self.audit = hook;
        self
    }

    /// The protected set this workspace enforces on patches and mounts.
    pub fn protected(&self) -> &ProtectedPaths {
        &self.protected
    }

    pub async fn create_with_container(
        source_repo: &Path,
        task_id: &str,
        branch: &str,
        image_ref: &str,
    ) -> Result<Self, WorkspaceError> {
        let network = NetworkPolicy::Enabled;
        Self::create_with_container_networked(source_repo, task_id, branch, image_ref, network)
            .await
    }

    /// Like [`TaskWorkspace::create_with_container`], with the dev
    /// container's network set by `network` (see
    /// [`NetworkPolicy::for_ceiling`] for the identity-derived policy).
    pub async fn create_with_container_networked(
        source_repo: &Path,
        task_id: &str,
        branch: &str,
        image_ref: &str,
        network: NetworkPolicy,
    ) -> Result<Self, WorkspaceError> {
        Self::create_with_container_using(
            source_repo,
            task_id,
            branch,
            image_ref,
            network,
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
        network: NetworkPolicy,
        start_fn: F,
    ) -> Result<Self, WorkspaceError>
    where
        F: FnOnce(ContainerConfig) -> Fut,
        Fut: std::future::Future<Output = Result<ContainerHandle, ContainerError>>,
    {
        debug_assert!(
            task_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "task_id must be alphanumeric+hyphen to avoid path traversal, got: {task_id:?}"
        );
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
        let protected = ProtectedPaths::for_repo(&workspace_path);
        let read_only_mounts = read_only_mounts(&protected, &workspace_path);

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
            network,
            read_only_mounts,
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
            protected,
            audit: Arc::new(NoopAuditHook),
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

    /// The registry for this workspace restricted to `identity`: only tools
    /// in `scope.tools` at or below `scope.max_effect`, with file tools
    /// confined to `scope.paths` / `scope.read_paths`. `run_command` is
    /// present only with a container and a ceiling of at least `workspace`.
    pub fn build_tool_registry_for(
        &self,
        identity: &AgentIdentity,
    ) -> Result<ToolRegistry, ScopeError> {
        let root = &self.workspace_path;
        match &self.container_handle {
            Some(handle) => {
                let handle = Arc::clone(handle);
                create_container_tool_registry_for(root, handle, CONTAINER_WORKSPACE_DIR, identity)
            }
            None => create_tool_registry_for(root, identity),
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

    /// The worktree-relative paths staged for the patch, sorted, with
    /// renames reported as a deletion and an addition.
    pub fn changed_paths(&self) -> Result<Vec<String>, WorkspaceError> {
        let output = git_cmd(&self.workspace_path)
            .args([
                "diff",
                "--cached",
                "--name-only",
                "-z",
                "--no-renames",
                "HEAD",
            ])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(WorkspaceError::ExtractChangesFailed(stderr));
        }
        let listing = String::from_utf8_lossy(&output.stdout);
        let mut paths: Vec<String> = listing
            .split(' ')
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect();
        paths.sort();
        Ok(paths)
    }

    fn refuse_protected_changes(&self) -> Result<(), WorkspaceError> {
        for path in self.changed_paths()? {
            if let Err(violation) = self.protected.check(Path::new(&path)) {
                self.audit
                    .on_protected_path_violation(&self.task_id, &violation);
                return Err(WorkspaceError::ProtectedPath(violation));
            }
        }
        Ok(())
    }

    /// The staged diff against `HEAD`, refused with
    /// [`WorkspaceError::ProtectedPath`] when it touches a protected path.
    pub fn extract_changes(&self) -> Result<String, WorkspaceError> {
        self.stage_all()?;
        self.refuse_protected_changes()?;
        let output = git_cmd(&self.workspace_path)
            .args(["diff", "--cached", "HEAD"])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(WorkspaceError::ExtractChangesFailed(stderr));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// The staged changes as an apply-ready patch, `None` when there are
    /// none, and refused with [`WorkspaceError::ProtectedPath`] before
    /// anything is committed when they touch a protected path.
    pub fn format_patch(&self) -> Result<Option<String>, WorkspaceError> {
        self.stage_all()?;
        self.refuse_protected_changes()?;

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

fn read_only_mounts(protected: &ProtectedPaths, workspace_path: &Path) -> Vec<ReadOnlyMount> {
    protected
        .existing_roots(workspace_path)
        .into_iter()
        .map(|root| {
            let container = Path::new(CONTAINER_WORKSPACE_DIR).join(&root);
            ReadOnlyMount::new(workspace_path.join(root), container)
        })
        .collect()
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
            NetworkPolicy::Enabled,
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
            NetworkPolicy::Enabled,
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
            NetworkPolicy::Enabled,
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

    #[tokio::test]
    async fn test_create_with_container_using_passes_the_network_policy() {
        use crate::container::{ContainerHandle, ContainerRuntime};

        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let seen = Arc::new(std::sync::Mutex::new(None));
        let sink = Arc::clone(&seen);

        let mut ws = TaskWorkspace::create_with_container_using(
            source.path(),
            &unique_id("ws-network"),
            "HEAD",
            "mock-image:latest",
            NetworkPolicy::Disabled,
            move |config: ContainerConfig| async move {
                *sink.lock().unwrap() = Some(config);
                Ok(ContainerHandle {
                    name: "mock-container".to_string(),
                    runtime: ContainerRuntime::None,
                    port: None,
                    needs_cleanup: false,
                })
            },
        )
        .await
        .unwrap();

        let config = seen.lock().unwrap().clone().unwrap();
        assert_eq!(config.network, NetworkPolicy::Disabled);
        let args = config.run_args(&ContainerRuntime::Podman, "mock-image:latest");
        assert!(args.iter().any(|a| a == "--network=none"));
        assert!(args.iter().any(|a| a.ends_with(CONTAINER_WORKSPACE_DIR)));
        ws.cleanup().unwrap();
    }

    #[tokio::test]
    async fn test_create_with_container_fails_without_an_image() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let result = TaskWorkspace::create_with_container_networked(
            source.path(),
            &unique_id("ws-networked"),
            "HEAD",
            "nonexistent-image-for-nanna-tests:none",
            NetworkPolicy::Disabled,
        )
        .await;

        assert!(matches!(
            result,
            Err(WorkspaceError::ContainerSetupFailed(_))
        ));
        let default_network = TaskWorkspace::create_with_container(
            source.path(),
            &unique_id("ws-default-network"),
            "HEAD",
            "nonexistent-image-for-nanna-tests:none",
        )
        .await;
        assert!(matches!(
            default_network,
            Err(WorkspaceError::ContainerSetupFailed(_))
        ));
    }

    fn workspace_identity(ceiling: crate::effects::EffectClass) -> AgentIdentity {
        let mut identity = crate::identity::example();
        identity.scope.max_effect = ceiling;
        identity.scope.tools = vec!["*".parse().unwrap()];
        identity
    }

    #[test]
    fn test_build_tool_registry_for_identity_without_container() {
        use crate::effects::EffectClass;
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-identity"), "HEAD").unwrap();

        let registry = ws
            .build_tool_registry_for(&workspace_identity(EffectClass::Workspace))
            .unwrap();
        assert_eq!(registry.identity(), Some("rust-implementer"));
        assert!(registry.get_tool("read_file").is_some());
        assert!(registry.get_tool("write_file").is_some());
        assert!(registry.get_tool("run_command").is_none());
        assert!(registry.get_tool("github_pr_status").is_none());

        let mut bad = workspace_identity(EffectClass::Workspace);
        bad.scope.paths = vec!["[".to_string()];
        assert!(ws.build_tool_registry_for(&bad).is_err());
        ws.cleanup().unwrap();
    }

    #[test]
    fn test_build_tool_registry_for_identity_with_container() {
        use crate::container::{ContainerHandle, ContainerRuntime};
        use crate::effects::EffectClass;
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-identity-container"), "HEAD")
                .unwrap();
        ws.container_handle = Some(Arc::new(ContainerHandle {
            name: "test-handle".to_string(),
            runtime: ContainerRuntime::None,
            port: None,
            needs_cleanup: false,
        }));

        let workspace = ws
            .build_tool_registry_for(&workspace_identity(EffectClass::Workspace))
            .unwrap();
        assert!(workspace.get_tool("run_command").is_some());
        assert!(workspace.get_tool("github_pr_status").is_none());

        let read_only = ws
            .build_tool_registry_for(&workspace_identity(EffectClass::None))
            .unwrap();
        assert!(read_only.get_tool("run_command").is_none());
        assert!(read_only.get_tool("write_file").is_none());
        assert!(read_only.get_tool("read_file").is_some());

        let repository = ws
            .build_tool_registry_for(&workspace_identity(EffectClass::Repository))
            .unwrap();
        assert!(repository.get_tool("run_command").is_some());
        assert!(repository.get_tool("github_pr_status").is_some());
        ws.cleanup().unwrap();
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

    struct RecordingHook {
        seen: std::sync::Mutex<Vec<(String, ProtectedPathViolation)>>,
    }

    impl AuditHook for RecordingHook {
        fn on_protected_path_violation(&self, task_id: &str, violation: &ProtectedPathViolation) {
            let entry = (task_id.to_string(), violation.clone());
            self.seen.lock().unwrap().push(entry);
        }
    }

    fn commit_count(dir: &Path) -> usize {
        let out = git_cmd(dir)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    fn protected_violation(err: WorkspaceError) -> ProtectedPathViolation {
        match err {
            WorkspaceError::ProtectedPath(violation) => violation,
            other => panic!("expected ProtectedPath, got {other:?}"),
        }
    }

    #[test]
    fn test_extract_changes_refuses_a_patch_touching_an_identity_file() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let hook = Arc::new(RecordingHook {
            seen: std::sync::Mutex::new(vec![]),
        });
        let task_id = unique_id("ws-protected");
        let mut ws = TaskWorkspace::create(source.path(), &task_id, "HEAD")
            .unwrap()
            .with_audit_hook(hook.clone());
        std::fs::write(ws.workspace_path.join("src.rs"), "fn main() {}").unwrap();
        std::fs::create_dir_all(ws.workspace_path.join(".nanna/agents")).unwrap();
        std::fs::write(ws.workspace_path.join(".nanna/agents/x.toml"), "[identity]").unwrap();

        let violation = protected_violation(ws.extract_changes().unwrap_err());
        assert_eq!(violation.path, ".nanna/agents/x.toml");
        assert_eq!(violation.rule, ".nanna/**");
        assert_eq!(
            violation.to_string(),
            "`.nanna/agents/x.toml` is protected by rule `.nanna/**`: Nanna may not modify its own configuration"
        );
        let seen = hook.seen.lock().unwrap().clone();
        assert_eq!(seen, vec![(task_id, violation)]);
        ws.cleanup().unwrap();
    }

    #[test]
    fn test_format_patch_refuses_and_commits_nothing() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let mut ws = TaskWorkspace::create(source.path(), &unique_id("ws-fp"), "HEAD").unwrap();
        std::fs::create_dir_all(ws.workspace_path.join(".github/workflows")).unwrap();
        std::fs::write(
            ws.workspace_path.join(".github/workflows/ci.yml"),
            "on: push",
        )
        .unwrap();

        let violation = protected_violation(ws.format_patch().unwrap_err());
        assert_eq!(violation.rule, ".github/workflows/**");
        assert_eq!(commit_count(&ws.workspace_path), 1);
        ws.cleanup().unwrap();
    }

    #[test]
    fn test_deleting_a_tracked_protected_file_is_refused() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        std::fs::write(source.path().join("codecov.yml"), "coverage: {}").unwrap();
        git_cmd(source.path()).args(["add", "."]).output().unwrap();
        git_cmd(source.path())
            .args(["commit", "-m", "add codecov"])
            .output()
            .unwrap();
        let mut ws = TaskWorkspace::create(source.path(), &unique_id("ws-del"), "HEAD").unwrap();
        std::fs::remove_file(ws.workspace_path.join("codecov.yml")).unwrap();
        std::fs::rename(
            ws.workspace_path.join("README.md"),
            ws.workspace_path.join("windows.toml"),
        )
        .unwrap();

        let violation = protected_violation(ws.extract_changes().unwrap_err());
        assert_eq!(violation.path, "codecov.yml");
        let paths = ws.changed_paths().unwrap();
        assert_eq!(paths, vec!["README.md", "codecov.yml", "windows.toml"]);
        ws.cleanup().unwrap();
    }

    #[test]
    fn test_unprotected_changes_still_produce_patches_and_call_no_hook() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let hook = Arc::new(RecordingHook {
            seen: std::sync::Mutex::new(vec![]),
        });
        let mut ws = TaskWorkspace::create(source.path(), &unique_id("ws-ok"), "HEAD")
            .unwrap()
            .with_audit_hook(hook.clone());
        std::fs::write(ws.workspace_path.join("docs.md"), "fine").unwrap();
        assert!(ws.extract_changes().unwrap().contains("docs.md"));
        assert!(ws.format_patch().unwrap().is_some());
        assert!(hook.seen.lock().unwrap().is_empty());
        ws.cleanup().unwrap();
    }

    #[tokio::test]
    async fn test_create_with_container_using_mounts_protected_roots_read_only() {
        use crate::container::{ContainerHandle, ContainerRuntime};

        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        std::fs::create_dir_all(source.path().join(".nanna/agents")).unwrap();
        std::fs::write(source.path().join(".nanna/agents/x.toml"), "").unwrap();
        std::fs::write(source.path().join("codecov.yml"), "coverage: {}").unwrap();
        git_cmd(source.path()).args(["add", "."]).output().unwrap();
        git_cmd(source.path())
            .args(["commit", "-m", "config"])
            .output()
            .unwrap();
        let seen = Arc::new(std::sync::Mutex::new(None));
        let sink = Arc::clone(&seen);

        let mut ws = TaskWorkspace::create_with_container_using(
            source.path(),
            &unique_id("ws-ro"),
            "HEAD",
            "mock-image:latest",
            NetworkPolicy::Enabled,
            move |config: ContainerConfig| async move {
                *sink.lock().unwrap() = Some(config);
                Ok(ContainerHandle {
                    name: "mock-container".to_string(),
                    runtime: ContainerRuntime::None,
                    port: None,
                    needs_cleanup: false,
                })
            },
        )
        .await
        .unwrap();

        let config = seen.lock().unwrap().clone().unwrap();
        let root = ws.workspace_path.clone();
        assert_eq!(
            config.read_only_mounts,
            vec![
                ReadOnlyMount::new(root.join(".nanna"), "/workspace/.nanna"),
                ReadOnlyMount::new(root.join(".git"), "/workspace/.git"),
                ReadOnlyMount::new(root.join("codecov.yml"), "/workspace/codecov.yml"),
            ]
        );
        let args = config.run_args(&ContainerRuntime::Podman, "mock-image:latest");
        let workspace_mount = args
            .iter()
            .position(|a| a.ends_with(":/workspace"))
            .unwrap();
        let ro_mount = args
            .iter()
            .position(|a| a.ends_with("/workspace/.nanna:ro"))
            .unwrap();
        assert!(ro_mount > workspace_mount);
        if let Some(config_dir) = ws.protected().config_dir() {
            let config_dir = config_dir.to_string_lossy().into_owned();
            assert!(args.iter().all(|a| !a.contains(&config_dir)), "{args:?}");
        }
        ws.cleanup().unwrap();
    }

    #[test]
    fn test_changed_paths_reports_a_git_failure() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let mut ws = TaskWorkspace::create(source.path(), &unique_id("ws-broken"), "HEAD").unwrap();
        let git_file = ws.workspace_path.join(".git");
        let original = std::fs::read(&git_file).unwrap();
        std::fs::write(&git_file, "gitdir: /nonexistent/worktree").unwrap();

        let err = ws.changed_paths().unwrap_err();
        assert!(
            matches!(err, WorkspaceError::ExtractChangesFailed(_)),
            "{err:?}"
        );

        std::fs::write(&git_file, original).unwrap();
        ws.cleanup().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_changed_paths_reports_the_unquoted_name_of_a_non_ascii_protected_file() {
        use std::os::unix::ffi::OsStrExt;

        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-nonascii"), "HEAD").unwrap();
        let name = std::ffi::OsStr::from_bytes(b".nanna/agents/\xc3\xa9.toml");
        let dir = ws.workspace_path.join(".nanna/agents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(ws.workspace_path.join(name), "[identity]").unwrap();
        git_cmd(&ws.workspace_path)
            .args(["add", "-A"])
            .output()
            .unwrap();

        let paths = ws.changed_paths().unwrap();
        assert!(
            paths.iter().any(|p| p == ".nanna/agents/\u{e9}.toml"),
            "expected an unquoted non-ASCII path, got {paths:?}"
        );
        let violation = protected_violation(ws.extract_changes().unwrap_err());
        assert_eq!(violation.rule, ".nanna/**");
        ws.cleanup().unwrap();
    }
}
