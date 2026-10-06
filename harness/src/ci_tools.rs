//! CI tools for the middle loop: dispatch or re-run a GitHub Actions
//! workflow, poll its status, and fetch a failed run's logs.
//!
//! `ci_trigger` is [`EffectClass::Ci`]: triggering a workflow run costs real
//! compute and shared CI capacity, so it is reviewed by the action auditor
//! (the ceiling check `Ci` shares with `Repository`; see
//! [`crate::action_auditor::RuleActionAuditor`]) before it runs.
//!
//! `ci_status` and `ci_logs` are [`EffectClass::Repository`], not `Ci`,
//! matching this crate's existing precedent that a read-only GitHub call is
//! classified with its mutating siblings rather than demoted just because it
//! has no side effect ([`crate::tools::GitHubPrStatusTool`] is `Repository`
//! alongside the PR-mutating tools in [`crate::pr_tools`]). Classifying them
//! `Ci` would also mean every status poll competes for the same per-task/
//! per-day `Ci` budget as an actual dispatch, which is not what "budget the
//! expensive trigger" means.
//!
//! GitHub access goes through [`GithubActionsClient`] (a sibling of
//! [`crate::backlog::GithubClient`] rather than an extension of it, so the
//! existing trait, its doctest and `crate::backlog::test_support::MockGithub`
//! are untouched). Credentials and repository resolution follow
//! [`crate::pr_tools`]'s existing pattern: a `GITHUB_TOKEN` read from the
//! harness process's environment, and the repository always resolved from
//! the worktree's own `origin` remote rather than accepted as a tool
//! argument, so a tool call cannot aim the harness's token at another
//! repository.

use crate::backlog::{BacklogError, GithubActionsClient, WorkflowRun};
use crate::budget::{BudgetClass, CostAccountant};
use crate::ci_integrity::{is_workflow_file_name, verify_ci_surface, workflow_file_of_run_path};
use crate::ci_policy::CiPolicy;
use crate::deploy::DeployTemplate;
use crate::effects::EffectClass;
use crate::pr_tools::{
    current_branch, map_backlog_error, required_str, required_u64, resolve_repo,
};
use crate::tools::{Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use chrono::Utc;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Identity/task context a tool charges its budget against, shared by
/// [`CiTriggerTool`] and [`CiStatusTool`].
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

/// GitHub Actions event name a dispatched run is filtered by when
/// correlating it back to a run id.
const DISPATCH_EVENT: &str = "workflow_dispatch";

/// How long [`CiTriggerTool::correlate_run`] waits for a dispatched run to
/// appear in the workflow's run list before giving up, and how often it
/// polls. GitHub typically lists a dispatched run within a couple of
/// seconds; tests override both to keep the retry path fast and
/// deterministic.
const DEFAULT_CORRELATION_BUDGET: Duration = Duration::from_secs(30);
const DEFAULT_CORRELATION_POLL: Duration = Duration::from_millis(1500);

/// Default and maximum number of characters [`CiLogsTool`] keeps from the
/// tail of a failed job's log. A hard ceiling exists so an argument asking
/// for more cannot force the tool to hand an agent's context window an
/// unbounded amount of log text.
pub const DEFAULT_LOG_TAIL_CHARS: usize = 8_000;
pub const MAX_LOG_TAIL_CHARS: usize = 65_536;

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

fn optional_u64(args: &Value, key: &str) -> ToolResult<Option<u64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidArguments {
                message: format!("'{key}' must be a non-negative integer, got {v}"),
            }),
    }
}

fn optional_object(args: &Value, key: &str) -> ToolResult<Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(json!({})),
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        Some(other) => Err(ToolError::InvalidArguments {
            message: format!("'{key}' must be an object, got {other}"),
        }),
    }
}

/// Whether `s` is safe to interpolate as a single path segment in a GitHub
/// Actions API URL (`/repos/{repo}/actions/workflows/{workflow}/...`).
///
/// `workflow` is a model-supplied [`ci_trigger`](CiTriggerTool) argument, not
/// something read from the worktree like `repo`/`branch` are, so it cannot be
/// trusted to be a plain file name or numeric id. Without this check, a
/// value containing `/` turns one path segment into several, and the `url`
/// crate's RFC 3986 dot-segment normalization resolves a `..` segment
/// against whatever precedes it in the URL -- `workflow =
/// "../../../../other-owner/other-repo/actions/workflows/ci.yml"` rewrites
/// the request to dispatch a workflow in a *different* repository entirely
/// using this harness's own `GITHUB_TOKEN`, bypassing `resolve_repo`'s
/// guarantee that a tool call can only reach the worktree's own repo. GitHub
/// workflow file names and numeric workflow ids never need `/` or a bare
/// `.`/`..` segment, so this rejects the argument outright rather than
/// attempting to percent-encode it.
fn valid_workflow_ref(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn run_json(run: &WorkflowRun) -> Value {
    json!({
        "id": run.id,
        "name": run.name,
        "status": run.status,
        "conclusion": run.conclusion,
        "html_url": run.html_url,
        "duration_minutes": run.duration_minutes(),
    })
}

/// The tail of `text`, at most `max_chars` `char`s, cut on a `char`
/// boundary. Slicing a `str` on a byte offset that lands inside a multibyte
/// character panics; walking `char_indices` from the end keeps every cut on
/// a boundary regardless of the log's encoding.
fn tail_chars(text: &str, max_chars: usize) -> (String, bool) {
    let total = text.chars().count();
    if total <= max_chars {
        return (text.to_string(), false);
    }
    let skip = total - max_chars;
    (text.chars().skip(skip).collect(), true)
}

/// Dispatches a named workflow (`workflow_dispatch`) or re-runs a failed
/// run's failed jobs, returning a run id either way.
pub struct CiTriggerTool {
    workspace_root: PathBuf,
    client: Arc<dyn GithubActionsClient>,
    correlation_budget: Duration,
    correlation_poll: Duration,
    budget: Option<BudgetContext>,
    policy: CiPolicy,
}

impl CiTriggerTool {
    pub fn new(workspace_root: PathBuf, client: Arc<dyn GithubActionsClient>) -> Self {
        Self {
            workspace_root,
            client,
            correlation_budget: DEFAULT_CORRELATION_BUDGET,
            correlation_poll: DEFAULT_CORRELATION_POLL,
            budget: None,
            policy: CiPolicy::deny_all(),
        }
    }

    pub fn with_policy(mut self, policy: CiPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn from_repo_template(
        workspace_root: PathBuf,
        client: Arc<dyn GithubActionsClient>,
    ) -> Self {
        let policy = match DeployTemplate::load_from_repo(&workspace_root) {
            Ok(template) => template.ci,
            Err(e) => {
                tracing::warn!(
                    "ci_trigger has no usable deploy template, denying all workflows: {e}"
                );
                CiPolicy::deny_all()
            }
        };
        Self::new(workspace_root, client).with_policy(policy)
    }

    /// Override how long and how often the dispatch-correlation retry loop
    /// polls for the dispatched run to appear. Tests use a near-zero budget
    /// so a "never appeared" case runs instantly instead of for real
    /// seconds.
    pub fn with_correlation_policy(mut self, budget: Duration, poll: Duration) -> Self {
        self.correlation_budget = budget;
        self.correlation_poll = poll;
        self
    }

    /// Charge one `Ci`-class call against `accountant` for `identity`/
    /// `task_id` before every dispatch or rerun. Without this, the tool
    /// runs unmetered -- the shape `create_tool_registry_with_scope`
    /// registers it with, since it has no per-task accountant or task id to
    /// offer; [`crate::workspace::TaskWorkspace`] re-registers a
    /// budget-aware instance for real task dispatch.
    pub fn with_budget(
        mut self,
        accountant: Arc<CostAccountant>,
        identity: impl Into<String>,
        task_id: impl Into<String>,
    ) -> Self {
        self.budget = Some(BudgetContext::new(accountant, identity, task_id));
        self
    }

    /// Charge one `Ci`-class call, when budgeted, once validation has
    /// already passed -- a malformed or refused call must never burn budget
    /// or raise an escalation for work that never ran.
    async fn charge(&self, repo: &str) -> ToolResult<()> {
        let Some(ctx) = &self.budget else {
            return Ok(());
        };
        self.settle_pending_runs(ctx, repo).await;
        ctx.accountant
            .charge_count(
                &ctx.identity,
                &ctx.task_id,
                repo,
                BudgetClass::Ci,
                Utc::now(),
            )
            .await
            .map_err(ToolError::BudgetExceeded)?;
        Ok(())
    }

    async fn settle_pending_runs(&self, ctx: &BudgetContext, repo: &str) {
        for run_id in ctx.accountant.pending_runs(repo) {
            if let Ok(run) = self.client.get_workflow_run(repo, run_id).await {
                record_run_minutes(ctx, repo, &run).await;
            }
        }
    }

    /// The highest run id already listed for `branch`/[`DISPATCH_EVENT`]
    /// before a dispatch, so [`Self::correlate_run`] can tell a genuinely
    /// new run apart from one this same workflow/branch already had. Without
    /// this baseline, re-dispatching the same workflow on the same branch --
    /// the normal shape of the middle loop's fix-and-retry cycle -- would let
    /// [`Self::correlate_run`] immediately return the *previous* dispatch's
    /// run, since GitHub run ids only ever increase and the run list is
    /// non-empty from the very first poll.
    async fn max_known_run_id(
        &self,
        repo: &str,
        workflow: &str,
        branch: &str,
    ) -> Result<Option<u64>, BacklogError> {
        let runs = self
            .client
            .list_workflow_runs(repo, workflow, branch, DISPATCH_EVENT)
            .await?;
        Ok(runs.into_iter().map(|run| run.id).max())
    }

    /// `workflow_dispatch` hands back `204 No Content` with no run id, so
    /// the run is found by listing the workflow's runs filtered to `branch`
    /// and [`DISPATCH_EVENT`] and taking the newest one with an id greater
    /// than `after_id` (see [`Self::max_known_run_id`]), retrying until one
    /// appears or the budget elapses.
    async fn correlate_run(
        &self,
        repo: &str,
        workflow: &str,
        branch: &str,
        after_id: Option<u64>,
    ) -> Result<WorkflowRun, BacklogError> {
        let start = Instant::now();
        loop {
            let mut runs = self
                .client
                .list_workflow_runs(repo, workflow, branch, DISPATCH_EVENT)
                .await?;
            runs.retain(|run| after_id.is_none_or(|baseline| run.id > baseline));
            if !runs.is_empty() {
                runs.sort_by_key(|run| std::cmp::Reverse(run.id));
                return Ok(runs.remove(0));
            }
            if start.elapsed() >= self.correlation_budget {
                return Err(BacklogError::Status {
                    url: format!("workflows/{workflow}/runs (correlating a dispatch)"),
                    status: 404,
                });
            }
            tokio::time::sleep(self.correlation_poll).await;
        }
    }
}

#[async_trait]
impl Tool for CiTriggerTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "ci_trigger",
            "Dispatch a GitHub Actions workflow (workflow_dispatch) against the task's own branch, or re-run a failed run's failed jobs when 'run_id' is given. Only allowlisted workflows run, and a call is refused when the branch changes anything under .github/ or the deploy template relative to the base branch. Returns the run id either way. Result: { mode: \"dispatch\"|\"rerun\", run_id, html_url, status }.",
            vec![
                ("workflow", property(SchemaType::String, "Workflow file name (e.g. \"ci.yml\") or numeric workflow id. Required to dispatch; ignored for a rerun.")),
                ("run_id", property(SchemaType::Integer, "Run to re-run instead of dispatching a new one.")),
                ("inputs", property(SchemaType::Object, "workflow_dispatch inputs, only used when dispatching.")),
            ],
            None,
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let repo = resolve_repo(&self.workspace_root)?;
        let branch = current_branch(&self.workspace_root)?;
        if let Some(run_id) = optional_u64(&args, "run_id")? {
            let run = self
                .client
                .get_workflow_run(&repo, run_id)
                .await
                .map_err(map_backlog_error)?;
            if !run.is_rerunnable_failure_on(&branch) {
                return Err(ToolError::InvalidArguments {
                    message: format!(
                        "run {run_id} is not a failed run on this task's branch ({branch}); refusing to rerun it"
                    ),
                });
            }
            let workflow = run
                .path
                .as_deref()
                .and_then(workflow_file_of_run_path)
                .ok_or_else(|| ToolError::InvalidArguments {
                    message: format!(
                        "run {run_id} does not name a workflow file under .github/workflows/; refusing to rerun it"
                    ),
                })?;
            if !self.policy.permits(workflow) {
                return Err(ToolError::InvalidArguments {
                    message: format!(
                        "run {run_id} belongs to workflow {workflow:?}, which is not allowlisted in the repository's .nanna/deploy.toml"
                    ),
                });
            }
            let head_sha = run
                .head_sha
                .clone()
                .ok_or_else(|| ToolError::InvalidArguments {
                    message: format!(
                        "run {run_id} does not report the commit it ran; refusing to rerun it"
                    ),
                })?;
            verify_ci_surface(&self.workspace_root, &branch, &[head_sha], Some(workflow)).map_err(
                |e| ToolError::InvalidArguments {
                    message: e.to_string(),
                },
            )?;
            self.charge(&repo).await?;
            self.client
                .rerun_workflow(&repo, run_id)
                .await
                .map_err(map_backlog_error)?;
            let refreshed = self
                .client
                .get_workflow_run(&repo, run_id)
                .await
                .map_err(map_backlog_error)?;
            return Ok(json!({
                "mode": "rerun",
                "run_id": refreshed.id,
                "html_url": refreshed.html_url,
                "status": refreshed.status,
            }));
        }
        let workflow = required_str(&args, "workflow")?;
        if !valid_workflow_ref(workflow) {
            return Err(ToolError::InvalidArguments {
                message: format!(
                    "'workflow' must be a plain file name or numeric workflow id (no '/', '.', or '..' segments), got {workflow:?}"
                ),
            });
        }
        let inputs = optional_object(&args, "inputs")?;
        self.policy
            .validate_dispatch(workflow, &inputs)
            .map_err(|e| ToolError::InvalidArguments {
                message: e.to_string(),
            })?;
        verify_ci_surface(
            &self.workspace_root,
            &branch,
            &[],
            is_workflow_file_name(workflow).then_some(workflow),
        )
        .map_err(|e| ToolError::InvalidArguments {
            message: e.to_string(),
        })?;
        let baseline = self
            .max_known_run_id(&repo, workflow, &branch)
            .await
            .map_err(map_backlog_error)?;
        self.charge(&repo).await?;
        self.client
            .dispatch_workflow(&repo, workflow, &branch, inputs)
            .await
            .map_err(map_backlog_error)?;
        let run = self
            .correlate_run(&repo, workflow, &branch, baseline)
            .await
            .map_err(map_backlog_error)?;
        if let Some(ctx) = &self.budget {
            ctx.accountant.note_run(&repo, run.id);
        }
        Ok(json!({
            "mode": "dispatch",
            "run_id": run.id,
            "html_url": run.html_url,
            "status": run.status,
        }))
    }

    fn name(&self) -> &str {
        "ci_trigger"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Ci
    }
}

/// Polls a workflow run's status by id.
pub struct CiStatusTool {
    workspace_root: PathBuf,
    client: Arc<dyn GithubActionsClient>,
    budget: Option<BudgetContext>,
}

impl CiStatusTool {
    pub fn new(workspace_root: PathBuf, client: Arc<dyn GithubActionsClient>) -> Self {
        Self {
            workspace_root,
            client,
            budget: None,
        }
    }

    /// Record a completed run's minutes against `accountant` for
    /// `identity`/`task_id`, once per run id. Never blocks: the run already
    /// finished, so this only affects a *later* [`CiTriggerTool`] call
    /// through the same accountant.
    pub fn with_budget(
        mut self,
        accountant: Arc<CostAccountant>,
        identity: impl Into<String>,
        task_id: impl Into<String>,
    ) -> Self {
        self.budget = Some(BudgetContext::new(accountant, identity, task_id));
        self
    }

    async fn charge_minutes_once(&self, repo: &str, run: &WorkflowRun) {
        if let Some(ctx) = &self.budget {
            record_run_minutes(ctx, repo, run).await;
        }
    }
}

async fn record_run_minutes(ctx: &BudgetContext, repo: &str, run: &WorkflowRun) {
    let Some(minutes) = run.duration_minutes() else {
        return;
    };
    if !ctx.accountant.claim_run(repo, run.id) {
        return;
    }
    ctx.accountant
        .record_minutes(
            &ctx.identity,
            &ctx.task_id,
            repo,
            BudgetClass::Ci,
            minutes,
            Utc::now(),
        )
        .await;
}

#[async_trait]
impl Tool for CiStatusTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "ci_status",
            "Poll a GitHub Actions run's status by id. Result: { id, name, status, conclusion, html_url, duration_minutes }; duration_minutes is null until the run completes.",
            vec![(
                "run_id",
                property(SchemaType::Integer, "Run id, as returned by ci_trigger."),
            )],
            Some(vec!["run_id".to_string()]),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let repo = resolve_repo(&self.workspace_root)?;
        let run_id = required_u64(&args, "run_id")?;
        let run = self
            .client
            .get_workflow_run(&repo, run_id)
            .await
            .map_err(map_backlog_error)?;
        self.charge_minutes_once(&repo, &run).await;
        Ok(run_json(&run))
    }

    fn name(&self) -> &str {
        "ci_status"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

/// Fetches the tail of a run's failed-step logs, truncated to a
/// configurable size.
pub struct CiLogsTool {
    workspace_root: PathBuf,
    client: Arc<dyn GithubActionsClient>,
}

impl CiLogsTool {
    pub fn new(workspace_root: PathBuf, client: Arc<dyn GithubActionsClient>) -> Self {
        Self {
            workspace_root,
            client,
        }
    }
}

#[async_trait]
impl Tool for CiLogsTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "ci_logs",
            "Fetch the tail of the logs of every failed job in a run, truncated to at most 'max_chars' characters (default 8000, hard ceiling 65536). Result: { run_id, jobs: [{ id, name }], truncated, logs }.",
            vec![
                ("run_id", property(SchemaType::Integer, "Run id, as returned by ci_trigger.")),
                ("max_chars", property(SchemaType::Integer, "Maximum characters of log tail to return (default 8000, capped at 65536).")),
            ],
            Some(vec!["run_id".to_string()]),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let repo = resolve_repo(&self.workspace_root)?;
        let run_id = required_u64(&args, "run_id")?;
        let max_chars = optional_u64(&args, "max_chars")?
            .map(|n| (n as usize).clamp(1, MAX_LOG_TAIL_CHARS))
            .unwrap_or(DEFAULT_LOG_TAIL_CHARS);
        let jobs = self
            .client
            .list_workflow_jobs(&repo, run_id)
            .await
            .map_err(map_backlog_error)?;
        let failed: Vec<_> = jobs.into_iter().filter(|job| job.failed()).collect();
        let mut logs = String::new();
        for job in &failed {
            let job_logs = self
                .client
                .get_job_logs(&repo, job.id)
                .await
                .map_err(map_backlog_error)?;
            logs.push_str(&format!("=== {} ===\n", job.name));
            logs.push_str(&job_logs);
            logs.push('\n');
        }
        let (tail, truncated) = tail_chars(&logs, max_chars);
        Ok(json!({
            "run_id": run_id,
            "jobs": failed.iter().map(|job| json!({"id": job.id, "name": job.name})).collect::<Vec<_>>(),
            "truncated": truncated,
            "logs": tail,
        }))
    }

    fn name(&self) -> &str {
        "ci_logs"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

/// A fresh [`crate::backlog::ReqwestGithubClient`] built from `GITHUB_TOKEN`,
/// shared by [`register`] and by [`crate::workspace::TaskWorkspace`]'s
/// budget-aware re-registration of `ci_trigger`/`ci_status`.
pub(crate) fn github_actions_client() -> Arc<dyn GithubActionsClient> {
    let token = std::env::var("GITHUB_TOKEN").ok();
    Arc::new(crate::backlog::ReqwestGithubClient::github(token))
}

/// Registers `ci_trigger`, `ci_status` and `ci_logs` against a fresh
/// [`crate::backlog::ReqwestGithubClient`] built from `GITHUB_TOKEN`,
/// mirroring [`crate::pr_tools::register`].
pub fn register(registry: &mut ToolRegistry, workspace_root: &Path) {
    let client = github_actions_client();
    registry.register(Box::new(CiTriggerTool::from_repo_template(
        workspace_root.to_path_buf(),
        Arc::clone(&client),
    )));
    registry.register(Box::new(CiStatusTool::new(
        workspace_root.to_path_buf(),
        Arc::clone(&client),
    )));
    registry.register(Box::new(CiLogsTool::new(
        workspace_root.to_path_buf(),
        client,
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::test_support::MockGithubActions;
    use crate::backlog::{WorkflowJob, WorkflowStep};
    use crate::budget::{BudgetConfig, BudgetLimits, InMemoryBudgetStore};
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

    /// A checkout on `feat/x` with `origin` set to a GitHub-shaped URL, so
    /// `resolve_repo`/`current_branch` succeed without a real remote.
    fn fixture() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "feat/x"]);
        git(dir.path(), &["config", "user.email", "test@test.invalid"]);
        git(dir.path(), &["config", "user.name", "test"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("f"), "1").unwrap();
        std::fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
        std::fs::write(dir.path().join(".github/workflows/ci.yml"), "name: ci\n").unwrap();
        git(dir.path(), &["add", "."]);
        let committed = StdCommand::new("git")
            .args(["commit", "-q", "-m", "init"])
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(committed.status.success());
        git(
            dir.path(),
            &["remote", "add", "origin", "git@github.com:o/n.git"],
        );
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/feat/x", "HEAD"],
        );
        git(
            dir.path(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        dir
    }

    fn commit_all(dir: &Path, message: &str) {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
    }

    fn rev_parse(dir: &Path, rev: &str) -> String {
        let out = StdCommand::new("git")
            .args(["rev-parse", rev])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn fixture_head_sha() -> String {
        static SHA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        SHA.get_or_init(|| rev_parse(fixture().path(), "HEAD"))
            .clone()
    }

    fn run(id: u64, status: &str, conclusion: Option<&str>) -> WorkflowRun {
        WorkflowRun {
            id,
            name: Some("ci".to_string()),
            status: status.to_string(),
            conclusion: conclusion.map(str::to_string),
            html_url: format!("https://example.invalid/runs/{id}"),
            head_branch: Some("feat/x".to_string()),
            path: Some(".github/workflows/ci.yml".to_string()),
            head_sha: Some(fixture_head_sha()),
            run_started_at: None,
            updated_at: None,
        }
    }

    fn allow_ci_yml() -> CiPolicy {
        toml::from_str(
            "[workflows.\"ci.yml\"]\n\n[workflows.\"ci.yml\".inputs.k]\ntype = \"string\"\n",
        )
        .unwrap()
    }

    fn write_template(dir: &Path, ci_section: &str) {
        std::fs::create_dir_all(dir.join(".nanna")).unwrap();
        let base = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"r.invalid\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"instant\"\n";
        std::fs::write(
            dir.join(".nanna/deploy.toml"),
            format!("{base}{ci_section}"),
        )
        .unwrap();
        commit_all(dir, "deploy template");
        git(dir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(dir, &["update-ref", "refs/remotes/origin/feat/x", "HEAD"]);
    }

    fn unlimited_accountant() -> Arc<CostAccountant> {
        Arc::new(CostAccountant::new(
            Arc::new(InMemoryBudgetStore::new()),
            BudgetConfig::UNLIMITED,
        ))
    }

    #[tokio::test]
    async fn dispatch_of_a_non_allowlisted_workflow_is_rejected_before_any_effect_or_charge() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let accountant = unlimited_accountant();
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        )
        .with_budget(Arc::clone(&accountant), "id", "t1");
        let err = tool
            .execute(json!({"workflow": "release.yml"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
        assert!(!mock
            .calls()
            .iter()
            .any(|c| c.contains("dispatch_workflow") || c.contains("list_workflow_runs")));
        assert_eq!(accountant.task_summary("t1").ci.count, 0);
    }

    #[tokio::test]
    async fn dispatch_inputs_are_validated_against_the_workflow_schema() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        for inputs in [json!({"unknown": "x"}), json!({"k": 5})] {
            let err = tool
                .execute(json!({"workflow": "ci.yml", "inputs": inputs}))
                .await
                .unwrap_err();
            assert!(matches!(err, ToolError::InvalidArguments { .. }));
        }
        assert!(!mock.calls().iter().any(|c| c.contains("dispatch_workflow")));
    }

    #[tokio::test]
    async fn a_tool_with_no_policy_denies_every_dispatch() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let tool = CiTriggerTool::new(dir.path().to_path_buf(), mock);
        let err = tool
            .execute(json!({"workflow": "ci.yml"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn the_allowlist_comes_from_the_deploy_template_and_not_from_agent_arguments() {
        let dir = fixture();
        write_template(dir.path(), "[ci.workflows.\"ci.yml\"]\n");
        let mock = Arc::new(MockGithubActions {
            list_result_before_dispatch: Some(vec![]),
            list_result: vec![run(1, "queued", None)],
            ..Default::default()
        });
        let tool = CiTriggerTool::from_repo_template(
            dir.path().to_path_buf(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        )
        .with_correlation_policy(Duration::from_millis(50), Duration::from_millis(5));
        tool.execute(json!({"workflow": "ci.yml"})).await.unwrap();
        let err = tool
            .execute(json!({
                "workflow": "evil.yml",
                "allow": ["evil.yml"],
                "allowlist": {"evil.yml": {}},
                "policy": {"workflows": {"evil.yml": {}}}
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn a_template_whose_allowlist_can_reach_production_denies_every_workflow() {
        let dir = fixture();
        write_template(
            dir.path(),
            "[ci.workflows.\"ci.yml\"]\n[ci.workflows.\"deploy.yml\".inputs.env]\ntype = \"string\"\nallowed = [\"production\"]\n",
        );
        let mock = Arc::new(MockGithubActions::default());
        let tool = CiTriggerTool::from_repo_template(
            dir.path().to_path_buf(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let err = tool.execute(json!({"workflow": "ci.yml"})).await;
        assert!(matches!(err, Err(ToolError::InvalidArguments { .. })));
        assert!(mock.calls().is_empty(), "{:?}", mock.calls());
    }

    #[tokio::test]
    async fn a_missing_or_broken_deploy_template_denies_every_workflow() {
        let dir = fixture();
        let mock: Arc<dyn GithubActionsClient> = Arc::new(MockGithubActions::default());
        let tool = CiTriggerTool::from_repo_template(dir.path().to_path_buf(), Arc::clone(&mock));
        assert!(tool.execute(json!({"workflow": "ci.yml"})).await.is_err());
        std::fs::create_dir_all(dir.path().join(".nanna")).unwrap();
        std::fs::write(dir.path().join(".nanna/deploy.toml"), "not toml [").unwrap();
        let tool = CiTriggerTool::from_repo_template(dir.path().to_path_buf(), mock);
        assert!(tool.execute(json!({"workflow": "ci.yml"})).await.is_err());
    }

    #[tokio::test]
    async fn a_dispatched_run_is_remembered_as_pending_for_later_settlement() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            list_result_before_dispatch: Some(vec![]),
            list_result: vec![run(1, "queued", None)],
            ..Default::default()
        });
        let accountant = unlimited_accountant();
        let tool = trigger_tool(dir.path(), mock).with_budget(Arc::clone(&accountant), "id", "t1");
        tool.execute(json!({"workflow": "ci.yml"})).await.unwrap();
        assert_eq!(accountant.pending_runs("o/n"), vec![1]);
    }

    fn finished_run(id: u64, minutes: i64) -> WorkflowRun {
        let mut finished = run(id, "completed", Some("success"));
        finished.run_started_at = Some(chrono::Utc::now());
        finished.updated_at = Some(chrono::Utc::now() + chrono::Duration::minutes(minutes));
        finished
    }

    #[tokio::test]
    async fn minutes_of_a_finished_dispatch_are_charged_on_the_next_trigger_without_any_status_poll(
    ) {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(finished_run(1, 9));
        mock.set_run(run(2, "completed", Some("failure")));
        let accountant = unlimited_accountant();
        accountant.note_run("o/n", 1);
        let tool = trigger_tool(dir.path(), mock).with_budget(Arc::clone(&accountant), "id", "t1");
        assert_eq!(accountant.task_summary("t1").ci.minutes, 0.0);
        tool.execute(json!({"run_id": 2})).await.unwrap();
        assert_eq!(accountant.task_summary("t1").ci.minutes, 9.0);
        tool.execute(json!({"run_id": 2})).await.unwrap();
        assert_eq!(accountant.task_summary("t1").ci.minutes, 9.0);
        assert!(accountant.pending_runs("o/n").is_empty());
    }

    #[tokio::test]
    async fn an_unfinished_pending_run_stays_pending_across_triggers() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(run(1, "in_progress", None));
        mock.set_run(run(2, "completed", Some("failure")));
        let accountant = unlimited_accountant();
        accountant.note_run("o/n", 1);
        let tool = trigger_tool(dir.path(), mock).with_budget(Arc::clone(&accountant), "id", "t1");
        tool.execute(json!({"run_id": 2})).await.unwrap();
        assert_eq!(accountant.pending_runs("o/n"), vec![1]);
        assert_eq!(accountant.task_summary("t1").ci.minutes, 0.0);
    }

    #[tokio::test]
    async fn a_status_poll_and_trigger_settlement_never_double_charge_a_run() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(finished_run(1, 5));
        mock.set_run(run(2, "completed", Some("failure")));
        let accountant = unlimited_accountant();
        accountant.note_run("o/n", 1);
        let client = Arc::clone(&mock) as Arc<dyn GithubActionsClient>;
        let trigger = trigger_tool(dir.path(), Arc::clone(&client)).with_budget(
            Arc::clone(&accountant),
            "id",
            "t1",
        );
        let status = CiStatusTool::new(dir.path().to_path_buf(), client).with_budget(
            Arc::clone(&accountant),
            "id",
            "t1",
        );
        status.execute(json!({"run_id": 1})).await.unwrap();
        trigger.execute(json!({"run_id": 2})).await.unwrap();
        assert_eq!(accountant.task_summary("t1").ci.minutes, 5.0);
    }

    #[tokio::test]
    async fn a_new_task_on_the_same_repo_cannot_bypass_the_day_ceiling() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(run(1, "completed", Some("failure")));
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_count_per_day: Some(2),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let accountant = Arc::new(CostAccountant::new(
            Arc::new(InMemoryBudgetStore::new()),
            config,
        ));
        let client = Arc::clone(&mock) as Arc<dyn GithubActionsClient>;
        for (identity, task) in [("a", "t1"), ("b", "t2")] {
            trigger_tool(dir.path(), Arc::clone(&client))
                .with_budget(Arc::clone(&accountant), identity, task)
                .execute(json!({"run_id": 1}))
                .await
                .unwrap();
        }
        let err = trigger_tool(dir.path(), client)
            .with_budget(Arc::clone(&accountant), "c", "t3")
            .execute(json!({"run_id": 1}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::BudgetExceeded(_)));
    }

    fn trigger_tool(dir: &Path, client: Arc<dyn GithubActionsClient>) -> CiTriggerTool {
        CiTriggerTool::new(dir.to_path_buf(), client)
            .with_policy(allow_ci_yml())
            .with_correlation_policy(Duration::from_millis(50), Duration::from_millis(5))
    }

    #[tokio::test]
    async fn dispatch_correlates_the_newest_matching_run() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            list_result_before_dispatch: Some(vec![]),
            list_result: vec![run(1, "queued", None), run(2, "queued", None)],
            ..Default::default()
        });
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let result = tool
            .execute(json!({"workflow": "ci.yml", "inputs": {"k": "v"}}))
            .await
            .unwrap();
        assert_eq!(result["mode"], "dispatch");
        assert_eq!(result["run_id"], 2);
        assert_eq!(result["status"], "queued");
        let calls = mock.calls();
        assert!(calls
            .iter()
            .any(|c| c.contains("dispatch_workflow o/n ci.yml@feat/x")));
        assert!(calls.iter().any(|c| c.contains("list_workflow_runs")));
    }

    /// A re-dispatch of the same workflow/branch -- the normal shape of the
    /// middle loop's fix-and-retry cycle -- must not resolve to the
    /// *previous* dispatch's run just because it is still the only one
    /// GitHub's eventually-consistent run list happens to show yet.
    #[tokio::test]
    async fn dispatch_does_not_correlate_to_a_run_that_predates_this_dispatch() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            list_result: vec![run(5, "completed", Some("failure"))],
            ..Default::default()
        });
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let err = tool
            .execute(json!({"workflow": "ci.yml"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    /// Once the fix above snapshots the pre-dispatch baseline, a run that was
    /// already visible before the dispatch is correctly excluded while a
    /// genuinely new run (a higher id, appearing only once the list is
    /// re-polled after the dispatch) is still picked up.
    #[tokio::test]
    async fn dispatch_correlates_to_the_new_run_even_when_an_older_run_is_still_listed() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            list_result_before_dispatch: Some(vec![run(5, "completed", Some("failure"))]),
            list_result: vec![run(5, "completed", Some("failure")), run(9, "queued", None)],
            ..Default::default()
        });
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let result = tool.execute(json!({"workflow": "ci.yml"})).await.unwrap();
        assert_eq!(result["run_id"], 9);
    }

    #[tokio::test]
    async fn dispatch_rejects_a_non_object_inputs_argument() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let tool = trigger_tool(dir.path(), mock);
        let err = tool
            .execute(json!({"workflow": "ci.yml", "inputs": "not an object"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn trigger_rejects_a_non_integer_run_id() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let tool = trigger_tool(dir.path(), mock);
        let err = tool
            .execute(json!({"run_id": "not a number"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn logs_rejects_a_non_integer_max_chars() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            jobs: HashMap::from([(9, vec![job(2, "test", Some("failure"))])]),
            logs: HashMap::from([(2, "log".to_string())]),
            ..Default::default()
        });
        let tool = CiLogsTool::new(dir.path().to_path_buf(), mock);
        let err = tool
            .execute(json!({"run_id": 9, "max_chars": "not a number"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn dispatch_requires_a_workflow_argument() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let tool = trigger_tool(dir.path(), mock);
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn dispatch_rejects_a_workflow_argument_that_would_escape_the_repo_path_segment() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        for bad in [
            "../../../../other-owner/other-repo/actions/workflows/ci.yml",
            "..",
            ".",
            "ci.yml/dispatches",
            "",
        ] {
            let err = tool.execute(json!({"workflow": bad})).await.unwrap_err();
            assert!(
                matches!(err, ToolError::InvalidArguments { .. }),
                "workflow={bad:?} produced {err:?}"
            );
        }
        assert!(mock.calls().is_empty(), "no request should have been sent");
    }

    #[test]
    fn valid_workflow_ref_accepts_ordinary_names_and_rejects_path_segments() {
        assert!(valid_workflow_ref("ci.yml"));
        assert!(valid_workflow_ref("deploy-staging.yaml"));
        assert!(valid_workflow_ref("123456"));
        assert!(!valid_workflow_ref(""));
        assert!(!valid_workflow_ref("."));
        assert!(!valid_workflow_ref(".."));
        assert!(!valid_workflow_ref("a/b"));
        assert!(!valid_workflow_ref("../../etc/passwd"));
    }

    #[tokio::test]
    async fn dispatch_gives_up_after_the_correlation_budget_elapses() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let tool = trigger_tool(dir.path(), mock);
        let err = tool
            .execute(json!({"workflow": "ci.yml"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    /// `fail_status` fails every mock call, including the pre-dispatch
    /// baseline listing `max_known_run_id` reads before `dispatch_workflow`
    /// is ever attempted; this only proves that *some* upstream call's
    /// error surfaces correctly.
    #[tokio::test]
    async fn dispatch_surfaces_a_client_error() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            fail_status: Some(422),
            ..Default::default()
        });
        let tool = trigger_tool(dir.path(), mock);
        let err = tool
            .execute(json!({"workflow": "ci.yml"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    /// Isolates a failure in `dispatch_workflow` itself, after a successful
    /// baseline listing, and confirms the call was already charged: the
    /// charge happens before the dispatch is attempted, so a dispatch that
    /// reaches GitHub and then fails still consumes budget.
    #[tokio::test]
    async fn dispatch_surfaces_a_failure_from_the_dispatch_call_itself_and_still_charges() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            fail_dispatch_status: Some(422),
            ..Default::default()
        });
        let accountant = Arc::new(CostAccountant::new(
            Arc::new(InMemoryBudgetStore::new()),
            BudgetConfig::UNLIMITED,
        ));
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        )
        .with_budget(Arc::clone(&accountant), "id", "t1");
        let err = tool
            .execute(json!({"workflow": "ci.yml"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(mock
            .calls()
            .iter()
            .any(|c| c.contains("dispatch_workflow o/n ci.yml@feat/x")));
        assert_eq!(accountant.task_summary("t1").ci.count, 1);
    }

    #[tokio::test]
    async fn rerun_reuses_the_given_run_id() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(run(9, "completed", Some("failure")));
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let result = tool.execute(json!({"run_id": 9})).await.unwrap();
        assert_eq!(result["mode"], "rerun");
        assert_eq!(result["run_id"], 9);
        assert!(mock
            .calls()
            .iter()
            .any(|c| c.contains("rerun_workflow o/n#9")));
        assert!(!mock.calls().iter().any(|c| c.contains("dispatch_workflow")));
    }

    fn dispatch_calls(mock: &MockGithubActions) -> Vec<String> {
        mock.calls()
            .into_iter()
            .filter(|c| c.starts_with("dispatch_workflow"))
            .collect()
    }

    fn modified_surface_refusal(err: ToolError) -> String {
        match err {
            ToolError::InvalidArguments { message } => message,
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_runs_the_allowlisted_workflow_on_the_task_branch_when_ci_files_match_base() {
        let dir = fixture();
        std::fs::write(dir.path().join("src.rs"), "fn main() {}").unwrap();
        commit_all(dir.path(), "feature work");
        let mock = Arc::new(MockGithubActions {
            list_result_before_dispatch: Some(vec![]),
            list_result: vec![run(1, "queued", None)],
            ..Default::default()
        });
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        tool.execute(json!({"workflow": "ci.yml"})).await.unwrap();
        assert_eq!(
            dispatch_calls(&mock),
            vec!["dispatch_workflow o/n ci.yml@feat/x {}".to_string()]
        );
    }

    #[tokio::test]
    async fn dispatch_is_refused_when_the_task_branch_edits_the_allowlisted_workflow() {
        let dir = fixture();
        std::fs::write(
            dir.path().join(".github/workflows/ci.yml"),
            "name: ci\non: workflow_dispatch\njobs: {}\n",
        )
        .unwrap();
        commit_all(dir.path(), "edit ci");
        let mock = Arc::new(MockGithubActions::default());
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let message = modified_surface_refusal(
            tool.execute(json!({"workflow": "ci.yml"}))
                .await
                .unwrap_err(),
        );
        assert!(message.contains(".github"), "{message}");
        assert!(mock.calls().is_empty(), "{:?}", mock.calls());
    }

    #[tokio::test]
    async fn dispatch_is_refused_when_the_task_branch_adds_or_edits_any_other_ci_file() {
        for (path, body) in [
            (".github/workflows/extra.yml", "name: extra\n"),
            (".github/actions/setup/action.yml", "name: setup\n"),
        ] {
            let dir = fixture();
            let target = dir.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, body).unwrap();
            commit_all(dir.path(), "touch ci surface");
            let mock = Arc::new(MockGithubActions::default());
            let tool = trigger_tool(
                dir.path(),
                Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
            );
            let err = tool.execute(json!({"workflow": "ci.yml"})).await;
            assert!(
                matches!(err, Err(ToolError::InvalidArguments { .. })),
                "{path}"
            );
            assert!(mock.calls().is_empty(), "{path}: {:?}", mock.calls());
        }
    }

    #[tokio::test]
    async fn dispatch_is_refused_when_the_pushed_branch_tip_differs_from_a_clean_local_head() {
        let dir = fixture();
        let clean = rev_parse(dir.path(), "HEAD");
        std::fs::write(
            dir.path().join(".github/workflows/ci.yml"),
            "name: pushed-evil\n",
        )
        .unwrap();
        commit_all(dir.path(), "evil");
        let evil = rev_parse(dir.path(), "HEAD");
        git(
            dir.path(),
            &["update-ref", "refs/remotes/origin/feat/x", &evil],
        );
        git(dir.path(), &["reset", "-q", "--hard", &clean]);
        let mock = Arc::new(MockGithubActions::default());
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let err = tool.execute(json!({"workflow": "ci.yml"})).await;
        assert!(matches!(err, Err(ToolError::InvalidArguments { .. })));
        assert!(mock.calls().is_empty(), "{:?}", mock.calls());
    }

    #[tokio::test]
    async fn dispatch_is_refused_when_the_branch_has_never_been_pushed() {
        let dir = fixture();
        git(
            dir.path(),
            &["update-ref", "-d", "refs/remotes/origin/feat/x"],
        );
        let mock = Arc::new(MockGithubActions::default());
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let message = modified_surface_refusal(
            tool.execute(json!({"workflow": "ci.yml"}))
                .await
                .unwrap_err(),
        );
        assert!(message.contains("push the branch"), "{message}");
        assert!(mock.calls().is_empty(), "{:?}", mock.calls());
    }

    #[tokio::test]
    async fn dispatch_is_refused_when_no_base_branch_can_be_resolved() {
        let dir = fixture();
        git(
            dir.path(),
            &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
        );
        git(
            dir.path(),
            &["update-ref", "-d", "refs/remotes/origin/main"],
        );
        let mock = Arc::new(MockGithubActions::default());
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let err = tool.execute(json!({"workflow": "ci.yml"})).await;
        assert!(matches!(err, Err(ToolError::InvalidArguments { .. })));
        assert!(mock.calls().is_empty(), "{:?}", mock.calls());
    }

    #[tokio::test]
    async fn dispatch_is_refused_when_the_allowlisted_workflow_is_absent_from_base() {
        let dir = fixture();
        let policy: CiPolicy = toml::from_str("[workflows.\"ghost.yml\"]\n").unwrap();
        let mock = Arc::new(MockGithubActions::default());
        let tool = CiTriggerTool::new(
            dir.path().to_path_buf(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        )
        .with_policy(policy);
        let err = tool.execute(json!({"workflow": "ghost.yml"})).await;
        assert!(matches!(err, Err(ToolError::InvalidArguments { .. })));
        assert!(mock.calls().is_empty(), "{:?}", mock.calls());
    }

    #[tokio::test]
    async fn dispatch_is_refused_when_the_policy_file_in_the_worktree_differs_from_base() {
        let dir = fixture();
        write_template(dir.path(), "[ci.workflows.\"ci.yml\"]\n");
        let widened = format!(
            "{}[ci.workflows.\"evil.yml\"]\n",
            std::fs::read_to_string(dir.path().join(".nanna/deploy.toml")).unwrap()
        );
        std::fs::write(dir.path().join(".nanna/deploy.toml"), widened).unwrap();
        let mock = Arc::new(MockGithubActions::default());
        let tool = CiTriggerTool::from_repo_template(
            dir.path().to_path_buf(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let err = tool.execute(json!({"workflow": "ci.yml"})).await;
        assert!(matches!(err, Err(ToolError::InvalidArguments { .. })));
        assert!(mock.calls().is_empty(), "{:?}", mock.calls());
    }

    fn rerun_fixture(
        path: Option<&str>,
        head_sha: Option<String>,
    ) -> (TempDir, Arc<MockGithubActions>) {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let mut failed = run(9, "completed", Some("failure"));
        failed.path = path.map(str::to_string);
        if head_sha.is_some() {
            failed.head_sha = head_sha;
        }
        mock.set_run(failed);
        (dir, mock)
    }

    fn reruns(mock: &MockGithubActions) -> usize {
        mock.calls()
            .iter()
            .filter(|c| c.starts_with("rerun_workflow"))
            .count()
    }

    #[tokio::test]
    async fn rerun_of_a_workflow_outside_the_allowlist_is_refused() {
        let (dir, mock) = rerun_fixture(Some(".github/workflows/release.yml"), None);
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let err = tool.execute(json!({"run_id": 9})).await.unwrap_err();
        assert!(modified_surface_refusal(err).contains("release.yml"));
        assert_eq!(reruns(&mock), 0);
    }

    #[tokio::test]
    async fn rerun_matches_the_workflow_file_name_exactly() {
        for path in [
            None,
            Some(""),
            Some(".github/workflows/CI.yml"),
            Some(".github/workflows/ci.yml "),
            Some(".github/workflows/ci.yml."),
            Some(".github/workflows/../ci.yml"),
            Some(".github/workflows/sub/ci.yml"),
            Some("ci.yml"),
            Some("x/.github/workflows/ci.yml"),
            Some(".github/workflows/ci%2Eyml"),
            Some(".github/workflows/\u{441}i.yml"),
        ] {
            let (dir, mock) = rerun_fixture(path, None);
            let tool = trigger_tool(
                dir.path(),
                Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
            );
            let err = tool.execute(json!({"run_id": 9})).await;
            assert!(
                matches!(err, Err(ToolError::InvalidArguments { .. })),
                "{path:?}"
            );
            assert_eq!(reruns(&mock), 0, "{path:?}");
        }
    }

    #[tokio::test]
    async fn rerun_of_an_allowlisted_workflow_with_a_ref_suffix_is_accepted() {
        let (dir, mock) = rerun_fixture(Some(".github/workflows/ci.yml@refs/heads/feat/x"), None);
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        tool.execute(json!({"run_id": 9})).await.unwrap();
        assert_eq!(reruns(&mock), 1);
    }

    #[tokio::test]
    async fn rerun_is_refused_when_the_run_executed_a_modified_workflow() {
        let dir = fixture();
        std::fs::write(dir.path().join(".github/workflows/ci.yml"), "name: evil\n").unwrap();
        commit_all(dir.path(), "evil");
        let evil = rev_parse(dir.path(), "HEAD");
        git(dir.path(), &["reset", "-q", "--hard", "HEAD~1"]);
        let mock = Arc::new(MockGithubActions::default());
        let mut failed = run(9, "completed", Some("failure"));
        failed.head_sha = Some(evil);
        mock.set_run(failed);
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let err = tool.execute(json!({"run_id": 9})).await;
        assert!(matches!(err, Err(ToolError::InvalidArguments { .. })));
        assert_eq!(reruns(&mock), 0);
    }

    #[tokio::test]
    async fn rerun_is_refused_when_the_run_does_not_report_its_commit() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let mut failed = run(9, "completed", Some("failure"));
        failed.head_sha = None;
        mock.set_run(failed);
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        );
        let err = tool.execute(json!({"run_id": 9})).await;
        assert!(matches!(err, Err(ToolError::InvalidArguments { .. })));
        assert_eq!(reruns(&mock), 0);
    }

    #[tokio::test]
    async fn rerun_refuses_a_run_on_a_different_branch() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let mut other_branch = run(9, "completed", Some("failure"));
        other_branch.head_branch = Some("main".to_string());
        mock.set_run(other_branch);
        let tool = trigger_tool(dir.path(), mock);
        let err = tool.execute(json!({"run_id": 9})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn rerun_refuses_a_run_that_did_not_fail() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(run(9, "completed", Some("success")));
        let tool = trigger_tool(dir.path(), mock);
        let err = tool.execute(json!({"run_id": 9})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn trigger_with_budget_blocks_once_the_task_ceiling_is_reached() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(run(1, "completed", Some("failure")));
        let config = BudgetConfig {
            ci: BudgetLimits {
                max_count_per_task: Some(1),
                ..BudgetLimits::UNLIMITED
            },
            sandbox: BudgetLimits::UNLIMITED,
        };
        let accountant = Arc::new(CostAccountant::new(
            Arc::new(InMemoryBudgetStore::new()),
            config,
        ));
        let tool = trigger_tool(
            dir.path(),
            Arc::clone(&mock) as Arc<dyn GithubActionsClient>,
        )
        .with_budget(Arc::clone(&accountant), "id", "t1");
        tool.execute(json!({"run_id": 1})).await.unwrap();
        let err = tool.execute(json!({"run_id": 1})).await.unwrap_err();
        assert!(matches!(err, ToolError::BudgetExceeded(_)));
        assert_eq!(accountant.task_summary("t1").ci.count, 1);
    }

    #[tokio::test]
    async fn trigger_without_budget_configured_is_unmetered() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(run(1, "completed", Some("failure")));
        let tool = trigger_tool(dir.path(), mock);
        for _ in 0..5 {
            tool.execute(json!({"run_id": 1})).await.unwrap();
        }
    }

    #[tokio::test]
    async fn status_reports_a_completed_run_with_duration() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let mut completed = run(9, "completed", Some("success"));
        completed.run_started_at = Some(chrono::Utc::now());
        completed.updated_at = Some(chrono::Utc::now() + chrono::Duration::minutes(4));
        mock.set_run(completed);
        let tool = CiStatusTool::new(dir.path().to_path_buf(), mock);
        let result = tool.execute(json!({"run_id": 9})).await.unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["conclusion"], "success");
        assert_eq!(result["duration_minutes"], 4.0);
    }

    #[tokio::test]
    async fn status_with_budget_records_minutes_once_per_run_even_when_polled_repeatedly() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let mut completed = run(9, "completed", Some("success"));
        completed.run_started_at = Some(chrono::Utc::now());
        completed.updated_at = Some(chrono::Utc::now() + chrono::Duration::minutes(7));
        mock.set_run(completed);
        let accountant = Arc::new(CostAccountant::new(
            Arc::new(InMemoryBudgetStore::new()),
            BudgetConfig::UNLIMITED,
        ));
        let tool = CiStatusTool::new(dir.path().to_path_buf(), mock).with_budget(
            Arc::clone(&accountant),
            "id",
            "t1",
        );
        tool.execute(json!({"run_id": 9})).await.unwrap();
        tool.execute(json!({"run_id": 9})).await.unwrap();
        tool.execute(json!({"run_id": 9})).await.unwrap();
        assert_eq!(accountant.task_summary("t1").ci.minutes, 7.0);
    }

    #[tokio::test]
    async fn status_with_budget_does_not_record_minutes_for_an_unfinished_run() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(run(9, "in_progress", None));
        let accountant = Arc::new(CostAccountant::new(
            Arc::new(InMemoryBudgetStore::new()),
            BudgetConfig::UNLIMITED,
        ));
        let tool = CiStatusTool::new(dir.path().to_path_buf(), mock).with_budget(
            Arc::clone(&accountant),
            "id",
            "t1",
        );
        tool.execute(json!({"run_id": 9})).await.unwrap();
        assert_eq!(accountant.task_summary("t1").ci.minutes, 0.0);
    }

    #[tokio::test]
    async fn status_requires_run_id() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let tool = CiStatusTool::new(dir.path().to_path_buf(), mock);
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn status_surfaces_an_unknown_run() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        let tool = CiStatusTool::new(dir.path().to_path_buf(), mock);
        let err = tool.execute(json!({"run_id": 404})).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    fn job(id: u64, name: &str, conclusion: Option<&str>) -> WorkflowJob {
        WorkflowJob {
            id,
            name: name.to_string(),
            status: "completed".to_string(),
            conclusion: conclusion.map(str::to_string),
            steps: vec![WorkflowStep {
                name: "cargo test".to_string(),
                status: "completed".to_string(),
                conclusion: conclusion.map(str::to_string),
                number: 3,
            }],
        }
    }

    #[tokio::test]
    async fn logs_concatenates_only_failed_jobs_and_truncates_the_tail() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            jobs: HashMap::from([(
                9,
                vec![
                    job(1, "build", Some("success")),
                    job(2, "test", Some("failure")),
                ],
            )]),
            logs: HashMap::from([(2, "x".repeat(100))]),
            ..Default::default()
        });
        let tool = CiLogsTool::new(dir.path().to_path_buf(), mock);
        let result = tool
            .execute(json!({"run_id": 9, "max_chars": 10}))
            .await
            .unwrap();
        assert_eq!(result["truncated"], true);
        assert_eq!(result["logs"].as_str().unwrap().len(), 10);
        let jobs = result["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0]["name"], "test");
    }

    #[tokio::test]
    async fn logs_returns_untruncated_output_within_the_default_budget() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            jobs: HashMap::from([(9, vec![job(2, "test", Some("failure"))])]),
            logs: HashMap::from([(2, "short log\n".to_string())]),
            ..Default::default()
        });
        let tool = CiLogsTool::new(dir.path().to_path_buf(), mock);
        let result = tool.execute(json!({"run_id": 9})).await.unwrap();
        assert_eq!(result["truncated"], false);
        assert!(result["logs"].as_str().unwrap().contains("short log"));
    }

    #[tokio::test]
    async fn logs_rejects_a_max_chars_above_zero_but_clamps_above_the_ceiling() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            jobs: HashMap::from([(9, vec![job(2, "test", Some("failure"))])]),
            logs: HashMap::from([(2, "y".repeat(5))]),
            ..Default::default()
        });
        let tool = CiLogsTool::new(dir.path().to_path_buf(), mock);
        let result = tool
            .execute(json!({"run_id": 9, "max_chars": 999_999_999u64}))
            .await
            .unwrap();
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn logs_reports_no_failed_jobs_as_empty() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            jobs: HashMap::from([(9, vec![job(1, "build", Some("success"))])]),
            ..Default::default()
        });
        let tool = CiLogsTool::new(dir.path().to_path_buf(), mock);
        let result = tool.execute(json!({"run_id": 9})).await.unwrap();
        assert_eq!(result["jobs"].as_array().unwrap().len(), 0);
        assert_eq!(result["logs"], "");
        assert_eq!(result["truncated"], false);
    }

    #[tokio::test]
    async fn logs_surfaces_a_client_error() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
            fail_status: Some(500),
            ..Default::default()
        });
        let tool = CiLogsTool::new(dir.path().to_path_buf(), mock);
        let err = tool.execute(json!({"run_id": 9})).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
    }

    #[test]
    fn tail_chars_cuts_multibyte_text_on_a_char_boundary() {
        let text = "é".repeat(20);
        let (tail, truncated) = tail_chars(&text, 5);
        assert!(truncated);
        assert_eq!(tail.chars().count(), 5);
        assert_eq!(tail, "é".repeat(5));
        let (whole, truncated) = tail_chars("short", 100);
        assert_eq!(whole, "short");
        assert!(!truncated);
    }

    #[test]
    fn definitions_declare_the_documented_shape() {
        let dir = fixture();
        let mock: Arc<dyn GithubActionsClient> = Arc::new(MockGithubActions::default());
        let trigger = CiTriggerTool::new(dir.path().to_path_buf(), Arc::clone(&mock));
        assert_eq!(trigger.name(), "ci_trigger");
        assert_eq!(trigger.effect_class(), EffectClass::Ci);
        assert_eq!(trigger.definition().function.name, "ci_trigger");
        let status = CiStatusTool::new(dir.path().to_path_buf(), Arc::clone(&mock));
        assert_eq!(status.effect_class(), EffectClass::Repository);
        assert_eq!(
            status.definition().function.parameters.required,
            Some(vec!["run_id".to_string()])
        );
        let logs = CiLogsTool::new(dir.path().to_path_buf(), mock);
        assert_eq!(logs.effect_class(), EffectClass::Repository);
        assert_eq!(logs.name(), "ci_logs");
    }

    #[test]
    fn register_adds_all_three_tools() {
        let dir = fixture();
        let mut registry = ToolRegistry::new();
        register(&mut registry, dir.path());
        let mut names = registry.list_tools();
        names.sort_unstable();
        assert_eq!(names, vec!["ci_logs", "ci_status", "ci_trigger"]);
    }
}
