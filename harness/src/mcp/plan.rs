use crate::auditor::{Refused, SpawnGate, TaskSummary};
use crate::identity::IdentityError;
use crate::plan_dispatch::{
    ChildOutcome, DispatchTarget, PlanRun, PlanRunError, PlanSummary, RunEnd,
};
use crate::planner::{
    audit_plan, NodeFailure, PlanAuditError, PlanError, PlanRefusal, Planner, Spawn,
};
use crate::task::TaskManager;
use chrono::{DateTime, Utc};
use model::ModelProvider;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::watch;

pub const PLAN_ID_PREFIX: &str = "plan-";
pub const DEFAULT_BUDGET_SECS: u64 = 600;
pub const MAX_BUDGET_SECS: u64 = 1_800;
pub const MAX_CONCURRENT_PLANS: usize = 8;
pub const MAX_RETAINED_PLANS: usize = 32;
pub const MAX_RESULT_BYTES: usize = 256 * 1024;
pub const MAX_DESCRIPTION_CHARS: usize = 8_000;
pub const MAX_ITERATIONS_CAP: usize = 1_000;
const MAX_REPORTED_FILES: usize = 200;
const UNTRUSTED_CLIP: usize = 200;
const POLL_EVERY: Duration = Duration::from_millis(500);
const WAIT_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Working,
    Completed,
    Failed,
    Cancelled,
}

impl PlanStatus {
    pub fn wire(self) -> &'static str {
        match self {
            PlanStatus::Working => "working",
            PlanStatus::Completed => "completed",
            PlanStatus::Failed => "failed",
            PlanStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistryError {
    #[error("{0} plans are already running; try again when one finishes")]
    TooManyRunning(usize),
    #[error("plan `{0}` was not found; it may have been evicted or the server restarted")]
    NotFound(String),
    #[error("plan `{0}` has already finished")]
    AlreadyFinished(String),
}

#[derive(Debug, Clone)]
pub struct PlanView {
    pub status: PlanStatus,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
}

#[derive(Debug)]
enum Slot {
    Running {
        cancel: watch::Sender<bool>,
        cancel_requested: bool,
    },
    Finished {
        status: PlanStatus,
        result: Value,
    },
}

#[derive(Debug)]
struct Entry {
    slot: Slot,
    created: DateTime<Utc>,
    updated: DateTime<Utc>,
}

#[derive(Debug, Default)]
struct Inner {
    entries: BTreeMap<String, Entry>,
    finished: VecDeque<String>,
}

/// ```
/// use harness::mcp::plan::{PlanRegistry, PlanStatus};
///
/// let registry = PlanRegistry::default();
/// let id = PlanRegistry::new_id();
/// let cancel = registry.register(&id).unwrap();
/// assert_eq!(registry.view(&id).unwrap().status, PlanStatus::Working);
/// registry.cancel(&id).unwrap();
/// assert!(*cancel.borrow());
/// assert_eq!(registry.view(&id).unwrap().status, PlanStatus::Cancelled);
/// registry.finish(&id, PlanStatus::Cancelled, serde_json::json!({"end": "cancelled"}));
/// assert_eq!(registry.view(&id).unwrap().status, PlanStatus::Cancelled);
/// assert!(registry.view("plan-unknown").is_err());
/// ```
#[derive(Debug, Default)]
pub struct PlanRegistry {
    inner: Mutex<Inner>,
}

fn plain(text: &str) -> String {
    let keep = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    text.chars()
        .take(80)
        .map(|c| if keep(c) { c } else { '?' })
        .collect()
}

fn capped(result: Value) -> Value {
    let bytes = result.to_string().len();
    if bytes <= MAX_RESULT_BYTES {
        return result;
    }
    json!({ "truncated": true, "bytes": bytes, "summary": result["summary"].clone() })
}

impl PlanRegistry {
    pub fn new_id() -> String {
        format!("{PLAN_ID_PREFIX}{}", uuid::Uuid::new_v4())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn running(inner: &Inner) -> usize {
        let running = |entry: &&Entry| matches!(entry.slot, Slot::Running { .. });
        inner.entries.values().filter(running).count()
    }

    pub fn ensure_room(&self) -> Result<(), RegistryError> {
        let running = Self::running(&self.lock());
        if running >= MAX_CONCURRENT_PLANS {
            return Err(RegistryError::TooManyRunning(running));
        }
        Ok(())
    }

    pub fn register(&self, id: &str) -> Result<watch::Receiver<bool>, RegistryError> {
        let mut inner = self.lock();
        let running = Self::running(&inner);
        if running >= MAX_CONCURRENT_PLANS {
            return Err(RegistryError::TooManyRunning(running));
        }
        let (cancel, receiver) = watch::channel(false);
        let now = Utc::now();
        let slot = Slot::Running {
            cancel,
            cancel_requested: false,
        };
        let entry = Entry {
            slot,
            created: now,
            updated: now,
        };
        inner.entries.insert(id.to_string(), entry);
        Ok(receiver)
    }

    pub fn finish(&self, id: &str, status: PlanStatus, result: Value) {
        let mut inner = self.lock();
        let Some(entry) = inner.entries.get_mut(id) else {
            return;
        };
        if matches!(entry.slot, Slot::Finished { .. }) {
            return;
        }
        entry.slot = Slot::Finished {
            status,
            result: capped(result),
        };
        entry.updated = Utc::now();
        inner.finished.push_back(id.to_string());
        while inner.finished.len() > MAX_RETAINED_PLANS {
            if let Some(oldest) = inner.finished.pop_front() {
                inner.entries.remove(&oldest);
            }
        }
    }

    pub fn view(&self, id: &str) -> Result<PlanView, RegistryError> {
        let inner = self.lock();
        let entry = inner
            .entries
            .get(id)
            .ok_or_else(|| RegistryError::NotFound(plain(id)))?;
        let status = match &entry.slot {
            Slot::Running {
                cancel_requested: true,
                ..
            } => PlanStatus::Cancelled,
            Slot::Running { .. } => PlanStatus::Working,
            Slot::Finished { status, .. } => *status,
        };
        Ok(PlanView {
            status,
            created: entry.created,
            updated: entry.updated,
        })
    }

    pub fn views(&self) -> Vec<(String, PlanView)> {
        let ids: Vec<String> = self.lock().entries.keys().cloned().collect();
        let view = |id: String| self.view(&id).ok().map(|view| (id, view));
        ids.into_iter().filter_map(view).collect()
    }

    pub fn cancel(&self, id: &str) -> Result<(), RegistryError> {
        let mut inner = self.lock();
        let entry = inner
            .entries
            .get_mut(id)
            .ok_or_else(|| RegistryError::NotFound(plain(id)))?;
        match &mut entry.slot {
            Slot::Running {
                cancel,
                cancel_requested,
            } => {
                *cancel_requested = true;
                let _ = cancel.send(true);
                Ok(())
            }
            Slot::Finished { .. } => Err(RegistryError::AlreadyFinished(plain(id))),
        }
    }

    pub async fn wait_finished(&self, id: &str) -> Result<(PlanStatus, Value), RegistryError> {
        loop {
            {
                let inner = self.lock();
                let entry = inner
                    .entries
                    .get(id)
                    .ok_or_else(|| RegistryError::NotFound(plain(id)))?;
                if let Slot::Finished { status, result } = &entry.slot {
                    return Ok((*status, result.clone()));
                }
            }
            tokio::time::sleep(WAIT_EVERY).await;
        }
    }

    pub fn running_count(&self) -> usize {
        Self::running(&self.lock())
    }
}

struct RunGuard {
    registry: Arc<PlanRegistry>,
    id: String,
    done: bool,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if !self.done {
            let aborted = json!({ "end": "aborted", "gave_up": true });
            self.registry.finish(&self.id, PlanStatus::Failed, aborted);
        }
    }
}

pub fn is_plan_id(id: &str) -> bool {
    id.starts_with(PLAN_ID_PREFIX)
}

/// ```
/// use harness::mcp::plan::{view_to_wire, PlanRegistry};
///
/// let registry = PlanRegistry::default();
/// let id = PlanRegistry::new_id();
/// let _cancel = registry.register(&id).unwrap();
/// let wire = view_to_wire(&id, &registry.view(&id).unwrap());
/// assert_eq!(wire["status"], "working");
/// assert_eq!(wire["taskId"], id);
/// ```
pub fn view_to_wire(id: &str, view: &PlanView) -> Value {
    json!({
        "taskId": id,
        "status": view.status.wire(),
        "createdAt": view.created.to_rfc3339(),
        "lastUpdatedAt": view.updated.to_rfc3339(),
        "ttl": Value::Null,
        "pollInterval": super::handlers::POLL_INTERVAL_MS,
    })
}

/// ```
/// use harness::mcp::plan::{result_to_call_tool_result, PlanStatus};
///
/// let result = serde_json::json!({ "end": "finished" });
/// let wire = result_to_call_tool_result("plan-1", PlanStatus::Failed, &result);
/// assert_eq!(wire["isError"], true);
/// let ok = result_to_call_tool_result("plan-1", PlanStatus::Completed, &result);
/// assert_eq!(ok["isError"], false);
/// ```
pub fn result_to_call_tool_result(id: &str, status: PlanStatus, result: &Value) -> Value {
    let text = serde_json::to_string_pretty(result).unwrap_or_default();
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": status != PlanStatus::Completed,
        "_meta": { "io.modelcontextprotocol/related-task": { "taskId": id } },
    })
}

/// ```
/// let definition = harness::mcp::plan::tool_definition();
/// assert_eq!(definition["name"], "plan_task");
/// assert_eq!(definition["execution"]["taskSupport"], "required");
/// ```
pub fn tool_definition() -> Value {
    json!({
        "name": "plan_task",
        "description": "Plan a task into subtasks over the identity catalog, submit every subtask to the spawn auditor, and run the approved plan as a set of scoped agents in dependency order. Task-augmented: invoke with a `task` field and fetch the result with tasks/result.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "The task to plan and perform" },
                "repo_path": { "type": "string", "description": "Absolute path to the git repository" },
                "branch": { "type": "string", "description": "Branch or ref to base the worktrees on (default: HEAD)" },
                "max_iterations": { "type": "integer", "description": "Maximum agent iterations per subtask (default: server default)" },
                "budget_secs": { "type": "integer", "description": "Wall-clock budget for the whole run in seconds (default 600, maximum 1800)" }
            },
            "required": ["description", "repo_path"]
        },
        "execution": { "taskSupport": "required" }
    })
}

#[derive(Debug, Error)]
pub enum PlanTaskError {
    #[error("missing or invalid argument `{0}`")]
    Argument(&'static str),
    #[error("budget_secs must be between 1 and {MAX_BUDGET_SECS}")]
    Budget,
    #[error("repo-local identity overrides rejected: {0}")]
    Overrides(#[from] IdentityError),
    #[error("the planner model call failed")]
    Model,
    #[error("planning failed: {0}")]
    Planning(PlanError),
    #[error("the plan cannot be audited: {0}")]
    Audit(#[from] PlanAuditError),
    #[error("the plan was refused: {0}")]
    Refused(String),
    #[error("the approved plan cannot be run: {0}")]
    Run(#[from] PlanRunError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

impl From<PlanError> for PlanTaskError {
    fn from(error: PlanError) -> Self {
        match error {
            PlanError::Model(_) => PlanTaskError::Model,
            other => PlanTaskError::Planning(other),
        }
    }
}

impl PlanTaskError {
    pub fn code(&self) -> i32 {
        match self {
            PlanTaskError::Argument(_) | PlanTaskError::Budget => -32602,
            PlanTaskError::Registry(RegistryError::TooManyRunning(_)) => -32001,
            _ => -32000,
        }
    }
}

fn code_name(code: crate::auditor::ReasonCode) -> String {
    let value = serde_json::to_value(code).unwrap_or_default();
    value.as_str().unwrap_or_default().to_string()
}

fn describe_refusal(refusal: &PlanRefusal) -> String {
    let mut parts = Vec::new();
    for failure in &refusal.failures {
        let part = match failure {
            NodeFailure::Refused {
                id,
                refusal: Refused::Verdict { verdict, .. },
            } => {
                let codes: Vec<String> = verdict
                    .reasons()
                    .iter()
                    .map(|r| code_name(r.code))
                    .collect();
                format!("node `{id}` {:?} ({})", verdict.kind(), codes.join(", "))
            }
            NodeFailure::Refused { id, .. } => format!("node `{id}` could not be audited"),
            NodeFailure::Skipped { id, blocked_by } => {
                format!("node `{id}` skipped (blocked by `{blocked_by}`)")
            }
            NodeFailure::TimedOut { id } => format!("node `{id}` timed out"),
        };
        parts.push(part);
    }
    parts.join("; ")
}

struct PlanArgs {
    description: String,
    repo_path: PathBuf,
    branch: String,
    max_iterations: usize,
    budget: Duration,
}

fn parse_args(params: &Value, default_iterations: usize) -> Result<PlanArgs, PlanTaskError> {
    let text = |key: &'static str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .ok_or(PlanTaskError::Argument(key))
    };
    let description = text("description")?.trim().to_string();
    if description.is_empty() || description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(PlanTaskError::Argument("description"));
    }
    let repo_path = PathBuf::from(text("repo_path")?);
    if !repo_path.is_dir() {
        return Err(PlanTaskError::Argument("repo_path"));
    }
    let branch = params
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or("HEAD")
        .to_string();
    let iterations = params.get("max_iterations").and_then(Value::as_u64);
    let max_iterations = iterations
        .map_or(default_iterations, |n| n as usize)
        .clamp(1, MAX_ITERATIONS_CAP);
    let secs = match params.get("budget_secs") {
        None => DEFAULT_BUDGET_SECS,
        Some(value) => value.as_u64().ok_or(PlanTaskError::Budget)?,
    };
    if secs == 0 || secs > MAX_BUDGET_SECS {
        return Err(PlanTaskError::Budget);
    }
    Ok(PlanArgs {
        description,
        repo_path,
        branch,
        max_iterations,
        budget: Duration::from_secs(secs),
    })
}

fn untrusted(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(UNTRUSTED_CLIP)
        .collect()
}

fn outcome_json(outcome: &ChildOutcome<'_>) -> Value {
    match outcome {
        ChildOutcome::Waiting => json!({ "state": "waiting" }),
        ChildOutcome::Starting => json!({ "state": "starting" }),
        ChildOutcome::Abandoned => json!({ "state": "abandoned" }),
        ChildOutcome::Running(task) => json!({ "state": "running", "task_id": task.0 }),
        ChildOutcome::Completed { task, .. } => json!({ "state": "completed", "task_id": task.0 }),
        ChildOutcome::Failed { task, error } => {
            json!({ "state": "failed", "task_id": task.0, "error_untrusted": untrusted(error) })
        }
        ChildOutcome::Cancelled(task) => json!({ "state": "cancelled", "task_id": task.0 }),
        ChildOutcome::Refused(refusal) => {
            json!({ "state": "refused", "reason": refusal.to_string() })
        }
        ChildOutcome::Lost(task) => json!({ "state": "lost", "task_id": task.0 }),
        ChildOutcome::Skipped { blocked_by } => {
            json!({ "state": "skipped", "blocked_by": blocked_by })
        }
    }
}

fn summary_json(summary: &PlanSummary) -> Value {
    let shown: Vec<&String> = summary
        .files_modified
        .iter()
        .take(MAX_REPORTED_FILES)
        .collect();
    json!({
        "completed": summary.completed,
        "failed": summary.failed,
        "skipped": summary.skipped,
        "unfinished": summary.unfinished,
        "succeeded": summary.succeeded,
        "effects_may_be_understated": summary.effects_may_be_understated,
        "max_effect": summary.max_effect,
        "files_modified": shown,
        "files_truncated": summary.files_modified.len() > MAX_REPORTED_FILES,
        "denials": summary.denials,
        "tool_calls": summary.tool_calls,
    })
}

fn end_name(end: RunEnd) -> &'static str {
    match end {
        RunEnd::Finished => "finished",
        RunEnd::BudgetExhausted => "budget_exhausted",
        RunEnd::Cancelled => "cancelled",
    }
}

/// ```
/// use harness::mcp::plan::run_status;
/// use harness::mcp::plan::PlanStatus;
/// use harness::plan_dispatch::RunEnd;
///
/// assert_eq!(run_status(RunEnd::Cancelled, true), PlanStatus::Cancelled);
/// assert_eq!(run_status(RunEnd::Finished, true), PlanStatus::Completed);
/// assert_eq!(run_status(RunEnd::Finished, false), PlanStatus::Failed);
/// assert_eq!(run_status(RunEnd::BudgetExhausted, true), PlanStatus::Failed);
/// ```
pub fn run_status(end: RunEnd, succeeded: bool) -> PlanStatus {
    match (end, succeeded) {
        (RunEnd::Cancelled, _) => PlanStatus::Cancelled,
        (RunEnd::Finished, true) => PlanStatus::Completed,
        _ => PlanStatus::Failed,
    }
}

fn render(spawns: &[Spawn], run: &PlanRun, end: RunEnd) -> (PlanStatus, Value) {
    let summary = run.summary();
    let outcomes: BTreeMap<&str, Value> = run
        .children()
        .map(|(id, o)| (id, outcome_json(&o)))
        .collect();
    let node = |spawn: &Spawn| {
        json!({
            "id": spawn.id,
            "identity": spawn.identity,
            "loop": spawn.dev_loop,
            "depends_on": spawn.depends_on,
            "subtask_untrusted": untrusted(&spawn.subtask),
            "outcome": outcomes.get(spawn.id.as_str()).cloned().unwrap_or_default(),
        })
    };
    let nodes: Vec<Value> = spawns.iter().map(node).collect();
    let result = json!({
        "end": end_name(end),
        "gave_up": end != RunEnd::Finished,
        "summary": summary_json(&summary),
        "nodes": nodes,
    });
    (run_status(end, summary.succeeded), result)
}

pub struct PlanTaskEnv<'a> {
    pub task_manager: &'a Arc<TaskManager>,
    pub provider: &'a Arc<dyn ModelProvider>,
    pub spawn_gate: &'a SpawnGate,
    pub registry: &'a Arc<PlanRegistry>,
    pub planner_model: &'a str,
    pub default_max_iterations: usize,
}

pub async fn handle_plan_task(
    params: &Value,
    env: &PlanTaskEnv<'_>,
) -> Result<Value, PlanTaskError> {
    let args = parse_args(params, env.default_max_iterations)?;
    env.registry.ensure_room()?;
    let context = env
        .spawn_gate
        .context()
        .with_repo_overrides(&args.repo_path)?;
    let planner = Planner::new(Arc::clone(env.provider), env.planner_model);
    let plan = planner
        .plan(&args.description, context.catalog(), context.repo_profile())
        .await?;
    let id = PlanRegistry::new_id();
    let repo = crate::scope::origin_slug(&args.repo_path)
        .unwrap_or_else(|| args.repo_path.display().to_string());
    let parent = TaskSummary::new(id.clone(), args.description.clone(), repo);
    let audit = audit_plan(&plan, &parent, env.spawn_gate, &args.repo_path).await?;
    let approved = audit
        .into_approved()
        .map_err(|refusal| PlanTaskError::Refused(describe_refusal(&refusal)))?;
    let spawns: Vec<Spawn> = approved.iter().map(|(spawn, _)| spawn.clone()).collect();
    let target = DispatchTarget {
        repo_path: args.repo_path,
        branch: args.branch,
        max_iterations: args.max_iterations,
        provider: Arc::clone(env.provider),
    };
    let mut run = PlanRun::new(approved, target)?;
    let cancel = env.registry.register(&id)?;
    let (manager, registry, run_id, budget) = (
        Arc::clone(env.task_manager),
        Arc::clone(env.registry),
        id.clone(),
        args.budget,
    );
    tokio::spawn(async move {
        let mut guard = RunGuard {
            registry: Arc::clone(&registry),
            id: run_id.clone(),
            done: false,
        };
        let end = run
            .run_with_cancel(&*manager, POLL_EVERY, budget, cancel)
            .await;
        let (status, result) = render(&spawns, &run, end);
        registry.finish(&run_id, status, result);
        guard.done = true;
    });
    let view = env.registry.view(&id)?;
    let mut wire = view_to_wire(&id, &view);
    wire["statusMessage"] = json!(format!(
        "plan accepted: {} node(s) approved by the spawn auditor",
        plan.nodes().len()
    ));
    Ok(wire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::test_support::{fixture_gate, repo_with_origin};
    use crate::auditor::{AuditContext, AuditError, AuditLog, Auditor, SpawnRequest, SpawnVerdict};
    use crate::identity::IdentityCatalog;
    use crate::mcp::jsonrpc::JsonRpcRequest;
    use crate::mcp::NannaMcpServer;
    use async_trait::async_trait;
    use model::types::{
        ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, MessageRole, ModelInfo,
    };
    use model::{ModelError, ModelResult};

    struct Scripted {
        reply: Option<String>,
    }

    #[async_trait]
    impl ModelProvider for Scripted {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            let Some(text) = &self.reply else {
                return Err(ModelError::ServiceUnavailable {
                    message: "backend secret detail".to_string(),
                });
            };
            let message = ChatMessage {
                role: MessageRole::Assistant,
                content: Some(text.clone()),
                tool_calls: None,
                tool_call_id: None,
            };
            let choice = Choice {
                message,
                finish_reason: Some(FinishReason::Stop),
            };
            Ok(ChatResponse {
                choices: vec![choice],
                usage: None,
            })
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "scripted"
        }
    }

    fn plan_reply(nodes: &[(&str, &str, &str, &str, &[&str])]) -> String {
        let nodes: Vec<Value> = nodes
            .iter()
            .map(|(id, identity, subtask, dev_loop, deps)| {
                json!({ "id": id, "identity": identity, "subtask": subtask, "loop": dev_loop, "depends_on": deps })
            })
            .collect();
        json!({ "nodes": nodes }).to_string()
    }

    fn two_node_reply() -> String {
        plan_reply(&[
            ("fix", "rust-implementer", "Fix bug X.", "inner", &[]),
            (
                "shepherd",
                "pr-shepherd",
                "Re-run CI on the PR.",
                "middle",
                &["fix"],
            ),
        ])
    }

    fn server(reply: Option<String>, planner: bool) -> NannaMcpServer {
        let provider: Arc<dyn ModelProvider> = Arc::new(Scripted { reply });
        let built = NannaMcpServer::new(Arc::new(TaskManager::new(0)), provider, fixture_gate(), 5);
        if planner {
            return built.with_planner("planner-model");
        }
        built
    }

    struct Judge {
        fail: bool,
    }

    #[async_trait]
    impl Auditor for Judge {
        fn name(&self) -> &str {
            "judge"
        }

        async fn review_spawn(
            &self,
            _request: &SpawnRequest,
            _context: &AuditContext,
        ) -> Result<SpawnVerdict, AuditError> {
            if self.fail {
                return Err(AuditError::Model(ModelError::RateLimit));
            }
            tokio::time::sleep(Duration::from_secs(10_000)).await;
            Ok(SpawnVerdict::Allow)
        }
    }

    fn judged_server(judge: Judge) -> NannaMcpServer {
        let fixtures = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/identities/global"
        );
        let catalog = IdentityCatalog::load(fixtures).unwrap();
        let auditor = catalog.get("auditor").unwrap().clone();
        let context = AuditContext::new(catalog, auditor).unwrap();
        let gate = SpawnGate::new(Box::new(judge), AuditLog::in_memory(), context);
        let provider: Arc<dyn ModelProvider> = Arc::new(Scripted {
            reply: Some(two_node_reply()),
        });
        NannaMcpServer::new(Arc::new(TaskManager::new(0)), provider, gate, 5)
            .with_planner("planner-model")
    }

    #[tokio::test]
    async fn an_auditor_that_cannot_decide_refuses_the_plan_and_submits_nothing() {
        let server = judged_server(Judge { fail: true });
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let response = call(&server, "tools/call", plan_call(repo.path(), json!({}))).await;
        assert!(error_message(&response).contains("node `fix` could not be audited"));
        assert!(server.task_manager.list().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_plan_whose_audit_exceeds_the_deadline_is_refused_as_timed_out() {
        let server = judged_server(Judge { fail: false });
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let response = call(&server, "tools/call", plan_call(repo.path(), json!({}))).await;
        assert!(error_message(&response).contains("node `fix` timed out"));
        assert!(server.plans.views().is_empty());
    }

    async fn call(server: &NannaMcpServer, method: &str, params: Value) -> Value {
        let request = JsonRpcRequest::call(json!(1), method, params);
        let response = server.handle_request(request).await;
        serde_json::to_value(response).unwrap()
    }

    fn plan_call(repo: &std::path::Path, extra: Value) -> Value {
        let mut arguments = json!({ "description": "fix bug X and ship it", "repo_path": repo });
        if let (Some(base), Some(more)) = (arguments.as_object_mut(), extra.as_object()) {
            base.extend(more.clone());
        }
        json!({ "name": "plan_task", "arguments": arguments, "task": {} })
    }

    fn error_message(response: &Value) -> String {
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test]
    async fn plan_task_is_absent_and_method_not_found_without_a_planner_model() {
        let server = server(Some(two_node_reply()), false);
        let listed = call(&server, "tools/list", json!({})).await;
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(!names.contains(&"plan_task"));
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let response = call(&server, "tools/call", plan_call(repo.path(), json!({}))).await;
        assert_eq!(response["error"]["code"], -32601);
        assert!(error_message(&response).contains("not enabled"));
    }

    #[tokio::test]
    async fn plan_task_is_listed_and_requires_task_augmentation_with_a_planner_model() {
        let server = server(Some(two_node_reply()), true);
        let listed = call(&server, "tools/list", json!({})).await;
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"plan_task"));
        let plain_call = json!({ "name": "plan_task", "arguments": {} });
        let response = call(&server, "tools/call", plain_call).await;
        assert_eq!(response["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn an_accepted_plan_runs_in_the_background_and_can_be_polled_and_cancelled() {
        let server = server(Some(two_node_reply()), true);
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let response = call(&server, "tools/call", plan_call(repo.path(), json!({}))).await;
        let task = &response["result"]["task"];
        let id = task["taskId"].as_str().unwrap().to_string();
        assert!(is_plan_id(&id));
        assert_eq!(task["status"], "working");
        let polled = call(&server, "tasks/get", json!({ "taskId": id })).await;
        assert_eq!(polled["result"]["taskId"], id);
        let listed = call(&server, "tasks/list", json!({})).await;
        assert!(listed["result"]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["taskId"] == id));
        let cancelled = call(&server, "tasks/cancel", json!({ "taskId": id })).await;
        assert_eq!(cancelled["result"]["status"], "cancelled");
        let result = call(&server, "tasks/result", json!({ "taskId": id })).await;
        assert_eq!(result["result"]["isError"], true);
        let text = result["result"]["content"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["end"], "cancelled");
        assert_eq!(body["gave_up"], true);
        assert_eq!(body["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(body["nodes"][0]["identity"], "rust-implementer");
        assert_eq!(body["nodes"][0]["outcome"]["state"], "skipped");
        assert_eq!(body["nodes"][0]["outcome"]["blocked_by"], "cancelled");
        assert_eq!(body["summary"]["effects_may_be_understated"], false);
        assert_eq!(body["summary"]["unfinished"], 0);
        let again = call(&server, "tasks/cancel", json!({ "taskId": id })).await;
        assert_eq!(again["error"]["code"], -32602);
        assert!(error_message(&again).contains("already finished"));
    }

    #[tokio::test(start_paused = true)]
    async fn the_budget_bounds_the_run_and_the_result_says_it_gave_up() {
        let server = server(Some(two_node_reply()), true);
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let response = call(
            &server,
            "tools/call",
            plan_call(repo.path(), json!({ "budget_secs": 2 })),
        )
        .await;
        let id = response["result"]["task"]["taskId"]
            .as_str()
            .unwrap()
            .to_string();
        let result = call(&server, "tasks/result", json!({ "taskId": id })).await;
        let body: Value =
            serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["end"], "budget_exhausted");
        assert_eq!(body["gave_up"], true);
        let polled = call(&server, "tasks/get", json!({ "taskId": id })).await;
        assert_eq!(polled["result"]["status"], "failed");
    }

    #[tokio::test]
    async fn bad_arguments_and_budgets_are_rejected_before_anything_runs() {
        let server = server(Some(two_node_reply()), true);
        let repo = repo_with_origin("https://github.com/example/repo.git");
        for extra in [
            json!({ "budget_secs": 0 }),
            json!({ "budget_secs": MAX_BUDGET_SECS + 1 }),
            json!({ "budget_secs": "soon" }),
        ] {
            let response = call(&server, "tools/call", plan_call(repo.path(), extra)).await;
            assert_eq!(response["error"]["code"], -32602);
            assert!(error_message(&response).contains("budget_secs"));
        }
        let missing =
            json!({ "name": "plan_task", "arguments": { "repo_path": repo.path() }, "task": {} });
        let response = call(&server, "tools/call", missing).await;
        assert!(error_message(&response).contains("`description`"));
        let blank = plan_call(repo.path(), json!({ "description": "   " }));
        assert!(error_message(&call(&server, "tools/call", blank).await).contains("`description`"));
        let too_long = plan_call(
            repo.path(),
            json!({ "description": "x".repeat(MAX_DESCRIPTION_CHARS + 1) }),
        );
        assert!(
            error_message(&call(&server, "tools/call", too_long).await).contains("`description`")
        );
        let no_dir = plan_call(std::path::Path::new("/nonexistent-nanna-repo"), json!({}));
        assert!(error_message(&call(&server, "tools/call", no_dir).await).contains("`repo_path`"));
        assert!(server.plans.views().is_empty());
    }

    #[tokio::test]
    async fn a_planner_model_failure_is_generic_and_never_echoes_provider_text() {
        let server = server(None, true);
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let response = call(&server, "tools/call", plan_call(repo.path(), json!({}))).await;
        let message = error_message(&response);
        assert!(message.contains("planner model call failed"));
        assert!(!message.contains("secret"));
        assert_eq!(response["error"]["code"], -32000);
    }

    #[tokio::test]
    async fn garbled_planner_output_is_a_typed_error_that_does_not_echo_it() {
        let server = server(
            Some("here is sk-live-SECRET-TOKEN, not json".to_string()),
            true,
        );
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let response = call(&server, "tools/call", plan_call(repo.path(), json!({}))).await;
        let message = error_message(&response);
        assert!(message.contains("planning failed"));
        assert!(!message.contains("SECRET"));
        assert!(server.plans.views().is_empty());
    }

    #[tokio::test]
    async fn a_hallucinated_identity_is_rejected_with_a_sanitised_name_and_nothing_is_submitted() {
        let reply = plan_reply(&[("a", "@admin sk-live-SECRET", "Fix bug X.", "inner", &[])]);
        let server = server(Some(reply), true);
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let response = call(&server, "tools/call", plan_call(repo.path(), json!({}))).await;
        let message = error_message(&response);
        assert!(message.contains("not in the catalog"));
        assert!(!message.contains('@') && !message.contains("live-SECRET "));
        assert!(server.plans.views().is_empty());
        assert!(server.task_manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn an_auditor_refusal_surfaces_node_and_reason_codes_and_submits_nothing() {
        let reply = plan_reply(&[
            ("fix", "rust-implementer", "Fix bug X.", "inner", &[]),
            (
                "push",
                "rust-implementer",
                "Deploy build 42 to production.",
                "inner",
                &["fix"],
            ),
            (
                "ship",
                "pr-shepherd",
                "Re-run CI on the PR.",
                "middle",
                &["push"],
            ),
        ]);
        let server = server(Some(reply), true);
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let response = call(&server, "tools/call", plan_call(repo.path(), json!({}))).await;
        let message = error_message(&response);
        assert!(message.contains("the plan was refused"));
        assert!(message.contains("node `push`"));
        assert!(message.contains("node `ship` skipped (blocked by `push`)"));
        assert!(!message.contains("Deploy build 42"));
        assert!(server.task_manager.list().await.is_empty());
        assert!(server.plans.views().is_empty());
    }

    #[tokio::test]
    async fn unknown_plan_ids_are_typed_not_found_on_every_task_method() {
        let server = server(Some(two_node_reply()), true);
        for method in ["tasks/get", "tasks/result", "tasks/cancel"] {
            let response = call(&server, method, json!({ "taskId": "plan-does-not-exist" })).await;
            assert_eq!(response["error"]["code"], -32602, "{method}");
            assert!(
                error_message(&response).contains("was not found"),
                "{method}"
            );
        }
    }

    #[test]
    fn the_registry_caps_concurrent_plans() {
        let registry = PlanRegistry::default();
        let mut receivers = Vec::new();
        for n in 0..MAX_CONCURRENT_PLANS {
            receivers.push(registry.register(&format!("plan-{n}")).unwrap());
        }
        assert_eq!(registry.running_count(), MAX_CONCURRENT_PLANS);
        assert_eq!(
            registry.ensure_room(),
            Err(RegistryError::TooManyRunning(MAX_CONCURRENT_PLANS))
        );
        assert!(matches!(
            registry.register("plan-extra"),
            Err(RegistryError::TooManyRunning(_))
        ));
        registry.finish("plan-0", PlanStatus::Completed, json!({}));
        assert_eq!(registry.ensure_room(), Ok(()));
        assert!(registry.register("plan-extra").is_ok());
    }

    #[test]
    fn the_registry_keeps_the_newest_finished_plans_and_evicts_the_oldest_first() {
        let registry = PlanRegistry::default();
        for n in 0..MAX_RETAINED_PLANS + 3 {
            let id = format!("plan-{n}");
            let _receiver = registry.register(&id).unwrap();
            registry.finish(&id, PlanStatus::Completed, json!({ "n": n }));
        }
        assert_eq!(registry.views().len(), MAX_RETAINED_PLANS);
        assert!(matches!(
            registry.view("plan-0"),
            Err(RegistryError::NotFound(_))
        ));
        assert!(matches!(
            registry.view("plan-2"),
            Err(RegistryError::NotFound(_))
        ));
        assert!(registry.view("plan-3").is_ok());
        assert!(registry
            .view(&format!("plan-{}", MAX_RETAINED_PLANS + 2))
            .is_ok());
    }

    #[test]
    fn running_plans_are_never_evicted_by_finished_ones() {
        let registry = PlanRegistry::default();
        let _running = registry.register("plan-running").unwrap();
        for n in 0..MAX_RETAINED_PLANS + 2 {
            let id = format!("plan-done-{n}");
            let _receiver = registry.register(&id).unwrap();
            registry.finish(&id, PlanStatus::Completed, json!({}));
            let _ = registry.cancel("plan-nothing");
        }
        assert_eq!(
            registry.view("plan-running").unwrap().status,
            PlanStatus::Working
        );
    }

    #[tokio::test]
    async fn an_oversized_result_is_replaced_by_a_truncated_summary() {
        let registry = PlanRegistry::default();
        let _receiver = registry.register("plan-big").unwrap();
        let big = "x".repeat(MAX_RESULT_BYTES);
        registry.finish(
            "plan-big",
            PlanStatus::Completed,
            json!({ "summary": { "ok": true }, "nodes": big }),
        );
        let (_, result) = registry.wait_finished("plan-big").await.unwrap();
        assert_eq!(result["truncated"], true);
        assert_eq!(result["summary"]["ok"], true);
        assert!(result.to_string().len() < MAX_RESULT_BYTES);
        let _small = registry.register("plan-small").unwrap();
        registry.finish("plan-small", PlanStatus::Completed, json!({ "nodes": [] }));
        let (status, small) = registry.wait_finished("plan-small").await.unwrap();
        assert_eq!(
            (status, small["nodes"].is_array()),
            (PlanStatus::Completed, true)
        );
    }

    #[test]
    fn finishing_twice_or_an_unknown_plan_changes_nothing() {
        let registry = PlanRegistry::default();
        registry.finish("plan-ghost", PlanStatus::Completed, json!({}));
        assert!(registry.views().is_empty());
        let _receiver = registry.register("plan-a").unwrap();
        registry.finish("plan-a", PlanStatus::Failed, json!({ "first": true }));
        registry.finish("plan-a", PlanStatus::Completed, json!({ "second": true }));
        assert_eq!(registry.view("plan-a").unwrap().status, PlanStatus::Failed);
    }

    #[test]
    fn a_dropped_run_guard_fails_the_plan_instead_of_leaking_its_slot() {
        let registry = Arc::new(PlanRegistry::default());
        let _receiver = registry.register("plan-a").unwrap();
        let guard = RunGuard {
            registry: Arc::clone(&registry),
            id: "plan-a".to_string(),
            done: false,
        };
        drop(guard);
        assert_eq!(registry.view("plan-a").unwrap().status, PlanStatus::Failed);
        assert_eq!(registry.running_count(), 0);
        let _other = registry.register("plan-b").unwrap();
        let finished = RunGuard {
            registry: Arc::clone(&registry),
            id: "plan-b".to_string(),
            done: true,
        };
        drop(finished);
        assert_eq!(registry.view("plan-b").unwrap().status, PlanStatus::Working);
    }

    #[test]
    fn plan_status_and_error_codes_have_stable_wire_names() {
        let names = [
            PlanStatus::Working,
            PlanStatus::Completed,
            PlanStatus::Failed,
            PlanStatus::Cancelled,
        ]
        .map(PlanStatus::wire);
        assert_eq!(names, ["working", "completed", "failed", "cancelled"]);
        assert_eq!(PlanTaskError::Budget.code(), -32602);
        assert_eq!(PlanTaskError::Argument("x").code(), -32602);
        assert_eq!(PlanTaskError::Model.code(), -32000);
        assert_eq!(
            PlanTaskError::from(RegistryError::TooManyRunning(1)).code(),
            -32001
        );
        assert!(!is_plan_id("0b1c2d"));
        assert_eq!(plain("a b@c"), "a?b?c");
        assert_eq!(plain(&"z".repeat(200)).len(), 80);
    }

    #[test]
    fn untrusted_text_is_stripped_of_control_characters_and_clipped() {
        let hostile = format!("line one\nline two\u{7}{}", "y".repeat(500));
        let shown = untrusted(&hostile);
        assert!(!shown.contains('\n') && !shown.contains('\u{7}'));
        assert_eq!(shown.chars().count(), UNTRUSTED_CLIP);
    }

    #[test]
    fn the_scripted_provider_describes_itself() {
        let provider = Scripted { reply: None };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(provider.list_models().await.unwrap().is_empty());
            provider.health_check().await.unwrap();
        });
        assert_eq!(provider.provider_name(), "scripted");
    }

    #[test]
    fn defaults_and_bounds_are_applied_to_the_arguments() {
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let minimal = json!({ "description": "do it", "repo_path": repo.path() });
        let args = parse_args(&minimal, 7).unwrap();
        assert_eq!(args.budget, Duration::from_secs(DEFAULT_BUDGET_SECS));
        assert_eq!((args.max_iterations, args.branch.as_str()), (7, "HEAD"));
        let tuned = json!({ "description": "do it", "repo_path": repo.path(), "max_iterations": 1_000_000, "budget_secs": MAX_BUDGET_SECS, "branch": "dev" });
        let args = parse_args(&tuned, 7).unwrap();
        assert_eq!(args.budget, Duration::from_secs(MAX_BUDGET_SECS));
        assert_eq!(
            (args.max_iterations, args.branch.as_str()),
            (MAX_ITERATIONS_CAP, "dev")
        );
        let one = json!({ "description": "do it", "repo_path": repo.path(), "budget_secs": 1 });
        assert_eq!(parse_args(&one, 7).unwrap().budget, Duration::from_secs(1));
    }

    #[test]
    fn the_run_end_and_success_map_to_the_wire_status() {
        assert_eq!(run_status(RunEnd::Cancelled, true), PlanStatus::Cancelled);
        assert_eq!(run_status(RunEnd::Finished, true), PlanStatus::Completed);
        assert_eq!(run_status(RunEnd::Finished, false), PlanStatus::Failed);
        assert_eq!(
            run_status(RunEnd::BudgetExhausted, true),
            PlanStatus::Failed
        );
        assert_eq!(end_name(RunEnd::Finished), "finished");
        assert_eq!(end_name(RunEnd::BudgetExhausted), "budget_exhausted");
        assert_eq!(end_name(RunEnd::Cancelled), "cancelled");
    }

    #[test]
    fn the_summary_json_caps_the_reported_files_and_says_so() {
        let files_modified = (0..MAX_REPORTED_FILES + 5)
            .map(|n| format!("f{n}"))
            .collect();
        let summary = PlanSummary {
            files_modified,
            ..PlanSummary::default()
        };
        let shown = summary_json(&summary);
        assert_eq!(
            shown["files_modified"].as_array().unwrap().len(),
            MAX_REPORTED_FILES
        );
        assert_eq!(shown["files_truncated"], true);
        let small = summary_json(&PlanSummary::default());
        assert_eq!(small["files_truncated"], false);
    }

    fn finished_result() -> crate::task::TaskResult {
        crate::task::TaskResult {
            result_summary: "done".to_string(),
            changes_patch: None,
            format_patch: None,
            files_modified: vec![],
            tool_calls_made: vec![],
            denials: vec![],
            action_audit: vec![],
            iterations: 1,
            model_used: "mock".to_string(),
            blast_radius: None,
            qa_summary: Default::default(),
            budget: Default::default(),
        }
    }

    #[test]
    fn every_child_outcome_has_a_stable_json_form() {
        let task = crate::task::TaskId("t-1".to_string());
        let result = finished_result();
        let refusal = crate::task::SpawnRefused::ModelMismatch {
            audited: "a".to_string(),
            requested: "b".to_string(),
        };
        let cases = [
            (ChildOutcome::Waiting, "waiting"),
            (ChildOutcome::Starting, "starting"),
            (ChildOutcome::Abandoned, "abandoned"),
            (ChildOutcome::Running(&task), "running"),
            (
                ChildOutcome::Completed {
                    task: &task,
                    result: &result,
                },
                "completed",
            ),
            (
                ChildOutcome::Failed {
                    task: &task,
                    error: "boom\nline",
                },
                "failed",
            ),
            (ChildOutcome::Cancelled(&task), "cancelled"),
            (ChildOutcome::Refused(&refusal), "refused"),
            (ChildOutcome::Lost(&task), "lost"),
            (ChildOutcome::Skipped { blocked_by: "fix" }, "skipped"),
        ];
        for (outcome, state) in cases {
            assert_eq!(outcome_json(&outcome)["state"], state);
        }
        let failed = ChildOutcome::Failed {
            task: &task,
            error: "boom\nline",
        };
        assert_eq!(outcome_json(&failed)["error_untrusted"], "boomline");
        assert_eq!(
            outcome_json(&ChildOutcome::Running(&task))["task_id"],
            "t-1"
        );
        assert_eq!(
            outcome_json(&ChildOutcome::Skipped { blocked_by: "fix" })["blocked_by"],
            "fix"
        );
        assert!(outcome_json(&ChildOutcome::Refused(&refusal))["reason"]
            .as_str()
            .unwrap()
            .contains("audited"));
    }

    #[test]
    fn a_poisoned_registry_lock_is_recovered_not_propagated() {
        let registry = Arc::new(PlanRegistry::default());
        let _receiver = registry.register("plan-a").unwrap();
        let shared = Arc::clone(&registry);
        let crashed = std::thread::spawn(move || {
            let _held = shared.inner.lock().unwrap();
            panic!("poison the registry lock");
        })
        .join();
        assert!(crashed.is_err());
        assert_eq!(registry.view("plan-a").unwrap().status, PlanStatus::Working);
        assert!(registry.register("plan-b").is_ok());
    }
}
