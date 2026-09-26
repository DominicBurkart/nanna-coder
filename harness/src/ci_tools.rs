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
//! existing trait, its doctest and [`crate::backlog::test_support::MockGithub`]
//! are untouched). Credentials and repository resolution follow
//! [`crate::pr_tools`]'s existing pattern: a `GITHUB_TOKEN` read from the
//! harness process's environment, and the repository always resolved from
//! the worktree's own `origin` remote rather than accepted as a tool
//! argument, so a tool call cannot aim the harness's token at another
//! repository.

use crate::backlog::{BacklogError, GithubActionsClient, WorkflowRun};
use crate::budget::{BudgetClass, CostAccountant};
use crate::effects::EffectClass;
use crate::pr_tools::{
    current_branch, map_backlog_error, required_str, required_u64, resolve_repo,
};
use crate::tools::{Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use chrono::Utc;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
}

impl CiTriggerTool {
    pub fn new(workspace_root: PathBuf, client: Arc<dyn GithubActionsClient>) -> Self {
        Self {
            workspace_root,
            client,
            correlation_budget: DEFAULT_CORRELATION_BUDGET,
            correlation_poll: DEFAULT_CORRELATION_POLL,
            budget: None,
        }
    }

    /// Override how long and how often [`Self::correlate_run`] polls for
    /// the dispatched run to appear. Tests use a near-zero budget so a
    /// "never appeared" case runs instantly instead of for real seconds.
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

    /// `workflow_dispatch` hands back `204 No Content` with no run id, so
    /// the run is found by listing the workflow's runs filtered to `branch`
    /// and [`DISPATCH_EVENT`] and taking the newest one, retrying until one
    /// appears or the budget elapses.
    async fn correlate_run(
        &self,
        repo: &str,
        workflow: &str,
        branch: &str,
    ) -> Result<WorkflowRun, BacklogError> {
        let start = Instant::now();
        loop {
            let mut runs = self
                .client
                .list_workflow_runs(repo, workflow, branch, DISPATCH_EVENT)
                .await?;
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
            "Dispatch a GitHub Actions workflow (workflow_dispatch) against the task's own branch, or re-run a failed run's failed jobs when 'run_id' is given. Returns the run id either way. Result: { mode: \"dispatch\"|\"rerun\", run_id, html_url, status }.",
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
        if let Some(ctx) = &self.budget {
            ctx.accountant
                .charge_count(
                    &ctx.identity,
                    &ctx.task_id,
                    &repo,
                    BudgetClass::Ci,
                    Utc::now(),
                )
                .await
                .map_err(ToolError::BudgetExceeded)?;
        }
        if let Some(run_id) = optional_u64(&args, "run_id")? {
            self.client
                .rerun_workflow(&repo, run_id)
                .await
                .map_err(map_backlog_error)?;
            let run = self
                .client
                .get_workflow_run(&repo, run_id)
                .await
                .map_err(map_backlog_error)?;
            return Ok(json!({
                "mode": "rerun",
                "run_id": run.id,
                "html_url": run.html_url,
                "status": run.status,
            }));
        }
        let workflow = required_str(&args, "workflow")?;
        let inputs = optional_object(&args, "inputs")?;
        let branch = current_branch(&self.workspace_root)?;
        self.client
            .dispatch_workflow(&repo, workflow, &branch, inputs)
            .await
            .map_err(map_backlog_error)?;
        let run = self
            .correlate_run(&repo, workflow, &branch)
            .await
            .map_err(map_backlog_error)?;
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
    /// Run ids whose minutes have already been charged, so repeated polling
    /// of the same completed run never double-counts.
    minutes_charged: Mutex<HashSet<u64>>,
}

impl CiStatusTool {
    pub fn new(workspace_root: PathBuf, client: Arc<dyn GithubActionsClient>) -> Self {
        Self {
            workspace_root,
            client,
            budget: None,
            minutes_charged: Mutex::new(HashSet::new()),
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
        let Some(ctx) = &self.budget else {
            return;
        };
        let Some(minutes) = run.duration_minutes() else {
            return;
        };
        {
            let mut charged = self.minutes_charged.lock().expect("charged set poisoned");
            if !charged.insert(run.id) {
                return;
            }
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

/// Registers `ci_trigger`, `ci_status` and `ci_logs` against a fresh
/// [`crate::backlog::ReqwestGithubClient`] built from `GITHUB_TOKEN`,
/// mirroring [`crate::pr_tools::register`].
pub fn register(registry: &mut ToolRegistry, workspace_root: &Path) {
    let token = std::env::var("GITHUB_TOKEN").ok();
    let client: Arc<dyn GithubActionsClient> =
        Arc::new(crate::backlog::ReqwestGithubClient::github(token));
    registry.register(Box::new(CiTriggerTool::new(
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
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "init"]);
        git(
            dir.path(),
            &["remote", "add", "origin", "git@github.com:o/n.git"],
        );
        dir
    }

    fn run(id: u64, status: &str, conclusion: Option<&str>) -> WorkflowRun {
        WorkflowRun {
            id,
            name: Some("ci".to_string()),
            status: status.to_string(),
            conclusion: conclusion.map(str::to_string),
            html_url: format!("https://example.invalid/runs/{id}"),
            run_started_at: None,
            updated_at: None,
        }
    }

    fn trigger_tool(dir: &Path, client: Arc<dyn GithubActionsClient>) -> CiTriggerTool {
        CiTriggerTool::new(dir.to_path_buf(), client)
            .with_correlation_policy(Duration::from_millis(50), Duration::from_millis(5))
    }

    #[tokio::test]
    async fn dispatch_correlates_the_newest_matching_run() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions {
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

    #[tokio::test]
    async fn rerun_reuses_the_given_run_id() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(run(9, "queued", None));
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

    #[tokio::test]
    async fn trigger_with_budget_blocks_once_the_task_ceiling_is_reached() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(run(1, "queued", None));
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
        // A rerun that never reaches the client because the budget already
        // blocked it must not have been recorded twice.
        assert_eq!(accountant.task_summary("t1").ci.count, 1);
    }

    #[tokio::test]
    async fn trigger_without_budget_configured_is_unmetered() {
        let dir = fixture();
        let mock = Arc::new(MockGithubActions::default());
        mock.set_run(run(1, "queued", None));
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
