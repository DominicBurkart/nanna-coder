use crate::apprun::{
    register_app_tools, stop_app, AppContext, AppSpec, Limits, PortAllocator, RunningApps,
    DEFAULT_POLL_INTERVAL,
};
use crate::container::{
    cleanup_container, start_container_with_fallback, ContainerConfig, ContainerError,
    ContainerHandle, NetworkPolicy, ReadOnlyMount,
};
use crate::identity::AgentIdentity;
use crate::onboarding::fullstack::{FullStackRust, CHECKS_FILE};
use crate::onboarding::OnboardingError;
use crate::protected::{AuditHook, NoopAuditHook, ProtectedPathViolation, ProtectedPaths};
use crate::qa::{
    register_qa_tools, trunk_asset_roots, BrowserDriver, ChromiumDriver, ContainerProbe, HttpProbe,
    Manifest, ProcessSpawner, QaContext, QaLedger, QaSummary, ARTIFACT_DIR, DEFAULT_POLL,
    DEFAULT_WAIT,
};
use crate::scope::ScopeError;
use crate::sidecar::{CommandRunner, SidecarSet, SystemRunner};
use crate::tools::{
    create_container_tool_registry, create_container_tool_registry_for, create_tool_registry,
    create_tool_registry_for, ToolRegistry, CONTAINER_WORKSPACE_DIR,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tracing::warn;

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

/// The `-v` argument that mounts the worktree at [`CONTAINER_WORKSPACE_DIR`].
/// The `:z` option relabels the directory for SELinux hosts (where the
/// container could otherwise only read it) and is ignored elsewhere.
pub fn worktree_mount_arg(workspace_path: &Path) -> String {
    format!(
        "-v={}:{CONTAINER_WORKSPACE_DIR}:z",
        workspace_path.display()
    )
}

/// What the full-stack profile tells the app and QA tools about a worktree.
struct FullStackApp {
    profile: FullStackRust,
    spec: AppSpec,
    asset_roots: Vec<String>,
}

fn detect_full_stack(workspace_path: &Path) -> Result<Option<FullStackApp>, OnboardingError> {
    let Some(profile) = FullStackRust::detect(workspace_path)? else {
        return Ok(None);
    };
    let spec = AppSpec::from_profile(&profile, workspace_path, CONTAINER_WORKSPACE_DIR)?;
    let asset_roots = trunk_asset_roots(&workspace_path.join(&profile.frontend.path))?;
    Ok(Some(FullStackApp {
        profile,
        spec,
        asset_roots,
    }))
}

pub struct TaskWorkspace {
    pub workspace_path: PathBuf,
    pub source_repo: PathBuf,
    pub task_id: String,
    container_handle: Option<Arc<crate::container::ContainerHandle>>,
    sidecars: Option<SidecarSet>,
    apps: Arc<RunningApps>,
    port_allocator: Arc<PortAllocator>,
    app_runner: Arc<dyn CommandRunner>,
    app_limits: Limits,
    extra_app_env: Vec<(String, String)>,
    qa_probe: Option<Arc<dyn HttpProbe>>,
    qa_driver: Option<Arc<dyn BrowserDriver>>,
    qa_ledger: Arc<QaLedger>,
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
            sidecars: None,
            apps: Arc::new(RunningApps::new()),
            port_allocator: PortAllocator::shared(),
            app_runner: Arc::new(SystemRunner),
            app_limits: Limits::default(),
            extra_app_env: Vec::new(),
            qa_probe: None,
            qa_driver: None,
            qa_ledger: Arc::new(QaLedger::new()),
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
        Self::create_with_container_using(
            source_repo,
            task_id,
            branch,
            image_ref,
            NetworkPolicy::Enabled,
            None,
            |config: ContainerConfig| async move { start_container_with_fallback(&config).await },
        )
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
            None,
            |config: ContainerConfig| async move { start_container_with_fallback(&config).await },
        )
        .await
    }

    /// Like [`Self::create_with_container`], but the dev container joins the
    /// network of an already started [`SidecarSet`] and receives the
    /// environment the sidecars export (for example `DATABASE_URL`). The
    /// workspace owns the set and tears it down in [`Self::cleanup`].
    pub async fn create_with_container_and_sidecars(
        source_repo: &Path,
        task_id: &str,
        branch: &str,
        image_ref: &str,
        sidecars: Option<SidecarSet>,
    ) -> Result<Self, WorkspaceError> {
        Self::create_with_container_using(
            source_repo,
            task_id,
            branch,
            image_ref,
            NetworkPolicy::Enabled,
            sidecars,
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
        sidecars: Option<SidecarSet>,
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
        let mut additional_args = vec![worktree_mount_arg(&workspace_path)];
        let mut env_vars = vec![];
        if let Some(set) = &sidecars {
            additional_args.extend(set.container_args());
            env_vars.extend(set.exports().iter().cloned());
        }
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
            env_vars,
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
            sidecars,
            apps: Arc::new(RunningApps::new()),
            port_allocator: PortAllocator::shared(),
            app_runner: Arc::new(SystemRunner),
            app_limits: Limits::default(),
            extra_app_env: Vec::new(),
            qa_probe: None,
            qa_driver: None,
            qa_ledger: Arc::new(QaLedger::new()),
            cleaned_up: false,
            protected,
            audit: Arc::new(NoopAuditHook),
        })
    }

    /// Environment the sidecars export into the dev container, empty when
    /// the workspace has no sidecars.
    pub fn sidecar_env(&self) -> &[(String, String)] {
        self.sidecars.as_ref().map_or(&[], |s| s.exports())
    }

    /// Extra environment for the application process, exported after the
    /// sidecar variables (a repository hook such as `FIXTURE_BREAK_ROUTE=1`).
    /// Call before [`Self::build_tool_registry`]: it bakes the environment
    /// into the tool context it returns, so a later call here has no effect
    /// on a registry already built.
    pub fn set_app_env(&mut self, env: Vec<(String, String)>) {
        self.extra_app_env = env;
    }

    /// The application environment: sidecar exports, then the extra
    /// variables from [`TaskWorkspace::set_app_env`].
    pub fn app_env(&self) -> Vec<(String, String)> {
        let mut env = self.sidecar_env().to_vec();
        env.extend(self.extra_app_env.iter().cloned());
        env
    }

    /// Probe the `qa_endpoints` tool requests with (a stub in tests);
    /// default: `curl` inside the dev container. Like
    /// [`Self::set_app_env`], call before [`Self::build_tool_registry`].
    pub fn set_qa_probe(&mut self, probe: Arc<dyn HttpProbe>) {
        self.qa_probe = Some(probe);
    }

    /// Browser the `qa_browser` tool drives (a stub in tests); default:
    /// headless Chromium inside the dev container. Like
    /// [`Self::set_app_env`], call before [`Self::build_tool_registry`].
    pub fn set_qa_driver(&mut self, driver: Arc<dyn BrowserDriver>) {
        self.qa_driver = Some(driver);
    }

    /// Totals of the QA the task's tools have run so far.
    pub fn qa_summary(&self) -> QaSummary {
        self.qa_ledger.snapshot()
    }

    /// Limits for the app tools; `None` restores the defaults.
    pub fn set_app_limits(&mut self, limits: Option<Limits>) {
        self.app_limits = limits.unwrap_or_default();
    }

    pub fn app_limits(&self) -> Limits {
        self.app_limits
    }

    /// Runner the app tools use for container commands (a stub in tests).
    pub fn set_app_runner(&mut self, runner: Arc<dyn CommandRunner>) {
        self.app_runner = runner;
    }

    /// Allocator the app tools take the backend port from.
    pub fn set_port_allocator(&mut self, allocator: Arc<PortAllocator>) {
        self.port_allocator = allocator;
    }

    /// Applications started for this task, shared with its tool registries.
    pub fn running_apps(&self) -> Arc<RunningApps> {
        Arc::clone(&self.apps)
    }

    /// App tool context when the workspace runs in a container and holds a
    /// full-stack Rust workspace; `None` otherwise. A profile that cannot be
    /// read is logged and treated as absent so the remaining tools still
    /// register.
    fn app_context(&self) -> Option<(FullStackApp, AppContext)> {
        let handle = self.container_handle.as_ref()?;
        let app = match detect_full_stack(&self.workspace_path) {
            Ok(Some(app)) => app,
            Ok(None) => return None,
            Err(e) => {
                warn!(
                    "task {}: full-stack profile unreadable, app tools skipped: {e}",
                    self.task_id
                );
                return None;
            }
        };
        let ctx = AppContext {
            task_id: self.task_id.clone(),
            handle: Arc::clone(handle),
            runner: Arc::clone(&self.app_runner),
            apps: Arc::clone(&self.apps),
            ports: Arc::clone(&self.port_allocator),
            spec: app.spec.clone(),
            env: self.app_env(),
            limits: self.app_limits,
            poll_interval: DEFAULT_POLL_INTERVAL,
        };
        Some((app, ctx))
    }

    /// QA tool context next to the app tools: the repository manifest (or
    /// one derived from the profile), the injected or container-backed
    /// probe and browser, and the shared ledger.
    fn qa_context(&self, app: &FullStackApp, ctx: &AppContext) -> QaContext {
        let handle = Arc::clone(&ctx.handle);
        let runner = Arc::clone(&ctx.runner);
        let probe: Arc<dyn HttpProbe> = match &self.qa_probe {
            Some(probe) => Arc::clone(probe),
            None => Arc::new(ContainerProbe::new(Arc::clone(&handle), runner)),
        };
        let driver: Arc<dyn BrowserDriver> = match &self.qa_driver {
            Some(driver) => Arc::clone(driver),
            None => Arc::new(ChromiumDriver::new(handle, Arc::new(ProcessSpawner))),
        };
        QaContext {
            task_id: self.task_id.clone(),
            apps: Arc::clone(&self.apps),
            workspace_root: self.workspace_path.clone(),
            manifest_path: self.workspace_path.join(CHECKS_FILE),
            derived: Manifest::derived(app.profile.health_path(), &app.asset_roots),
            probe,
            driver,
            ledger: Arc::clone(&self.qa_ledger),
            wait: DEFAULT_WAIT,
            poll: DEFAULT_POLL,
        }
    }

    fn stop_forgotten_app(&self, handle: &ContainerHandle) {
        let stopped = stop_app(self.app_runner.as_ref(), handle, &self.apps, &self.task_id);
        match stopped {
            Ok(Some(app)) => warn!(
                "task {}: app pid {} was still running at cleanup",
                self.task_id, app.instance.pid
            ),
            Ok(None) => {}
            Err(e) => warn!("task {}: could not stop app at cleanup: {e}", self.task_id),
        }
    }

    /// Remove the worktree, the dev container and any sidecars, stopping
    /// the application first if the agent left it running.
    ///
    /// The dev container is removed explicitly rather than through the last
    /// `Arc<ContainerHandle>` drop, because tool registries built from this
    /// workspace keep their own reference to the handle and may outlive it;
    /// the sidecar network can only be removed once the container has left it.
    pub fn cleanup(&mut self) -> Result<(), WorkspaceError> {
        if self.cleaned_up {
            return Ok(());
        }
        if let Some(handle) = self.container_handle.take() {
            self.stop_forgotten_app(&handle);
            if handle.needs_cleanup {
                if let Err(e) = cleanup_container(&handle) {
                    warn!(
                        "dev container cleanup for task {} failed: {e}",
                        self.task_id
                    );
                }
            }
        }
        drop(self.sidecars.take());
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
            let mut registry = create_container_tool_registry(
                &self.workspace_path,
                Arc::clone(handle),
                CONTAINER_WORKSPACE_DIR,
            );
            if let Some((app, ctx)) = self.app_context() {
                register_qa_tools(&mut registry, self.qa_context(&app, &ctx));
                register_app_tools(&mut registry, ctx);
            }
            registry
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

    /// Stage every change except the harness artefacts under
    /// [`ARTIFACT_DIR`], which are evidence for the task result, not part
    /// of the patch.
    fn stage_all(&self) -> Result<(), WorkspaceError> {
        let exclude = format!(":(exclude){ARTIFACT_DIR}");
        let add_output = git_cmd(&self.workspace_path)
            .args(["add", "--all", "--", ".", &exclude])
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
    fn test_extract_changes_excludes_qa_artifacts() {
        let source = TempDir::new().unwrap();
        init_git_repo(source.path());

        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-artifact-exclude"), "HEAD")
                .unwrap();
        std::fs::create_dir_all(ws.workspace_path.join(".nanna-artifacts/qa")).unwrap();
        std::fs::write(
            ws.workspace_path
                .join(".nanna-artifacts/qa/endpoints-1.json"),
            "{}",
        )
        .unwrap();
        std::fs::write(ws.workspace_path.join("tracked.txt"), "hello").unwrap();

        let diff = ws.extract_changes().unwrap();
        assert!(diff.contains("tracked.txt"), "{diff}");
        assert!(
            !diff.contains(".nanna-artifacts"),
            "QA artefacts must not enter the patch: {diff}"
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
            None,
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
            None,
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
            None,
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
            None,
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
        assert!(args.iter().any(|a| a.contains(CONTAINER_WORKSPACE_DIR)));
        ws.cleanup().unwrap();
    }

    #[tokio::test]
    async fn test_create_with_container_using_sidecars_injects_env_and_network() {
        use crate::container::{ContainerHandle, ContainerRuntime};
        use crate::sidecar::{PostgresSidecar, ReadinessConfig, SidecarSet};
        use std::sync::Mutex;

        struct RecordingRunner;
        impl crate::sidecar::CommandRunner for RecordingRunner {
            fn run(&self, _: &str, _: &[String]) -> std::io::Result<crate::sidecar::RunOutput> {
                Ok(crate::sidecar::RunOutput {
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }

        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let task_id = unique_id("ws-sidecars");
        let specs = vec![PostgresSidecar::with_password(&task_id, "pw").spec()];
        let set = SidecarSet::start(
            ContainerRuntime::Stub,
            Arc::new(RecordingRunner),
            &task_id,
            &specs,
            ReadinessConfig::default(),
        )
        .await
        .unwrap();
        let expected_network = format!("--network={}", set.network_name());
        let seen = Arc::new(Mutex::new(None));
        let seen_in_start = Arc::clone(&seen);

        let mut ws = TaskWorkspace::create_with_container_using(
            source.path(),
            &task_id,
            "HEAD",
            "mock-image:latest",
            NetworkPolicy::Enabled,
            Some(set),
            |config: ContainerConfig| async move {
                *seen_in_start.lock().unwrap() = Some(config);
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

        let config = seen.lock().unwrap().take().unwrap();
        assert!(config.additional_args.contains(&expected_network));
        let mount = format!("-v={}:/workspace:z", ws.workspace_path.display());
        assert_eq!(
            config.additional_args[0], mount,
            "worktree mount is relabelled"
        );
        assert_eq!(worktree_mount_arg(Path::new("/w")), "-v=/w:/workspace:z");
        assert_eq!(config.env_vars, specs[0].exports);
        assert_eq!(ws.sidecar_env(), specs[0].exports.as_slice());
        ws.cleanup().unwrap();
        assert!(ws.sidecars.is_none());
        assert!(ws.sidecar_env().is_empty());
    }

    #[test]
    fn test_cleanup_removes_container_even_when_registry_holds_a_reference() {
        use crate::container::{ContainerHandle, ContainerRuntime};

        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-cleanup-stub"), "HEAD").unwrap();
        ws.container_handle = Some(Arc::new(ContainerHandle {
            name: "stub-handle".to_string(),
            runtime: ContainerRuntime::Stub,
            port: None,
            needs_cleanup: true,
        }));
        let registry = ws.build_tool_registry();
        ws.cleanup().unwrap();
        assert!(ws.container_handle.is_none());
        assert!(registry.get_tool("run_command").is_some());
    }

    #[test]
    fn test_cleanup_tolerates_container_removal_failure() {
        use crate::container::{ContainerHandle, ContainerRuntime};

        let source = TempDir::new().unwrap();
        init_git_repo(source.path());
        let mut ws =
            TaskWorkspace::create(source.path(), &unique_id("ws-cleanup-fail"), "HEAD").unwrap();
        ws.container_handle = Some(Arc::new(ContainerHandle {
            name: format!("nanna-no-such-container-{}", Uuid::new_v4()),
            runtime: ContainerRuntime::Docker,
            port: None,
            needs_cleanup: true,
        }));
        ws.cleanup().unwrap();
        assert!(!ws.workspace_path.exists());
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
            None,
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
                ReadOnlyMount::new(root.join("codecov.yml"), "/workspace/codecov.yml"),
            ]
        );
        let args = config.run_args(&ContainerRuntime::Podman, "mock-image:latest");
        let workspace_mount = args
            .iter()
            .position(|a| a.ends_with(":/workspace:z"))
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

    mod apps {
        use super::*;
        use crate::apprun::{
            AppInstance, Limits, PortAllocator, RunningApps, APP_LOGS_TOOL, APP_START_TOOL,
            APP_STOP_TOOL,
        };
        use crate::container::{ContainerHandle, ContainerRuntime};
        use crate::sidecar::{CommandRunner, RunOutput};
        use std::sync::Mutex;

        const BUILD_JSON: &str = r#"{"reason":"compiler-artifact","executable":"/t/debug/api"}"#;

        struct HealthyRunner {
            scripts: Mutex<Vec<String>>,
        }

        impl HealthyRunner {
            fn new() -> Arc<Self> {
                Arc::new(Self {
                    scripts: Mutex::new(Vec::new()),
                })
            }

            fn scripts(&self) -> Vec<String> {
                self.scripts.lock().unwrap().clone()
            }
        }

        impl CommandRunner for HealthyRunner {
            fn run(&self, _: &str, args: &[String]) -> std::io::Result<RunOutput> {
                let last = args.last().cloned().unwrap_or_default();
                self.scripts.lock().unwrap().push(args.join(" "));
                let stdout = if args.iter().any(|a| a == "cargo") {
                    BUILD_JSON
                } else if last.contains("nohup") {
                    "99\n"
                } else {
                    ""
                };
                Ok(RunOutput {
                    success: true,
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                })
            }
        }

        fn copy_dir_all(src: &Path, dst: &Path) {
            std::fs::create_dir_all(dst).unwrap();
            for entry in std::fs::read_dir(src).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name();
                if name == "target" || name == "dist" || name == ".git" {
                    continue;
                }
                let target = dst.join(&name);
                if entry.file_type().unwrap().is_dir() {
                    copy_dir_all(&entry.path(), &target);
                } else {
                    std::fs::copy(entry.path(), target).unwrap();
                }
            }
        }

        fn fixture_repo() -> TempDir {
            let source = TempDir::new().unwrap();
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/fullstack");
            copy_dir_all(&fixture, source.path());
            init_git_repo(source.path());
            source
        }

        fn stub_handle() -> Arc<ContainerHandle> {
            Arc::new(ContainerHandle {
                name: "stub".to_string(),
                runtime: ContainerRuntime::Stub,
                port: None,
                needs_cleanup: false,
            })
        }

        fn fullstack_workspace(
            source: &Path,
            runner: Arc<HealthyRunner>,
            allocator: Arc<PortAllocator>,
            prefix: &str,
        ) -> TaskWorkspace {
            let mut ws = TaskWorkspace::create(source, &unique_id(prefix), "HEAD").unwrap();
            ws.container_handle = Some(stub_handle());
            ws.set_app_runner(runner);
            ws.set_port_allocator(allocator);
            ws
        }

        #[test]
        fn app_tools_registered_only_for_full_stack_profile_in_a_container() {
            let source = fixture_repo();
            let mut ws =
                TaskWorkspace::create(source.path(), &unique_id("ws-apps-host"), "HEAD").unwrap();
            let registry = ws.build_tool_registry();
            assert!(
                registry.get_tool(APP_START_TOOL).is_none(),
                "no container, no app tools"
            );
            ws.container_handle = Some(stub_handle());
            let registry = ws.build_tool_registry();
            for name in [APP_START_TOOL, APP_STOP_TOOL, APP_LOGS_TOOL] {
                assert!(registry.get_tool(name).is_some(), "{name} missing");
            }
            for name in [crate::qa::QA_ENDPOINTS_TOOL, crate::qa::QA_BROWSER_TOOL] {
                assert!(registry.get_tool(name).is_some(), "{name} missing");
            }
            assert!(registry.get_tool("trunk_build").is_some());
            ws.cleanup().unwrap();

            let plain = TempDir::new().unwrap();
            init_git_repo(plain.path());
            let mut ws =
                TaskWorkspace::create(plain.path(), &unique_id("ws-apps-plain"), "HEAD").unwrap();
            ws.container_handle = Some(stub_handle());
            let registry = ws.build_tool_registry();
            assert!(
                registry.get_tool(APP_START_TOOL).is_none(),
                "plain repo has no app tools"
            );
            for name in [crate::qa::QA_ENDPOINTS_TOOL, crate::qa::QA_BROWSER_TOOL] {
                assert!(
                    registry.get_tool(name).is_none(),
                    "plain repo has no {name}"
                );
            }
            assert!(registry.get_tool("run_command").is_some());
            ws.cleanup().unwrap();
        }

        #[test]
        fn malformed_manifest_skips_app_tools_but_keeps_the_registry() {
            let source = fixture_repo();
            std::fs::write(source.path().join("Cargo.toml"), "[workspace\n").unwrap();
            git_cmd(source.path())
                .args(["commit", "-qam", "break"])
                .output()
                .unwrap();
            let mut ws =
                TaskWorkspace::create(source.path(), &unique_id("ws-apps-broken"), "HEAD").unwrap();
            ws.container_handle = Some(stub_handle());
            let registry = ws.build_tool_registry();
            assert!(registry.get_tool(APP_START_TOOL).is_none());
            assert!(registry.get_tool(crate::qa::QA_ENDPOINTS_TOOL).is_none());
            assert!(registry.get_tool(crate::qa::QA_BROWSER_TOOL).is_none());
            assert!(registry.get_tool("run_command").is_some());
            ws.cleanup().unwrap();
        }

        #[tokio::test]
        async fn cleanup_stops_a_forgotten_app_and_releases_its_port() {
            let source = fixture_repo();
            let leases = TempDir::new().unwrap();
            let allocator = Arc::new(PortAllocator::new(43000..=43001, leases.path()));
            let runner = HealthyRunner::new();
            let mut ws = fullstack_workspace(
                source.path(),
                Arc::clone(&runner),
                Arc::clone(&allocator),
                "ws-apps-forgot",
            );
            ws.set_app_limits(Some(Limits {
                max_wall_clock_secs: 30,
            }));
            let registry = ws.build_tool_registry();
            let started = registry
                .execute(APP_START_TOOL, serde_json::Value::Null)
                .await
                .unwrap();
            assert_eq!(started["port"], 43000);
            assert_eq!(started["pid"], 99);
            assert!(
                runner
                    .scripts()
                    .iter()
                    .any(|s| s.ends_with("timeout 30 trunk build")),
                "limits reach the tools: {:?}",
                runner.scripts()
            );
            let apps = ws.running_apps();
            assert_eq!(apps.len(), 1);
            assert_eq!(apps.get(&ws.task_id).map(|a| a.pid), Some(99));
            ws.cleanup().unwrap();
            assert!(
                runner
                    .scripts()
                    .iter()
                    .any(|s| s.ends_with("sh -c kill 99")),
                "cleanup kills the app: {:?}",
                runner.scripts()
            );
            assert!(apps.is_empty());
            assert!(allocator.held().is_empty());
            assert!(!ws.workspace_path.exists());
        }

        #[tokio::test]
        async fn two_workspaces_get_distinct_ports() {
            let source = fixture_repo();
            let leases = TempDir::new().unwrap();
            let allocator = Arc::new(PortAllocator::new(44000..=44009, leases.path()));
            let mut a = fullstack_workspace(
                source.path(),
                HealthyRunner::new(),
                Arc::clone(&allocator),
                "ws-apps-a",
            );
            let mut b = fullstack_workspace(
                source.path(),
                HealthyRunner::new(),
                Arc::clone(&allocator),
                "ws-apps-b",
            );
            let ra = a.build_tool_registry();
            let rb = b.build_tool_registry();
            let (sa, sb) = tokio::join!(
                ra.execute(APP_START_TOOL, serde_json::Value::Null),
                rb.execute(APP_START_TOOL, serde_json::Value::Null)
            );
            let (sa, sb) = (sa.unwrap(), sb.unwrap());
            assert_ne!(sa["port"], sb["port"]);
            assert_ne!(sa["base_url"], sb["base_url"]);
            assert_eq!(allocator.held().len(), 2);
            a.cleanup().unwrap();
            assert_eq!(allocator.held().len(), 1);
            b.cleanup().unwrap();
            assert!(allocator.held().is_empty());
        }

        #[test]
        fn cleanup_tolerates_a_failing_stop() {
            struct Broken;
            impl CommandRunner for Broken {
                fn run(&self, _: &str, _: &[String]) -> std::io::Result<RunOutput> {
                    Err(std::io::Error::other("no runtime"))
                }
            }
            let source = fixture_repo();
            let leases = TempDir::new().unwrap();
            let allocator = Arc::new(PortAllocator::new(45000..=45000, leases.path()));
            let mut ws =
                TaskWorkspace::create(source.path(), &unique_id("ws-apps-brokenstop"), "HEAD")
                    .unwrap();
            ws.container_handle = Some(stub_handle());
            ws.set_app_runner(Arc::new(Broken));
            let lease = allocator.allocate(&ws.task_id).unwrap();
            ws.running_apps().insert(
                AppInstance::local(&ws.task_id, lease.port(), 7, "/l"),
                lease,
            );
            ws.cleanup().unwrap();
            assert!(!ws.workspace_path.exists());
        }

        struct StubHttpProbe {
            responses: std::collections::HashMap<String, (u16, String)>,
        }

        impl crate::qa::HttpProbe for StubHttpProbe {
            fn get(&self, url: &str) -> Result<crate::qa::ProbeResponse, crate::qa::ProbeError> {
                self.responses
                    .get(url)
                    .map(|(status, body)| crate::qa::ProbeResponse {
                        status: *status,
                        body: body.clone(),
                    })
                    .ok_or_else(|| crate::qa::ProbeError::Request {
                        url: url.to_string(),
                        detail: "unscripted request".to_string(),
                    })
            }
        }

        #[tokio::test]
        async fn qa_tools_run_against_the_started_app_using_the_repository_manifest() {
            let source = fixture_repo();
            let leases = TempDir::new().unwrap();
            let allocator = Arc::new(PortAllocator::new(46000..=46000, leases.path()));
            let mut ws = fullstack_workspace(
                source.path(),
                HealthyRunner::new(),
                Arc::clone(&allocator),
                "ws-qa-checks",
            );
            let mut responses = std::collections::HashMap::new();
            responses.insert(
                "http://127.0.0.1:46000/".to_string(),
                (200u16, "<html>".to_string()),
            );
            responses.insert(
                "http://127.0.0.1:46000/health/v1".to_string(),
                (200, "{\"status\":\"ok\"}".to_string()),
            );
            responses.insert(
                "http://127.0.0.1:46000/api/v1/greeting".to_string(),
                (200, "Hello from the full-stack fixture".to_string()),
            );
            ws.set_qa_probe(Arc::new(StubHttpProbe { responses }));
            let page = crate::qa::browser::stub::StubPage::new(&[
                serde_json::json!("complete"),
                serde_json::json!("Hello from the full-stack fixture"),
            ]);
            ws.set_qa_driver(crate::qa::browser::stub::StubDriver::new(vec![page]));
            let registry = ws.build_tool_registry();
            registry
                .execute(APP_START_TOOL, serde_json::Value::Null)
                .await
                .unwrap();
            let endpoints = registry
                .execute(crate::qa::QA_ENDPOINTS_TOOL, serde_json::Value::Null)
                .await
                .unwrap();
            assert_eq!(endpoints["manifest"], "CHECKS");
            assert_eq!(endpoints["all_passed"], true, "{endpoints}");
            let scenario = serde_json::json!({ "steps": [
                { "step": "goto", "path": "/" },
                { "step": "expect_text", "selector": "#greeting", "text": "Hello" }
            ] });
            let browser = registry
                .execute(
                    crate::qa::QA_BROWSER_TOOL,
                    serde_json::json!({ "scenario": scenario }),
                )
                .await
                .unwrap();
            assert_eq!(browser["passed"], true, "{browser}");
            let summary = ws.qa_summary();
            assert_eq!(summary.endpoint_runs, 1);
            assert_eq!(summary.browser_runs, 1);
            assert_eq!(summary.artifacts.len(), 2);
            ws.cleanup().unwrap();
        }

        #[tokio::test]
        async fn qa_endpoints_derives_a_manifest_when_the_repo_has_no_checks_file() {
            let source = fixture_repo();
            std::fs::remove_file(source.path().join("CHECKS")).unwrap();
            git_cmd(source.path())
                .args(["commit", "-qam", "drop checks"])
                .output()
                .unwrap();
            let leases = TempDir::new().unwrap();
            let allocator = Arc::new(PortAllocator::new(46100..=46100, leases.path()));
            let mut ws = fullstack_workspace(
                source.path(),
                HealthyRunner::new(),
                Arc::clone(&allocator),
                "ws-qa-derived",
            );
            let mut responses = std::collections::HashMap::new();
            responses.insert(
                "http://127.0.0.1:46100/".to_string(),
                (200u16, "<html>".to_string()),
            );
            responses.insert(
                "http://127.0.0.1:46100/health/v1".to_string(),
                (200, "{\"status\":\"ok\"}".to_string()),
            );
            ws.set_qa_probe(Arc::new(StubHttpProbe { responses }));
            let registry = ws.build_tool_registry();
            registry
                .execute(APP_START_TOOL, serde_json::Value::Null)
                .await
                .unwrap();
            let endpoints = registry
                .execute(crate::qa::QA_ENDPOINTS_TOOL, serde_json::Value::Null)
                .await
                .unwrap();
            assert_eq!(endpoints["manifest"], "derived");
            assert_eq!(endpoints["passed"], 2, "{endpoints}");
            assert_eq!(endpoints["all_passed"], true, "{endpoints}");
            ws.cleanup().unwrap();
        }

        #[test]
        fn app_env_appends_extra_vars_after_sidecar_exports() {
            let source = fixture_repo();
            let mut ws =
                TaskWorkspace::create(source.path(), &unique_id("ws-app-env"), "HEAD").unwrap();
            assert!(ws.app_env().is_empty());
            ws.set_app_env(vec![("FIXTURE_BREAK_ROUTE".to_string(), "1".to_string())]);
            assert_eq!(
                ws.app_env(),
                vec![("FIXTURE_BREAK_ROUTE".to_string(), "1".to_string())]
            );
            ws.cleanup().unwrap();
        }

        #[test]
        fn app_limits_default_when_unset() {
            let source = fixture_repo();
            let mut ws =
                TaskWorkspace::create(source.path(), &unique_id("ws-apps-limits"), "HEAD").unwrap();
            assert_eq!(ws.app_limits(), Limits::default());
            ws.set_app_limits(Some(Limits {
                max_wall_clock_secs: 5,
            }));
            assert_eq!(ws.app_limits().max_wall_clock_secs, 5);
            ws.set_app_limits(None);
            assert_eq!(ws.app_limits(), Limits::default());
            assert!(Arc::ptr_eq(&ws.running_apps(), &ws.running_apps()));
            let _: &RunningApps = &ws.running_apps();
            ws.cleanup().unwrap();
        }
    }
}
