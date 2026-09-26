//! `sandbox_deploy` / `sandbox_teardown`: a per-PR sandbox environment,
//! deployed from the repository's `.nanna/deploy.toml` target and checked
//! against its endpoint manifest, torn down explicitly or by workspace
//! cleanup.
//!
//! Both tools are [`EffectClass::Sandbox`], so [`crate::tools::ToolRegistry::execute`]
//! reviews every call through the action auditor before it runs: window and
//! coordination-lease checks are already structural there
//! ([`crate::action_auditor::RuleActionAuditor::window_and_lease_check`]
//! acquires `sandbox:<repo>:<pr>` via [`crate::leases::LeaseName::sandbox`]
//! for any `Sandbox`-class call, keyed off the caller-supplied
//! [`crate::tools::ActionSubject::pr`]), so neither tool acquires a lease
//! itself.
//!
//! [`SandboxTarget`] is intentionally small and synchronous, mirroring
//! [`crate::qa::HttpProbe`] rather than the rollout executor's
//! [`crate::rollout::TargetAdapter`]: a sandbox has one URL and one
//! lifetime, not slots and traffic percentages, and teardown must be
//! callable synchronously from [`crate::workspace::TaskWorkspace::cleanup`]
//! (itself sync, reachable from `Drop`). Only [`FakeSandboxTarget`] ships
//! here, matching [`crate::rollout::FakeAdapter`]'s precedent that a real
//! provider adapter is a separate, later piece of work.
//!
//! The manifest QA step reuses [`crate::qa::manifest::Manifest`] and
//! [`crate::qa::manifest::ManifestChecker`] directly rather than
//! [`crate::qa::QaContext`]: that context is built around the *task's own*
//! dev-container application (`app_start`, its `RunningApps` entry, its QA
//! ledger and artefact directory), a different lifecycle than a PR sandbox
//! that outlives any single QA run. [`HostHttpProbe`] issues the checks from
//! the harness host itself (not inside the dev container, where
//! [`crate::qa::manifest::ContainerProbe`] runs), over a
//! [`crate::sidecar::CommandRunner`] so it stays testable without shelling
//! out for real.

use crate::budget::{BudgetClass, CostAccountant};
use crate::deploy::{DeployError, DeployTemplate};
use crate::effects::EffectClass;
use crate::onboarding::fullstack::CHECKS_FILE;
use crate::pr_tools::{required_str, required_u64, resolve_repo};
use crate::qa::manifest::{ContainerProbe, HttpProbe, Manifest, ManifestChecker, ProbeError};
use crate::qa::{MANIFEST_SOURCE_DERIVED, MANIFEST_SOURCE_REPO};
use crate::sidecar::CommandRunner;
use crate::tools::{Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use thiserror::Error;

/// Identity/task context a tool charges its budget against.
struct BudgetContext {
    accountant: Arc<CostAccountant>,
    identity: String,
    task_id: String,
}

impl BudgetContext {
    fn new(
        accountant: Arc<CostAccountant>,
        identity: impl Into<String>,
        task_id: impl Into<String>,
    ) -> Self {
        Self {
            accountant,
            identity: identity.into(),
            task_id: task_id.into(),
        }
    }
}

/// A deployed sandbox: where it lives and when it went up, enough to tear
/// it down and to bill its lifetime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SandboxHandle {
    pub repo: String,
    pub pr: u64,
    pub url: String,
    pub deployed_at: DateTime<Utc>,
}

/// Failures deploying to, or tearing down, a sandbox.
#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("the deploy template does not declare a \"sandbox\" environment")]
    NoSandboxEnvironment,
    #[error(transparent)]
    Template(#[from] DeployError),
    #[error(transparent)]
    Manifest(#[from] crate::qa::manifest::ManifestError),
    #[error("sandbox deploy failed: {0}")]
    Deploy(String),
    #[error("sandbox teardown failed: {0}")]
    Teardown(String),
}

fn map_sandbox_error(e: SandboxError) -> ToolError {
    ToolError::ExecutionFailed {
        message: e.to_string(),
    }
}

/// Deploys and tears down one sandbox instance of an image, for one PR.
/// Synchronous so [`crate::workspace::TaskWorkspace::cleanup`] can call
/// [`Self::teardown`] directly without needing an async runtime.
pub trait SandboxTarget: Send + Sync {
    fn deploy(&self, repo: &str, pr: u64, image: &str) -> Result<SandboxHandle, SandboxError>;
    fn teardown(&self, handle: &SandboxHandle) -> Result<(), SandboxError>;
}

/// In-memory [`SandboxTarget`] that starts a tiny real HTTP listener per
/// deploy (answering every request `200 OK`) so [`ManifestChecker`] has a
/// genuine origin to probe, without reaching any real cloud provider.
/// Mirrors [`crate::rollout::FakeAdapter`]'s role for [`crate::rollout::TargetAdapter`]:
/// the only [`SandboxTarget`] this crate ships, standing in for a real
/// provider-specific adapter.
pub struct FakeSandboxTarget {
    deployed: Mutex<Vec<SandboxHandle>>,
    torn_down: Mutex<Vec<SandboxHandle>>,
    servers: Mutex<HashMap<String, std::sync::mpsc::Sender<()>>>,
    fail_deploy: Mutex<bool>,
    fail_teardown: Mutex<bool>,
}

impl Default for FakeSandboxTarget {
    fn default() -> Self {
        Self {
            deployed: Mutex::new(Vec::new()),
            torn_down: Mutex::new(Vec::new()),
            servers: Mutex::new(HashMap::new()),
            fail_deploy: Mutex::new(false),
            fail_teardown: Mutex::new(false),
        }
    }
}

fn serve_ok_forever(listener: std::net::TcpListener, stop: std::sync::mpsc::Receiver<()>) {
    listener
        .set_nonblocking(true)
        .expect("fake sandbox listener must support non-blocking mode");
    loop {
        if stop.try_recv().is_ok() {
            return;
        }
        match listener.accept() {
            Ok((mut socket, _)) => {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf);
                let body = "ok";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes());
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => return,
        }
    }
}

impl FakeSandboxTarget {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make the next [`SandboxTarget::deploy`] call fail once.
    pub fn fail_next_deploy(&self) {
        *self.fail_deploy.lock().expect("fail_deploy poisoned") = true;
    }

    /// Make the next [`SandboxTarget::teardown`] call fail once.
    pub fn fail_next_teardown(&self) {
        *self.fail_teardown.lock().expect("fail_teardown poisoned") = true;
    }

    pub fn deployed(&self) -> Vec<SandboxHandle> {
        self.deployed.lock().expect("deployed poisoned").clone()
    }

    pub fn torn_down(&self) -> Vec<SandboxHandle> {
        self.torn_down.lock().expect("torn_down poisoned").clone()
    }
}

impl SandboxTarget for FakeSandboxTarget {
    fn deploy(&self, repo: &str, pr: u64, image: &str) -> Result<SandboxHandle, SandboxError> {
        if std::mem::take(&mut *self.fail_deploy.lock().expect("fail_deploy poisoned")) {
            return Err(SandboxError::Deploy("scripted failure".to_string()));
        }
        let _ = image;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| SandboxError::Deploy(e.to_string()))?;
        let addr = listener
            .local_addr()
            .map_err(|e| SandboxError::Deploy(e.to_string()))?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || serve_ok_forever(listener, rx));
        let handle = SandboxHandle {
            repo: repo.to_string(),
            pr,
            url: format!("http://{addr}"),
            deployed_at: Utc::now(),
        };
        self.servers
            .lock()
            .expect("servers poisoned")
            .insert(handle.url.clone(), tx);
        let mut deployed = self.deployed.lock().expect("deployed poisoned");
        deployed.push(handle.clone());
        drop(deployed);
        Ok(handle)
    }

    fn teardown(&self, handle: &SandboxHandle) -> Result<(), SandboxError> {
        if std::mem::take(&mut *self.fail_teardown.lock().expect("fail_teardown poisoned")) {
            return Err(SandboxError::Teardown("scripted failure".to_string()));
        }
        if let Some(stop) = self
            .servers
            .lock()
            .expect("servers poisoned")
            .remove(&handle.url)
        {
            let _ = stop.send(());
        }
        let mut torn_down = self.torn_down.lock().expect("torn_down poisoned");
        torn_down.push(handle.clone());
        drop(torn_down);
        Ok(())
    }
}

/// [`HttpProbe`] over the harness host's own network, for a sandbox reached
/// over a real (if loopback, in tests) address rather than the dev
/// container [`ContainerProbe`] execs into.
pub struct HostHttpProbe {
    runner: Arc<dyn CommandRunner>,
    timeout_secs: u64,
}

impl HostHttpProbe {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            timeout_secs: crate::qa::manifest::DEFAULT_PROBE_TIMEOUT_SECS,
        }
    }

    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }
}

impl HttpProbe for HostHttpProbe {
    fn get(&self, url: &str) -> Result<crate::qa::manifest::ProbeResponse, ProbeError> {
        let argv = ContainerProbe::argv(url, self.timeout_secs);
        let program = argv[0].clone();
        let args = argv[1..].to_vec();
        let output = self
            .runner
            .run(&program, &args)
            .map_err(|source| ProbeError::Spawn {
                command: format!("{program} {}", args.join(" ")),
                source,
            })?;
        if !output.success {
            return Err(ProbeError::Request {
                url: url.to_string(),
                detail: output.stderr.trim().to_string(),
            });
        }
        ContainerProbe::parse_output(url, &output.stdout)
    }
}

/// Sandboxes currently deployed, keyed by task id. Removing an entry does
/// not tear anything down; callers tear down first, then remove.
#[derive(Debug, Default)]
pub struct SandboxRegistry {
    inner: Mutex<HashMap<String, SandboxHandle>>,
}

impl SandboxRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, task_id: &str) -> Option<SandboxHandle> {
        self.lock().get(task_id).cloned()
    }

    pub fn insert(&self, task_id: &str, handle: SandboxHandle) {
        self.lock().insert(task_id.to_string(), handle);
    }

    pub fn remove(&self, task_id: &str) -> Option<SandboxHandle> {
        self.lock().remove(task_id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SandboxHandle>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn property(schema_type: SchemaType, description: &str) -> PropertySchema {
    PropertySchema {
        schema_type,
        description: Some(description.to_string()),
        items: None,
    }
}

fn definition(
    name: &str,
    description: &str,
    props: Vec<(&str, PropertySchema)>,
    required: Option<Vec<String>>,
) -> ToolDefinition {
    let properties: HashMap<String, PropertySchema> =
        props.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    ToolDefinition {
        function: FunctionDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters: JsonSchema {
                schema_type: SchemaType::Object,
                properties: Some(properties),
                required,
            },
        },
    }
}

/// The manifest to run and where it came from: the repository's `CHECKS`,
/// or one derived from the deploy template's health endpoint. Mirrors
/// [`crate::qa::QaContext::manifest`]'s fallback for the sandbox case, where
/// there is no per-task derived-manifest override to consult.
fn load_manifest(
    workspace_root: &Path,
    template: &DeployTemplate,
) -> Result<(Manifest, &'static str), SandboxError> {
    let checks = workspace_root.join(CHECKS_FILE);
    if checks.is_file() {
        return Ok((Manifest::load(&checks)?, MANIFEST_SOURCE_REPO));
    }
    let health_path = template
        .health
        .as_ref()
        .and_then(|h| h.endpoints.first())
        .map(String::as_str)
        .unwrap_or("/");
    Ok((Manifest::derived(health_path, &[]), MANIFEST_SOURCE_DERIVED))
}

/// Deploys the PR's built image to a per-PR sandbox per the repository's
/// `.nanna/deploy.toml` target, then runs the endpoint manifest QA against
/// it and returns the report.
pub struct SandboxDeployTool {
    workspace_root: PathBuf,
    target: Arc<dyn SandboxTarget>,
    probe: Arc<dyn HttpProbe>,
    sandboxes: Arc<SandboxRegistry>,
    task_id: String,
    budget: Option<BudgetContext>,
}

impl SandboxDeployTool {
    pub fn new(
        workspace_root: PathBuf,
        target: Arc<dyn SandboxTarget>,
        probe: Arc<dyn HttpProbe>,
        sandboxes: Arc<SandboxRegistry>,
        task_id: impl Into<String>,
    ) -> Self {
        Self {
            workspace_root,
            target,
            probe,
            sandboxes,
            task_id: task_id.into(),
            budget: None,
        }
    }

    /// Charge one `Sandbox`-class call against `accountant` before every
    /// deploy.
    pub fn with_budget(
        mut self,
        accountant: Arc<CostAccountant>,
        identity: impl Into<String>,
    ) -> Self {
        self.budget = Some(BudgetContext::new(
            accountant,
            identity,
            self.task_id.clone(),
        ));
        self
    }

    fn deploy_and_check(
        &self,
        repo: &str,
        pr: u64,
        image_tag: &str,
    ) -> Result<Value, SandboxError> {
        let template = DeployTemplate::load_from_repo(&self.workspace_root)?;
        if !template
            .target
            .environments
            .iter()
            .any(|env| env == "sandbox")
        {
            return Err(SandboxError::NoSandboxEnvironment);
        }
        let image = format!("{}:{image_tag}", template.target.image_ref());
        let handle = self.target.deploy(repo, pr, &image)?;
        self.sandboxes.insert(&self.task_id, handle.clone());
        let (manifest, source) = load_manifest(&self.workspace_root, &template)?;
        let report = ManifestChecker::new(Arc::clone(&self.probe)).run(&handle.url, &manifest);
        let mut result = report.to_json();
        result["pr_number"] = json!(pr);
        result["url"] = json!(handle.url);
        result["manifest"] = json!(source);
        result["all_passed"] = json!(report.all_passed());
        result["text"] = json!(report.render_text());
        Ok(result)
    }
}

#[async_trait]
impl Tool for SandboxDeployTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "sandbox_deploy",
            "Deploy the PR's built image (tagged image_tag, per the repository's registry/image in .nanna/deploy.toml) to a per-PR sandbox, then run the endpoint manifest QA against it. Result: { pr_number, url, manifest, checks, passed, failed, all_passed, text }.",
            vec![
                ("pr_number", property(SchemaType::Integer, "Pull request the sandbox is for.")),
                ("image_tag", property(SchemaType::String, "Tag of the already-built image to deploy.")),
            ],
            Some(vec!["pr_number".to_string(), "image_tag".to_string()]),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let repo = resolve_repo(&self.workspace_root)?;
        let pr = required_u64(&args, "pr_number")?;
        let image_tag = required_str(&args, "image_tag")?;
        if let Some(ctx) = &self.budget {
            ctx.accountant
                .charge_count(
                    &ctx.identity,
                    &ctx.task_id,
                    &repo,
                    BudgetClass::Sandbox,
                    Utc::now(),
                )
                .await
                .map_err(ToolError::BudgetExceeded)?;
        }
        self.deploy_and_check(&repo, pr, image_tag)
            .map_err(map_sandbox_error)
    }

    fn name(&self) -> &str {
        "sandbox_deploy"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Sandbox
    }
}

/// Tears down this task's sandbox deployment, if any. Called explicitly by
/// the agent or, for a sandbox the agent forgot, as a safety net from
/// [`crate::workspace::TaskWorkspace::cleanup`].
pub struct SandboxTeardownTool {
    target: Arc<dyn SandboxTarget>,
    sandboxes: Arc<SandboxRegistry>,
    task_id: String,
    budget: Option<BudgetContext>,
}

impl SandboxTeardownTool {
    pub fn new(
        target: Arc<dyn SandboxTarget>,
        sandboxes: Arc<SandboxRegistry>,
        task_id: impl Into<String>,
    ) -> Self {
        Self {
            target,
            sandboxes,
            task_id: task_id.into(),
            budget: None,
        }
    }

    /// Record the sandbox's lifetime (deploy to teardown) as `Sandbox`
    /// minutes against `accountant`. Never blocks the teardown itself: the
    /// time already elapsed.
    pub fn with_budget(
        mut self,
        accountant: Arc<CostAccountant>,
        identity: impl Into<String>,
    ) -> Self {
        self.budget = Some(BudgetContext::new(
            accountant,
            identity,
            self.task_id.clone(),
        ));
        self
    }
}

#[async_trait]
impl Tool for SandboxTeardownTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "sandbox_teardown",
            "Tear down this task's sandbox deployment, if one is running. Result: { torn_down, pr_number?, url? }.",
            vec![],
            None,
        )
    }

    async fn execute(&self, _args: Value) -> ToolResult<Value> {
        let Some(handle) = self.sandboxes.get(&self.task_id) else {
            return Ok(json!({ "torn_down": false }));
        };
        self.target.teardown(&handle).map_err(map_sandbox_error)?;
        self.sandboxes.remove(&self.task_id);
        if let Some(ctx) = &self.budget {
            let minutes = ((Utc::now() - handle.deployed_at).num_seconds() as f64 / 60.0).max(0.0);
            ctx.accountant
                .record_minutes(
                    &ctx.identity,
                    &ctx.task_id,
                    &handle.repo,
                    BudgetClass::Sandbox,
                    minutes,
                    Utc::now(),
                )
                .await;
        }
        Ok(json!({ "torn_down": true, "pr_number": handle.pr, "url": handle.url }))
    }

    fn name(&self) -> &str {
        "sandbox_teardown"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Sandbox
    }
}

/// Registers `sandbox_deploy` and `sandbox_teardown` against `target`,
/// sharing `sandboxes` so [`crate::workspace::TaskWorkspace::cleanup`] can
/// find and tear down whatever this task deployed.
#[allow(clippy::too_many_arguments)]
pub fn register(
    registry: &mut ToolRegistry,
    workspace_root: &Path,
    target: Arc<dyn SandboxTarget>,
    probe: Arc<dyn HttpProbe>,
    sandboxes: Arc<SandboxRegistry>,
    task_id: &str,
    budget: Option<(Arc<CostAccountant>, &str)>,
) {
    let mut deploy = SandboxDeployTool::new(
        workspace_root.to_path_buf(),
        Arc::clone(&target),
        probe,
        Arc::clone(&sandboxes),
        task_id,
    );
    let mut teardown = SandboxTeardownTool::new(target, sandboxes, task_id);
    if let Some((accountant, identity)) = budget {
        deploy = deploy.with_budget(Arc::clone(&accountant), identity);
        teardown = teardown.with_budget(accountant, identity);
    }
    registry.register(Box::new(deploy));
    registry.register(Box::new(teardown));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetConfig, BudgetLimits, InMemoryBudgetStore};
    use crate::sidecar::SystemRunner;
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

    const DEPLOY_TOML: &str = r#"
[target]
kind = "container-registry+serverless"
registry = "registry.example.invalid/ns"
image = "app"
environments = ["sandbox", "staging", "production"]

[risk]
class = "edge"

[rollout]
strategy = "gradual"
steps = [10, 50, 100]
min_step_duration = "8h"
windows = "business-hours"

[health]
endpoints = ["/health/v1"]
error_rate_max = 0.01
latency_p99_max_ms = 800
bake_time = "30m"

[rollback]
automatic = true
on_breach = "rollback"
"#;

    const CHECKS: &str = "/\n/health/v1\n/api/v1/greeting\n";

    /// A checkout with a `.nanna/deploy.toml` declaring a `sandbox`
    /// environment and a `CHECKS` manifest, matching the fixture monorepo
    /// under `tests/fixtures/fullstack`.
    fn fixture() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.email", "test@test.invalid"]);
        git(dir.path(), &["config", "user.name", "test"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(dir.path().join(".nanna")).unwrap();
        std::fs::write(dir.path().join(".nanna/deploy.toml"), DEPLOY_TOML).unwrap();
        std::fs::write(dir.path().join(CHECKS_FILE), CHECKS).unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        git(
            dir.path(),
            &["remote", "add", "origin", "git@github.com:o/n.git"],
        );
        dir
    }

    fn no_sandbox_fixture() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.email", "test@test.invalid"]);
        git(dir.path(), &["config", "user.name", "test"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(dir.path().join(".nanna")).unwrap();
        let toml = DEPLOY_TOML.replace(
            "environments = [\"sandbox\", \"staging\", \"production\"]",
            "environments = [\"staging\", \"production\"]",
        );
        std::fs::write(dir.path().join(".nanna/deploy.toml"), toml).unwrap();
        std::fs::write(dir.path().join(CHECKS_FILE), CHECKS).unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        git(
            dir.path(),
            &["remote", "add", "origin", "git@github.com:o/n.git"],
        );
        dir
    }

    fn deploy_tool(
        dir: &Path,
        target: Arc<dyn SandboxTarget>,
        sandboxes: Arc<SandboxRegistry>,
    ) -> SandboxDeployTool {
        let probe: Arc<dyn HttpProbe> = Arc::new(HostHttpProbe::new(Arc::new(SystemRunner)));
        SandboxDeployTool::new(dir.to_path_buf(), target, probe, sandboxes, "t1")
    }

    #[tokio::test]
    async fn deploy_against_the_fake_target_runs_manifest_qa_and_reports_all_checks_passing() {
        let dir = fixture();
        let target: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let sandboxes = Arc::new(SandboxRegistry::new());
        let tool = deploy_tool(dir.path(), Arc::clone(&target), Arc::clone(&sandboxes));
        let result = tool
            .execute(json!({"pr_number": 7, "image_tag": "v1"}))
            .await
            .unwrap();
        assert_eq!(result["pr_number"], 7);
        assert_eq!(result["manifest"], "CHECKS");
        assert_eq!(result["all_passed"], true, "{result}");
        assert_eq!(result["passed"], 3, "{result}");
        let checks = result["checks"].as_array().unwrap();
        let paths: Vec<&str> = checks.iter().map(|c| c["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["/", "/health/v1", "/api/v1/greeting"]);
        assert!(result["url"]
            .as_str()
            .unwrap()
            .starts_with("http://127.0.0.1:"));
        assert_eq!(sandboxes.get("t1").unwrap().pr, 7);
    }

    #[tokio::test]
    async fn deploy_derives_a_manifest_when_the_repo_has_no_checks_file() {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["config", "user.email", "test@test.invalid"]);
        git(dir.path(), &["config", "user.name", "test"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(dir.path().join(".nanna")).unwrap();
        std::fs::write(dir.path().join(".nanna/deploy.toml"), DEPLOY_TOML).unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        git(
            dir.path(),
            &["remote", "add", "origin", "git@github.com:o/n.git"],
        );
        let target: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let sandboxes = Arc::new(SandboxRegistry::new());
        let tool = deploy_tool(dir.path(), target, sandboxes);
        let result = tool
            .execute(json!({"pr_number": 1, "image_tag": "v1"}))
            .await
            .unwrap();
        assert_eq!(result["manifest"], "derived");
        let checks = result["checks"].as_array().unwrap();
        let paths: Vec<&str> = checks.iter().map(|c| c["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["/health/v1", "/"]);
    }

    #[tokio::test]
    async fn deploy_refuses_a_template_without_a_sandbox_environment() {
        let dir = no_sandbox_fixture();
        let target: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let sandboxes = Arc::new(SandboxRegistry::new());
        let tool = deploy_tool(dir.path(), target, sandboxes);
        let err = tool
            .execute(json!({"pr_number": 1, "image_tag": "v1"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn deploy_requires_pr_number_and_image_tag() {
        let dir = fixture();
        let target: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let sandboxes = Arc::new(SandboxRegistry::new());
        let tool = deploy_tool(dir.path(), target, sandboxes);
        let err = tool.execute(json!({"image_tag": "v1"})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        let dir2 = fixture();
        let target2: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let sandboxes2 = Arc::new(SandboxRegistry::new());
        let tool2 = deploy_tool(dir2.path(), target2, sandboxes2);
        let err = tool2.execute(json!({"pr_number": 1})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn deploy_surfaces_a_target_failure() {
        let dir = fixture();
        let target = Arc::new(FakeSandboxTarget::new());
        target.fail_next_deploy();
        let sandboxes = Arc::new(SandboxRegistry::new());
        let tool = deploy_tool(dir.path(), target, sandboxes);
        let err = tool
            .execute(json!({"pr_number": 1, "image_tag": "v1"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn deploy_with_budget_blocks_once_the_task_ceiling_is_reached() {
        let dir = fixture();
        let target: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let sandboxes = Arc::new(SandboxRegistry::new());
        let config = BudgetConfig {
            ci: BudgetLimits::UNLIMITED,
            sandbox: BudgetLimits {
                max_count_per_task: Some(1),
                ..BudgetLimits::UNLIMITED
            },
        };
        let accountant = Arc::new(CostAccountant::new(
            Arc::new(InMemoryBudgetStore::new()),
            config,
        ));
        let probe: Arc<dyn HttpProbe> = Arc::new(HostHttpProbe::new(Arc::new(SystemRunner)));
        let tool = SandboxDeployTool::new(dir.path().to_path_buf(), target, probe, sandboxes, "t1")
            .with_budget(Arc::clone(&accountant), "id");
        tool.execute(json!({"pr_number": 1, "image_tag": "v1"}))
            .await
            .unwrap();
        let err = tool
            .execute(json!({"pr_number": 1, "image_tag": "v1"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BudgetExceeded(_)));
        assert_eq!(accountant.task_summary("t1").sandbox.count, 1);
    }

    #[tokio::test]
    async fn teardown_tears_down_and_reports_no_sandbox_afterwards() {
        let dir = fixture();
        let target: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let sandboxes = Arc::new(SandboxRegistry::new());
        let deploy = deploy_tool(dir.path(), Arc::clone(&target), Arc::clone(&sandboxes));
        deploy
            .execute(json!({"pr_number": 3, "image_tag": "v1"}))
            .await
            .unwrap();
        let teardown = SandboxTeardownTool::new(Arc::clone(&target), Arc::clone(&sandboxes), "t1");
        let result = teardown.execute(json!({})).await.unwrap();
        assert_eq!(result["torn_down"], true);
        assert_eq!(result["pr_number"], 3);
        assert!(sandboxes.get("t1").is_none());
        let second = teardown.execute(json!({})).await.unwrap();
        assert_eq!(second["torn_down"], false);
    }

    #[tokio::test]
    async fn teardown_with_no_sandbox_reports_torn_down_false() {
        let target: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let sandboxes = Arc::new(SandboxRegistry::new());
        let tool = SandboxTeardownTool::new(target, sandboxes, "t1");
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["torn_down"], false);
    }

    #[tokio::test]
    async fn teardown_surfaces_a_target_failure_and_keeps_the_registry_entry() {
        let dir = fixture();
        let target = Arc::new(FakeSandboxTarget::new());
        let sandboxes = Arc::new(SandboxRegistry::new());
        let deploy = deploy_tool(
            dir.path(),
            Arc::clone(&target) as Arc<dyn SandboxTarget>,
            Arc::clone(&sandboxes),
        );
        deploy
            .execute(json!({"pr_number": 4, "image_tag": "v1"}))
            .await
            .unwrap();
        target.fail_next_teardown();
        let teardown = SandboxTeardownTool::new(
            target as Arc<dyn SandboxTarget>,
            Arc::clone(&sandboxes),
            "t1",
        );
        let err = teardown.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(sandboxes.get("t1").is_some());
    }

    #[tokio::test]
    async fn teardown_with_budget_records_the_sandbox_lifetime_in_minutes() {
        let target: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let sandboxes = Arc::new(SandboxRegistry::new());
        sandboxes.insert(
            "t1",
            SandboxHandle {
                repo: "o/n".to_string(),
                pr: 5,
                url: "http://127.0.0.1:1".to_string(),
                deployed_at: Utc::now() - chrono::Duration::minutes(3),
            },
        );
        let accountant = Arc::new(CostAccountant::new(
            Arc::new(InMemoryBudgetStore::new()),
            BudgetConfig::UNLIMITED,
        ));
        let teardown = SandboxTeardownTool::new(target, sandboxes, "t1")
            .with_budget(Arc::clone(&accountant), "id");
        teardown.execute(json!({})).await.unwrap();
        let summary = accountant.task_summary("t1");
        assert!(summary.sandbox.minutes >= 2.9, "{summary:?}");
    }

    #[test]
    fn tool_definitions_declare_the_documented_shape() {
        let dir = fixture();
        let target: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let sandboxes = Arc::new(SandboxRegistry::new());
        let deploy = deploy_tool(dir.path(), Arc::clone(&target), Arc::clone(&sandboxes));
        assert_eq!(deploy.name(), "sandbox_deploy");
        assert_eq!(deploy.effect_class(), EffectClass::Sandbox);
        assert_eq!(deploy.definition().function.name, "sandbox_deploy");
        assert_eq!(
            deploy.definition().function.parameters.required,
            Some(vec!["pr_number".to_string(), "image_tag".to_string()])
        );
        let teardown = SandboxTeardownTool::new(target, sandboxes, "t1");
        assert_eq!(teardown.name(), "sandbox_teardown");
        assert_eq!(teardown.effect_class(), EffectClass::Sandbox);
        assert_eq!(teardown.definition().function.name, "sandbox_teardown");
    }

    #[test]
    fn host_http_probe_accepts_a_configured_timeout() {
        let probe = HostHttpProbe::new(Arc::new(SystemRunner)).with_timeout(1);
        assert_eq!(probe.timeout_secs, 1);
    }

    struct FailingRunner;

    impl CommandRunner for FailingRunner {
        fn run(
            &self,
            _program: &str,
            _args: &[String],
        ) -> std::io::Result<crate::sidecar::RunOutput> {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such program",
            ))
        }
    }

    struct NonZeroExitRunner;

    impl CommandRunner for NonZeroExitRunner {
        fn run(
            &self,
            _program: &str,
            _args: &[String],
        ) -> std::io::Result<crate::sidecar::RunOutput> {
            Ok(crate::sidecar::RunOutput {
                success: false,
                stdout: String::new(),
                stderr: "curl: connection refused".to_string(),
            })
        }
    }

    #[test]
    fn host_http_probe_surfaces_a_spawn_failure() {
        let probe = HostHttpProbe::new(Arc::new(FailingRunner));
        let err = probe.get("http://127.0.0.1:1").unwrap_err();
        assert!(matches!(err, ProbeError::Spawn { .. }));
    }

    #[test]
    fn host_http_probe_surfaces_a_non_success_exit() {
        let probe = HostHttpProbe::new(Arc::new(NonZeroExitRunner));
        let err = probe.get("http://127.0.0.1:1").unwrap_err();
        assert!(matches!(err, ProbeError::Request { .. }));
    }

    #[test]
    fn registry_is_isolated_per_task() {
        let registry = SandboxRegistry::new();
        let handle = SandboxHandle {
            repo: "o/n".to_string(),
            pr: 1,
            url: "http://x".to_string(),
            deployed_at: Utc::now(),
        };
        registry.insert("t1", handle.clone());
        assert_eq!(registry.get("t1"), Some(handle));
        assert_eq!(registry.get("t2"), None);
        assert_eq!(registry.remove("t2"), None);
        assert!(registry.remove("t1").is_some());
        assert_eq!(registry.get("t1"), None);
    }

    #[test]
    fn register_adds_both_tools_and_wires_budget() {
        let dir = fixture();
        let mut registry = ToolRegistry::new();
        let target: Arc<dyn SandboxTarget> = Arc::new(FakeSandboxTarget::new());
        let probe: Arc<dyn HttpProbe> = Arc::new(HostHttpProbe::new(Arc::new(SystemRunner)));
        let sandboxes = Arc::new(SandboxRegistry::new());
        let accountant = Arc::new(CostAccountant::new(
            Arc::new(InMemoryBudgetStore::new()),
            BudgetConfig::UNLIMITED,
        ));
        register(
            &mut registry,
            dir.path(),
            target,
            probe,
            sandboxes,
            "t1",
            Some((accountant, "id")),
        );
        let mut names = registry.list_tools();
        names.sort_unstable();
        assert_eq!(names, vec!["sandbox_deploy", "sandbox_teardown"]);
        assert_eq!(
            registry.get_tool("sandbox_deploy").unwrap().effect_class(),
            EffectClass::Sandbox
        );
    }

    #[test]
    fn fake_sandbox_target_can_be_scripted_to_fail_once_then_succeed() {
        let target = FakeSandboxTarget::new();
        target.fail_next_deploy();
        assert!(target.deploy("o/n", 1, "img:v1").is_err());
        let handle = target.deploy("o/n", 1, "img:v1").unwrap();
        assert_eq!(target.deployed().len(), 1);
        target.fail_next_teardown();
        assert!(target.teardown(&handle).is_err());
        target.teardown(&handle).unwrap();
        assert_eq!(target.torn_down().len(), 1);
    }
}
