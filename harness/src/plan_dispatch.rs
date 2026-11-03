use crate::auditor::Allowed;
use crate::effects::EffectClass;
use crate::entities::context::types::ToolCallRecord;
use crate::planner::{Plan, PlanRejection, Spawn};
use crate::task::{FailureDiagnostics, SpawnRefused, TaskId, TaskManager, TaskResult, TaskStatus};
use async_trait::async_trait;
use model::ModelProvider;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::time::Instant;

const BUDGET: &str = "budget";

/// ```
/// use harness::plan_dispatch::DispatchTarget;
/// use model::{OllamaConfig, OllamaProvider};
/// use std::sync::Arc;
///
/// let provider = Arc::new(OllamaProvider::new(OllamaConfig::default()).unwrap());
/// let target = DispatchTarget {
///     repo_path: "/work/repo".into(),
///     branch: "HEAD".to_string(),
///     max_iterations: 20,
///     provider,
/// };
/// assert_eq!(target.max_iterations, 20);
/// ```
pub struct DispatchTarget {
    pub repo_path: PathBuf,
    pub branch: String,
    pub max_iterations: usize,
    pub provider: Arc<dyn ModelProvider>,
}

/// ```
/// use async_trait::async_trait;
/// use harness::auditor::Allowed;
/// use harness::plan_dispatch::{ChildRunner, DispatchTarget};
/// use harness::task::{SpawnRefused, TaskId, TaskStatus};
///
/// struct Refusing;
///
/// #[async_trait]
/// impl ChildRunner for Refusing {
///     async fn start(&self, _proof: Allowed, _target: &DispatchTarget) -> Result<TaskId, SpawnRefused> {
///         Err(SpawnRefused::ModelMismatch { audited: "a".to_string(), requested: "b".to_string() })
///     }
///
///     async fn status(&self, _id: &TaskId) -> Option<TaskStatus> {
///         None
///     }
///
///     async fn cancel(&self, _id: &TaskId) {}
/// }
///
/// fn takes_a_runner(_runner: &dyn ChildRunner) {}
/// takes_a_runner(&Refusing);
/// ```
#[async_trait]
pub trait ChildRunner: Send + Sync {
    async fn start(&self, proof: Allowed, target: &DispatchTarget) -> Result<TaskId, SpawnRefused>;

    async fn status(&self, id: &TaskId) -> Option<TaskStatus>;

    async fn cancel(&self, id: &TaskId);
}

#[async_trait]
impl ChildRunner for TaskManager {
    async fn start(&self, proof: Allowed, target: &DispatchTarget) -> Result<TaskId, SpawnRefused> {
        let model = proof.model().to_string();
        let (repo, branch) = (target.repo_path.clone(), target.branch.clone());
        let provider = Arc::clone(&target.provider);
        self.submit_spawn(proof, repo, branch, model, target.max_iterations, provider)
            .await
    }

    async fn status(&self, id: &TaskId) -> Option<TaskStatus> {
        self.poll(id).await.map(|task| task.status)
    }

    async fn cancel(&self, id: &TaskId) {
        let _ = TaskManager::cancel(self, id).await;
    }
}

#[derive(Debug)]
enum ChildState {
    Waiting,
    Starting,
    Abandoned,
    Running(TaskId),
    Completed {
        task: TaskId,
        result: Box<TaskResult>,
    },
    Failed {
        task: TaskId,
        error: String,
        diagnostics: Box<FailureDiagnostics>,
    },
    Cancelled(TaskId),
    Refused(SpawnRefused),
    Lost(TaskId),
    Skipped {
        blocked_by: String,
    },
}

/// ```
/// use harness::plan_dispatch::ChildOutcome;
///
/// let outcome = ChildOutcome::Skipped { blocked_by: "fix" };
/// assert!(matches!(outcome, ChildOutcome::Skipped { blocked_by: "fix" }));
/// assert!(!matches!(outcome, ChildOutcome::Waiting));
/// ```
#[derive(Debug)]
pub enum ChildOutcome<'a> {
    Waiting,
    Starting,
    Abandoned,
    Running(&'a TaskId),
    Completed {
        task: &'a TaskId,
        result: &'a TaskResult,
    },
    Failed {
        task: &'a TaskId,
        error: &'a str,
    },
    Cancelled(&'a TaskId),
    Refused(&'a SpawnRefused),
    Lost(&'a TaskId),
    Skipped {
        blocked_by: &'a str,
    },
}

#[derive(Debug)]
struct Child {
    spawn: Spawn,
    proof: Option<Box<Allowed>>,
    state: ChildState,
}

impl Child {
    fn task_id(&self) -> Option<&TaskId> {
        match &self.state {
            ChildState::Running(task) | ChildState::Cancelled(task) | ChildState::Lost(task) => {
                Some(task)
            }
            ChildState::Completed { task, .. } | ChildState::Failed { task, .. } => Some(task),
            _ => None,
        }
    }

    fn outcome(&self) -> ChildOutcome<'_> {
        match &self.state {
            ChildState::Waiting => ChildOutcome::Waiting,
            ChildState::Starting => ChildOutcome::Starting,
            ChildState::Abandoned => ChildOutcome::Abandoned,
            ChildState::Running(task) => ChildOutcome::Running(task),
            ChildState::Completed { task, result } => ChildOutcome::Completed { task, result },
            ChildState::Failed { task, error, .. } => ChildOutcome::Failed { task, error },
            ChildState::Cancelled(task) => ChildOutcome::Cancelled(task),
            ChildState::Refused(refusal) => ChildOutcome::Refused(refusal),
            ChildState::Lost(task) => ChildOutcome::Lost(task),
            ChildState::Skipped { blocked_by } => ChildOutcome::Skipped { blocked_by },
        }
    }
}

enum Action {
    Skip(String),
    Start,
    Poll(TaskId),
    Hold,
}

#[derive(Debug, Error)]
pub enum PlanRunError {
    #[error("the approved plan is not a valid DAG: {0}")]
    Plan(#[from] PlanRejection),
    #[error("the proof for node `{id}` does not match the node's identity and subtask")]
    ProofMismatch { id: String },
}

/// ```
/// use harness::plan_dispatch::PlanSummary;
///
/// let summary = PlanSummary { completed: 2, ..PlanSummary::default() };
/// assert_eq!(summary.completed, 2);
/// assert!(!summary.succeeded);
/// assert_eq!(summary.max_effect, None);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanSummary {
    pub completed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub unfinished: usize,
    pub succeeded: bool,
    pub effects_may_be_understated: bool,
    pub max_effect: Option<EffectClass>,
    pub files_modified: Vec<String>,
    pub denials: usize,
    pub tool_calls: usize,
}

/// ```
/// # #[tokio::main]
/// # async fn main() {
/// use async_trait::async_trait;
/// use harness::auditor::{Allowed, AuditContext, AuditLog, RuleAuditor, SpawnGate, TaskSummary};
/// use harness::identity::{DevLoop, IdentityCatalog};
/// use harness::plan_dispatch::{ChildOutcome, ChildRunner, DispatchTarget, PlanRun};
/// use harness::planner::{audit_plan, Plan, Spawn};
/// use harness::task::{SpawnRefused, TaskId, TaskStatus};
/// use model::{OllamaConfig, OllamaProvider};
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// struct Refusing;
///
/// #[async_trait]
/// impl ChildRunner for Refusing {
///     async fn start(&self, _proof: Allowed, _target: &DispatchTarget) -> Result<TaskId, SpawnRefused> {
///         Err(SpawnRefused::ModelMismatch { audited: "a".to_string(), requested: "b".to_string() })
///     }
///
///     async fn status(&self, _id: &TaskId) -> Option<TaskStatus> {
///         None
///     }
///
///     async fn cancel(&self, _id: &TaskId) {}
/// }
///
/// let dir = tempfile::tempdir().unwrap();
/// let card = |name: &str, effect: &str, tools: &str| format!(
///     "[identity]\nname = \"{name}\"\ndescription = \"d\"\nloop = \"inner\"\nmodel = \"m\"\n\
///      system_prompt = {{ inline = \"p\" }}\n\n[scope]\nrepos = []\npaths = [\"**\"]\n\
///      max_effect = \"{effect}\"\ntools = [{tools}]\n\n[limits]\nmax_iterations = 1\n\
///      max_wall_clock_secs = 1\nmax_concurrent = 1\n"
/// );
/// std::fs::write(dir.path().join("rust-implementer.toml"), card("rust-implementer", "repository", "\"read_file\"")).unwrap();
/// std::fs::write(dir.path().join("auditor.toml"), card("auditor", "none", "")).unwrap();
/// let catalog = IdentityCatalog::load(dir.path()).unwrap();
/// let context = AuditContext::new(catalog.clone(), catalog.get("auditor").unwrap().clone()).unwrap();
/// let gate = SpawnGate::new(Box::new(RuleAuditor::new()), AuditLog::in_memory(), context);
///
/// let node = |id: &str, deps: &[&str]| Spawn {
///     id: id.to_string(),
///     identity: "rust-implementer".to_string(),
///     subtask: format!("Fix {id}."),
///     dev_loop: DevLoop::Inner,
///     depends_on: deps.iter().map(|d| d.to_string()).collect(),
/// };
/// let plan = Plan::new(vec![node("a", &[]), node("b", &["a"])]).unwrap();
/// let parent = TaskSummary::new("task-1", "Fix a and b", "github.com/example/repo");
/// let repo = tempfile::tempdir().unwrap();
/// let approved = audit_plan(&plan, &parent, &gate, repo.path()).await.unwrap().into_approved().unwrap();
///
/// let target = || DispatchTarget {
///     repo_path: repo.path().to_path_buf(),
///     branch: "HEAD".to_string(),
///     max_iterations: 5,
///     provider: Arc::new(OllamaProvider::new(OllamaConfig::default()).unwrap()),
/// };
/// let mut run = PlanRun::new(approved, target()).unwrap();
/// let finished = run.run(&Refusing, Duration::from_millis(1), Duration::from_secs(1)).await;
/// assert!(finished && run.is_finished());
/// let outcomes: Vec<(&str, &str)> = run.children().map(|(id, outcome)| match outcome {
///     ChildOutcome::Refused(_) => (id, "refused"),
///     ChildOutcome::Skipped { .. } => (id, "skipped"),
///     _ => (id, "other"),
/// }).collect();
/// assert_eq!(outcomes, [("a", "refused"), ("b", "skipped")]);
/// assert!(run.child_ids().is_empty());
/// let summary = run.summary();
/// assert!(!summary.succeeded);
/// assert_eq!((summary.failed, summary.skipped), (1, 1));
///
/// assert!(PlanRun::new(vec![], target()).is_err());
/// # }
/// ```
pub struct PlanRun {
    children: Vec<Child>,
    target: DispatchTarget,
}

impl PlanRun {
    pub fn new(
        approved: Vec<(Spawn, Allowed)>,
        target: DispatchTarget,
    ) -> Result<Self, PlanRunError> {
        let spawns: Vec<Spawn> = approved.iter().map(|(spawn, _)| spawn.clone()).collect();
        let plan = Plan::new(spawns)?;
        let mut proofs: BTreeMap<String, (Spawn, Allowed)> = BTreeMap::new();
        proofs.extend(approved.into_iter().map(|pair| (pair.0.id.clone(), pair)));
        let mut children = Vec::new();
        for ordered in plan.topological() {
            if let Some((spawn, proof)) = proofs.remove(&ordered.id) {
                if !proof_fits(&spawn, &proof) {
                    return Err(PlanRunError::ProofMismatch { id: spawn.id });
                }
                let proof = Some(Box::new(proof));
                children.push(Child {
                    spawn,
                    proof,
                    state: ChildState::Waiting,
                });
            }
        }
        Ok(Self { children, target })
    }

    pub fn children(&self) -> impl Iterator<Item = (&str, ChildOutcome<'_>)> {
        self.children
            .iter()
            .map(|child| (child.spawn.id.as_str(), child.outcome()))
    }

    pub fn child_ids(&self) -> Vec<(&str, &TaskId)> {
        let tracked = self.children.iter();
        tracked
            .filter_map(|child| Some((child.spawn.id.as_str(), child.task_id()?)))
            .collect()
    }

    pub fn is_finished(&self) -> bool {
        let open = |child: &Child| {
            matches!(
                child.state,
                ChildState::Waiting | ChildState::Starting | ChildState::Running(_)
            )
        };
        !self.children.iter().any(open)
    }

    fn action(&self, index: usize) -> Action {
        let child = &self.children[index];
        if let ChildState::Running(task) = &child.state {
            return Action::Poll(task.clone());
        }
        if !matches!(child.state, ChildState::Waiting) {
            return Action::Hold;
        }
        let mut pending = false;
        for dependency in &child.spawn.depends_on {
            let found = self.children.iter().find(|c| c.spawn.id == *dependency);
            match found.map(|c| &c.state) {
                Some(ChildState::Completed { .. }) => {}
                Some(ChildState::Waiting | ChildState::Starting | ChildState::Running(_)) => {
                    pending = true;
                }
                _ => return Action::Skip(dependency.clone()),
            }
        }
        if pending {
            Action::Hold
        } else {
            Action::Start
        }
    }

    pub async fn advance(&mut self, runner: &dyn ChildRunner) {
        for index in 0..self.children.len() {
            let action = self.action(index);
            let child = &mut self.children[index];
            match action {
                Action::Skip(blocked_by) => child.state = ChildState::Skipped { blocked_by },
                Action::Start => start_child(child, runner, &self.target).await,
                Action::Poll(task) => child.state = settle(runner.status(&task).await, task),
                Action::Hold => {}
            }
        }
    }

    async fn give_up(&mut self, runner: &dyn ChildRunner) {
        for child in &mut self.children {
            let running = match &child.state {
                ChildState::Running(task) => Some(task.clone()),
                _ => None,
            };
            if let Some(task) = running {
                runner.cancel(&task).await;
                child.state = settle(runner.status(&task).await, task);
            }
            if matches!(child.state, ChildState::Starting) {
                child.state = ChildState::Abandoned;
            }
            if matches!(child.state, ChildState::Waiting) {
                child.state = ChildState::Skipped {
                    blocked_by: BUDGET.to_string(),
                };
            }
        }
    }

    pub async fn run(
        &mut self,
        runner: &dyn ChildRunner,
        poll_every: Duration,
        budget: Duration,
    ) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            self.advance(runner).await;
            if self.is_finished() {
                return true;
            }
            if Instant::now() >= deadline {
                self.give_up(runner).await;
                return false;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::time::sleep(poll_every.min(remaining)).await;
        }
    }

    pub fn summary(&self) -> PlanSummary {
        let mut summary = PlanSummary::default();
        let mut files: BTreeSet<String> = BTreeSet::new();
        for child in &self.children {
            match &child.state {
                ChildState::Completed { result, .. } => {
                    summary.completed += 1;
                    let calls = &result.tool_calls_made;
                    absorb(
                        &mut summary,
                        widest(calls),
                        result.denial_count(),
                        calls.len(),
                    );
                    files.extend(result.files_modified.iter().cloned());
                }
                ChildState::Failed { diagnostics, .. } => {
                    summary.failed += 1;
                    let calls = &diagnostics.tool_call_history;
                    absorb(
                        &mut summary,
                        widest(calls),
                        diagnostics.denials.len(),
                        calls.len(),
                    );
                }
                ChildState::Cancelled(_) | ChildState::Lost(_) | ChildState::Abandoned => {
                    summary.failed += 1;
                    summary.effects_may_be_understated = true;
                }
                ChildState::Refused(_) => summary.failed += 1,
                ChildState::Skipped { .. } => summary.skipped += 1,
                ChildState::Waiting => summary.unfinished += 1,
                ChildState::Starting | ChildState::Running(_) => {
                    summary.unfinished += 1;
                    summary.effects_may_be_understated = true;
                }
            }
        }
        summary.succeeded = summary.completed == self.children.len();
        summary.files_modified = files.into_iter().collect();
        summary
    }
}

fn proof_fits(spawn: &Spawn, proof: &Allowed) -> bool {
    let request = proof.request();
    request.identity == spawn.identity && request.subtask == spawn.subtask
}

fn widest(calls: &[ToolCallRecord]) -> Option<EffectClass> {
    calls
        .iter()
        .filter_map(|call| call.effect.as_ref())
        .map(|effect| effect.class)
        .max()
}

fn absorb(summary: &mut PlanSummary, effect: Option<EffectClass>, denials: usize, calls: usize) {
    summary.max_effect = summary.max_effect.max(effect);
    summary.denials += denials;
    summary.tool_calls += calls;
}

async fn start_child(child: &mut Child, runner: &dyn ChildRunner, target: &DispatchTarget) {
    if let Some(proof) = child.proof.take() {
        child.state = ChildState::Starting;
        child.state = match runner.start(*proof, target).await {
            Ok(task) => ChildState::Running(task),
            Err(refusal) => ChildState::Refused(refusal),
        };
    }
}

fn settle(status: Option<TaskStatus>, task: TaskId) -> ChildState {
    match status {
        Some(TaskStatus::Completed { result, .. }) => ChildState::Completed {
            task,
            result: Box::new(result),
        },
        Some(TaskStatus::Failed {
            error, diagnostics, ..
        }) => ChildState::Failed {
            task,
            error,
            diagnostics: Box::new(diagnostics),
        },
        Some(TaskStatus::Cancelled { .. }) => ChildState::Cancelled(task),
        Some(TaskStatus::Pending | TaskStatus::Running { .. }) => ChildState::Running(task),
        None => ChildState::Lost(task),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::test_support::{fixture_gate, repo_with_origin};
    use crate::auditor::TaskSummary;
    use crate::effects::EffectRecord;
    use crate::identity::DevLoop;
    use crate::planner::audit_plan;
    use crate::scope::{DenialReason, ScopeDenial};
    use chrono::Utc;
    use model::types::{ChatRequest, ChatResponse, ModelInfo};
    use model::{ModelError, ModelResult};
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct NoModel;

    #[async_trait]
    impl ModelProvider for NoModel {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            Err(ModelError::RateLimit)
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "none"
        }
    }

    fn target(repo: &std::path::Path) -> DispatchTarget {
        DispatchTarget {
            repo_path: repo.to_path_buf(),
            branch: "HEAD".to_string(),
            max_iterations: 5,
            provider: Arc::new(NoModel),
        }
    }

    fn node(id: &str, identity: &str, dev_loop: DevLoop, subtask: &str, deps: &[&str]) -> Spawn {
        Spawn {
            id: id.to_string(),
            identity: identity.to_string(),
            subtask: subtask.to_string(),
            dev_loop,
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
        }
    }

    fn ship_nodes() -> Vec<Spawn> {
        vec![
            node("fix", "rust-implementer", DevLoop::Inner, "Fix bug X.", &[]),
            node(
                "shepherd",
                "pr-shepherd",
                DevLoop::Middle,
                "Re-run CI on the PR.",
                &["fix"],
            ),
            node(
                "ship",
                "deployer",
                DevLoop::Outer,
                "Deploy the merged change to the sandbox environment.",
                &["shepherd"],
            ),
        ]
    }

    async fn approved(nodes: Vec<Spawn>) -> Vec<(Spawn, Allowed)> {
        let plan = Plan::new(nodes).unwrap();
        let parent = TaskSummary::new("task-1", "fix bug X and ship it", "github.com/example/repo");
        let repo = tempfile::tempdir().unwrap();
        let audit = audit_plan(&plan, &parent, &fixture_gate(), repo.path())
            .await
            .unwrap();
        audit.into_approved().unwrap()
    }

    async fn ship_run() -> PlanRun {
        let pairs = approved(ship_nodes()).await;
        PlanRun::new(pairs, target(std::path::Path::new("/nonexistent"))).unwrap()
    }

    struct Fake {
        started: Mutex<Vec<(String, String, TaskId)>>,
        statuses: Mutex<HashMap<TaskId, TaskStatus>>,
        cancelled: Mutex<Vec<TaskId>>,
        refuse: Option<&'static str>,
        auto_complete: bool,
        start_delay: Duration,
        status_delay: Duration,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                started: Mutex::new(Vec::new()),
                statuses: Mutex::new(HashMap::new()),
                cancelled: Mutex::new(Vec::new()),
                refuse: None,
                auto_complete: false,
                start_delay: Duration::ZERO,
                status_delay: Duration::ZERO,
            }
        }

        fn identities(&self) -> Vec<String> {
            let started = self.started.lock().unwrap();
            started.iter().map(|s| s.0.clone()).collect()
        }

        fn id_of(&self, identity: &str) -> TaskId {
            let started = self.started.lock().unwrap();
            started.iter().find(|s| s.0 == identity).unwrap().2.clone()
        }

        fn set(&self, identity: &str, status: TaskStatus) {
            let id = self.id_of(identity);
            self.statuses.lock().unwrap().insert(id, status);
        }
    }

    #[async_trait]
    impl ChildRunner for Fake {
        async fn start(
            &self,
            proof: Allowed,
            _target: &DispatchTarget,
        ) -> Result<TaskId, SpawnRefused> {
            let request = proof.request();
            let id = TaskId::new();
            let entry = (
                request.identity.clone(),
                request.subtask.clone(),
                id.clone(),
            );
            self.started.lock().unwrap().push(entry);
            tokio::time::sleep(self.start_delay).await;
            if self.refuse == Some(request.identity.as_str()) {
                return Err(SpawnRefused::ModelMismatch {
                    audited: "a".to_string(),
                    requested: "b".to_string(),
                });
            }
            let status = if self.auto_complete {
                completed(&["a.rs"], None)
            } else {
                TaskStatus::Pending
            };
            self.statuses.lock().unwrap().insert(id.clone(), status);
            Ok(id)
        }

        async fn status(&self, id: &TaskId) -> Option<TaskStatus> {
            tokio::time::sleep(self.status_delay).await;
            self.statuses.lock().unwrap().get(id).cloned()
        }

        async fn cancel(&self, id: &TaskId) {
            self.cancelled.lock().unwrap().push(id.clone());
            self.statuses
                .lock()
                .unwrap()
                .insert(id.clone(), cancelled());
        }
    }

    fn call(class: EffectClass) -> ToolCallRecord {
        ToolCallRecord {
            tool_name: "t".to_string(),
            arguments: serde_json::json!({}),
            call_id: "c".to_string(),
            result: String::new(),
            effect: Some(EffectRecord::new(class)),
        }
    }

    fn denial() -> ScopeDenial {
        ScopeDenial {
            identity: "rust-implementer".to_string(),
            tool: "write_file".to_string(),
            reason: DenialReason::ToolNotInScope,
        }
    }

    fn result(files: &[&str], effect: Option<EffectClass>) -> TaskResult {
        TaskResult {
            result_summary: "done".to_string(),
            changes_patch: None,
            format_patch: None,
            files_modified: files.iter().map(|f| f.to_string()).collect(),
            tool_calls_made: effect.into_iter().map(call).collect(),
            denials: vec![],
            action_audit: vec![],
            iterations: 1,
            model_used: "mock".to_string(),
            blast_radius: None,
            qa_summary: Default::default(),
            budget: Default::default(),
        }
    }

    fn completed(files: &[&str], effect: Option<EffectClass>) -> TaskStatus {
        TaskStatus::Completed {
            finished_at: Utc::now(),
            result: result(files, effect),
        }
    }

    fn completed_with_denials() -> TaskStatus {
        let mut done = result(&[], None);
        done.denials = vec![denial(), denial()];
        TaskStatus::Completed {
            finished_at: Utc::now(),
            result: done,
        }
    }

    fn failed_with(calls: Vec<ToolCallRecord>) -> TaskStatus {
        TaskStatus::Failed {
            finished_at: Utc::now(),
            error: "boom".to_string(),
            diagnostics: FailureDiagnostics {
                error_type: "E".to_string(),
                iterations_completed: 0,
                last_tool_call: None,
                partial_changes: None,
                tool_call_history: calls,
                last_agent_state: None,
                conversation_snapshot: None,
                denials: vec![denial()],
                action_audit: vec![],
            },
        }
    }

    fn failed() -> TaskStatus {
        failed_with(vec![])
    }

    fn cancelled() -> TaskStatus {
        TaskStatus::Cancelled {
            finished_at: Utc::now(),
            iterations_completed: 0,
        }
    }

    fn kinds(run: &PlanRun) -> Vec<(String, &'static str)> {
        run.children()
            .map(|(id, outcome)| {
                let kind = match outcome {
                    ChildOutcome::Waiting => "waiting",
                    ChildOutcome::Starting => "starting",
                    ChildOutcome::Abandoned => "abandoned",
                    ChildOutcome::Running(_) => "running",
                    ChildOutcome::Completed { .. } => "completed",
                    ChildOutcome::Failed { .. } => "failed",
                    ChildOutcome::Cancelled(_) => "cancelled",
                    ChildOutcome::Refused(_) => "refused",
                    ChildOutcome::Lost(_) => "lost",
                    ChildOutcome::Skipped { .. } => "skipped",
                };
                (id.to_string(), kind)
            })
            .collect()
    }

    fn kind_of(run: &PlanRun, id: &str) -> &'static str {
        kinds(run).into_iter().find(|(n, _)| n == id).unwrap().1
    }

    fn blocker(run: &PlanRun, id: &str) -> Option<String> {
        let found = run.children().find(|(n, _)| *n == id);
        found.and_then(|(_, outcome)| match outcome {
            ChildOutcome::Skipped { blocked_by } => Some(blocked_by.to_string()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn only_nodes_without_dependencies_start_first_and_their_ids_are_tracked() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        assert!(run.child_ids().is_empty());
        run.advance(&fake).await;
        assert_eq!(fake.identities(), ["rust-implementer"]);
        let expected = [
            ("fix".into(), "running"),
            ("shepherd".into(), "waiting"),
            ("ship".into(), "waiting"),
        ];
        assert_eq!(kinds(&run), expected);
        assert_eq!(run.child_ids(), [("fix", &fake.id_of("rust-implementer"))]);
        assert!(!run.is_finished());
    }

    #[tokio::test]
    async fn a_dependent_starts_only_after_its_dependency_completed_and_nothing_starts_twice() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        for _ in 0..3 {
            run.advance(&fake).await;
        }
        assert_eq!(fake.identities(), ["rust-implementer"]);
        fake.set("rust-implementer", completed(&["a.rs"], None));
        run.advance(&fake).await;
        run.advance(&fake).await;
        assert_eq!(fake.identities(), ["rust-implementer", "pr-shepherd"]);
        fake.set("pr-shepherd", completed(&["b.rs"], None));
        run.advance(&fake).await;
        let all = ["rust-implementer", "pr-shepherd", "deployer"];
        assert_eq!(fake.identities(), all);
    }

    #[tokio::test]
    async fn each_node_is_started_with_exactly_the_audited_request() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        run.advance(&fake).await;
        let started = fake.started.lock().unwrap();
        assert_eq!(started[0].0, "rust-implementer");
        assert_eq!(started[0].1, "Fix bug X.");
    }

    #[tokio::test]
    async fn a_failed_dependency_skips_every_dependent_and_none_is_started() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        run.advance(&fake).await;
        fake.set("rust-implementer", failed());
        run.advance(&fake).await;
        assert_eq!(kind_of(&run, "fix"), "failed");
        assert_eq!(blocker(&run, "fix"), None);
        assert_eq!(blocker(&run, "shepherd"), Some("fix".to_string()));
        assert_eq!(blocker(&run, "ship"), Some("shepherd".to_string()));
        assert_eq!(fake.identities(), ["rust-implementer"]);
        assert!(run.is_finished());
        let summary = run.summary();
        let counts = (summary.failed, summary.skipped, summary.succeeded);
        assert_eq!(counts, (1, 2, false));
    }

    #[tokio::test]
    async fn a_cancelled_or_lost_dependency_also_blocks_its_dependents() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        run.advance(&fake).await;
        fake.set("rust-implementer", cancelled());
        run.advance(&fake).await;
        assert_eq!(kind_of(&run, "fix"), "cancelled");
        assert_eq!(kind_of(&run, "shepherd"), "skipped");
        assert!(run.summary().effects_may_be_understated);
        let (mut lost_run, lost_fake) = (ship_run().await, Fake::new());
        lost_run.advance(&lost_fake).await;
        lost_fake.statuses.lock().unwrap().clear();
        lost_run.advance(&lost_fake).await;
        assert_eq!(kind_of(&lost_run, "fix"), "lost");
        assert_eq!(kind_of(&lost_run, "ship"), "skipped");
        assert_eq!(lost_fake.identities(), ["rust-implementer"]);
    }

    #[tokio::test]
    async fn a_refused_start_blocks_dependents_and_keeps_the_refusal() {
        let mut fake = Fake::new();
        fake.refuse = Some("pr-shepherd");
        let mut run = ship_run().await;
        run.advance(&fake).await;
        fake.set("rust-implementer", completed(&[], None));
        run.advance(&fake).await;
        assert_eq!(kind_of(&run, "shepherd"), "refused");
        assert_eq!(kind_of(&run, "ship"), "skipped");
        assert!(run.child_ids().iter().all(|(id, _)| *id == "fix"));
        assert!(run.is_finished());
        let summary = run.summary();
        assert!(!summary.succeeded);
        assert_eq!((summary.failed, summary.skipped), (1, 1));
    }

    #[tokio::test]
    async fn the_summary_aggregates_results_and_the_widest_effect_class() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        run.advance(&fake).await;
        let first = completed(&["b.rs", "a.rs"], Some(EffectClass::Workspace));
        fake.set("rust-implementer", first);
        run.advance(&fake).await;
        fake.set("pr-shepherd", completed(&["a.rs"], Some(EffectClass::Ci)));
        run.advance(&fake).await;
        fake.set("deployer", completed(&[], None));
        run.advance(&fake).await;
        let summary = run.summary();
        assert!(summary.succeeded && run.is_finished());
        assert!(!summary.effects_may_be_understated);
        let counts = (summary.completed, summary.failed, summary.skipped);
        assert_eq!((counts, summary.unfinished), ((3, 0, 0), 0));
        assert_eq!(summary.max_effect, Some(EffectClass::Ci));
        assert_eq!(summary.files_modified, ["a.rs", "b.rs"]);
        assert_eq!((summary.denials, summary.tool_calls), (0, 2));
    }

    #[tokio::test]
    async fn completed_denials_are_summed() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        run.advance(&fake).await;
        fake.set("rust-implementer", completed_with_denials());
        run.advance(&fake).await;
        assert_eq!(run.summary().denials, 2);
    }

    #[tokio::test]
    async fn a_failed_child_still_reports_the_effects_denials_and_calls_it_made() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        run.advance(&fake).await;
        let calls = vec![call(EffectClass::Workspace), call(EffectClass::Production)];
        fake.set("rust-implementer", failed_with(calls));
        run.advance(&fake).await;
        let summary = run.summary();
        assert_eq!(summary.max_effect, Some(EffectClass::Production));
        assert_eq!((summary.denials, summary.tool_calls), (1, 2));
        assert_eq!((summary.failed, summary.completed), (1, 0));
        assert!(!summary.effects_may_be_understated);
    }

    #[tokio::test(start_paused = true)]
    async fn run_drives_the_whole_plan_in_dependency_order() {
        let mut fake = Fake::new();
        fake.auto_complete = true;
        let mut run = ship_run().await;
        let (poll, budget) = (Duration::from_millis(10), Duration::from_secs(5));
        assert!(run.run(&fake, poll, budget).await && run.summary().succeeded);
        let all = ["rust-implementer", "pr-shepherd", "deployer"];
        assert_eq!(fake.identities(), all);
    }

    #[tokio::test(start_paused = true)]
    async fn giving_up_cancels_running_children_skips_waiting_ones_and_starts_nothing_more() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        let (poll, budget) = (Duration::from_millis(10), Duration::from_millis(100));
        assert!(!run.run(&fake, poll, budget).await);
        assert!(run.is_finished());
        assert_eq!(kind_of(&run, "fix"), "cancelled");
        assert_eq!(blocker(&run, "shepherd"), Some("budget".to_string()));
        assert_eq!(blocker(&run, "ship"), Some("budget".to_string()));
        assert_eq!(
            *fake.cancelled.lock().unwrap(),
            [fake.id_of("rust-implementer")]
        );
        run.advance(&fake).await;
        assert_eq!(fake.identities(), ["rust-implementer"]);
        let summary = run.summary();
        assert!(summary.effects_may_be_understated && !summary.succeeded);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_start_past_the_budget_keeps_the_child_tracked_and_never_skips_it() {
        let mut fake = Fake::new();
        fake.start_delay = Duration::from_secs(10);
        let mut run = ship_run().await;
        let (poll, budget) = (Duration::from_millis(100), Duration::from_secs(1));
        assert!(!run.run(&fake, poll, budget).await);
        assert_eq!(run.child_ids().len(), 1);
        assert_eq!(kind_of(&run, "fix"), "cancelled");
        assert_eq!(fake.identities(), ["rust-implementer"]);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_an_advance_mid_start_leaves_the_node_starting_not_skipped() {
        let mut fake = Fake::new();
        fake.start_delay = Duration::from_secs(10);
        let mut run = ship_run().await;
        let cut = tokio::time::timeout(Duration::from_secs(1), run.advance(&fake)).await;
        assert!(cut.is_err());
        assert_eq!(kind_of(&run, "fix"), "starting");
        assert_eq!(kind_of(&run, "shepherd"), "waiting");
        assert!(!run.is_finished());
        assert!(run.child_ids().is_empty());
        assert!(run.summary().effects_may_be_understated);
        run.advance(&fake).await;
        assert_eq!(kind_of(&run, "fix"), "starting");
        assert_eq!(kind_of(&run, "shepherd"), "waiting");
        assert_eq!(fake.identities(), ["rust-implementer"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_single_node_dropped_mid_start_is_not_finished() {
        let mut fake = Fake::new();
        fake.start_delay = Duration::from_secs(10);
        let nodes = vec![node(
            "fix",
            "rust-implementer",
            DevLoop::Inner,
            "Fix bug X.",
            &[],
        )];
        let pairs = approved(nodes).await;
        let mut run = PlanRun::new(pairs, target(std::path::Path::new("/nonexistent"))).unwrap();
        let cut = tokio::time::timeout(Duration::from_secs(1), run.advance(&fake)).await;
        assert!(cut.is_err());
        assert_eq!(kind_of(&run, "fix"), "starting");
        assert!(!run.is_finished());
    }

    #[tokio::test(start_paused = true)]
    async fn giving_up_settles_a_node_left_starting_as_abandoned_and_skips_its_dependents() {
        let mut fake = Fake::new();
        fake.start_delay = Duration::from_secs(10);
        let mut run = ship_run().await;
        let cut = tokio::time::timeout(Duration::from_secs(1), run.advance(&fake)).await;
        assert!(cut.is_err());
        let (poll, budget) = (Duration::from_millis(10), Duration::from_millis(100));
        assert!(!run.run(&fake, poll, budget).await);
        assert!(run.is_finished());
        assert_eq!(kind_of(&run, "fix"), "abandoned");
        assert_eq!(blocker(&run, "shepherd"), Some("budget".to_string()));
        assert_eq!(blocker(&run, "ship"), Some("budget".to_string()));
        assert_eq!(fake.identities(), ["rust-implementer"]);
        let summary = run.summary();
        assert!(summary.effects_may_be_understated && !summary.succeeded);
        assert_eq!(
            (summary.failed, summary.skipped, summary.unfinished),
            (1, 2, 0)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_an_advance_mid_poll_leaves_the_child_running_and_tracked() {
        let mut fake = Fake::new();
        fake.status_delay = Duration::from_secs(10);
        let mut run = ship_run().await;
        run.advance(&fake).await;
        let cut = tokio::time::timeout(Duration::from_secs(1), run.advance(&fake)).await;
        assert!(cut.is_err());
        assert_eq!(kind_of(&run, "fix"), "running");
        assert_eq!(run.child_ids().len(), 1);
    }

    async fn rejected(pairs: Vec<(Spawn, Allowed)>) -> PlanRunError {
        let outcome = PlanRun::new(pairs, target(std::path::Path::new("/nonexistent")));
        outcome.err().unwrap()
    }

    #[tokio::test]
    async fn a_plan_run_refuses_a_malformed_approved_set() {
        let mut pairs = approved(ship_nodes()).await;
        pairs[1].0.id = "fix".to_string();
        let error = rejected(pairs).await;
        assert!(matches!(
            error,
            PlanRunError::Plan(PlanRejection::DuplicateId { .. })
        ));
        assert!(error.to_string().contains("not a valid DAG"));

        let mut pairs = approved(ship_nodes()).await;
        pairs[1].0.depends_on.push("ghost".to_string());
        let error = rejected(pairs).await;
        assert!(matches!(
            error,
            PlanRunError::Plan(PlanRejection::DanglingDependency { .. })
        ));

        let mut pairs = approved(ship_nodes()).await;
        pairs[0].0.depends_on.push("fix".to_string());
        let error = rejected(pairs).await;
        assert!(matches!(
            error,
            PlanRunError::Plan(PlanRejection::SelfDependency { .. })
        ));

        let mut pairs = approved(ship_nodes()).await;
        pairs[0].0.depends_on.push("ship".to_string());
        let error = rejected(pairs).await;
        assert!(matches!(
            error,
            PlanRunError::Plan(PlanRejection::Cycle { .. })
        ));

        let error = rejected(vec![]).await;
        assert!(matches!(
            error,
            PlanRunError::Plan(PlanRejection::EmptyPlan)
        ));
    }

    #[tokio::test]
    async fn a_proof_that_does_not_belong_to_its_node_is_refused() {
        let mut pairs = approved(ship_nodes()).await;
        pairs[0].0.subtask = "Something the auditor never saw.".to_string();
        let error = rejected(pairs).await;
        assert!(matches!(&error, PlanRunError::ProofMismatch { id } if id == "fix"));
        assert!(error.to_string().contains("does not match"));
        let mut pairs = approved(ship_nodes()).await;
        pairs[2].0.identity = "rust-implementer".to_string();
        let error = rejected(pairs).await;
        assert!(matches!(&error, PlanRunError::ProofMismatch { id } if id == "ship"));
    }

    #[tokio::test]
    async fn children_are_ordered_by_dependency_whatever_order_the_proofs_arrive_in() {
        let mut pairs = approved(ship_nodes()).await;
        pairs.reverse();
        let run = PlanRun::new(pairs, target(std::path::Path::new("/nonexistent"))).unwrap();
        let ids: Vec<String> = kinds(&run).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["fix", "shepherd", "ship"]);
    }

    #[tokio::test]
    async fn the_stub_provider_describes_itself() {
        let provider = NoModel;
        assert!(provider.list_models().await.unwrap().is_empty());
        provider.health_check().await.unwrap();
        assert_eq!(provider.provider_name(), "none");
        let refused = provider.chat(ChatRequest::new("m", vec![])).await;
        assert!(matches!(refused, Err(ModelError::RateLimit)));
    }

    #[tokio::test]
    async fn the_task_manager_runner_binds_the_audited_identity_before_the_task_can_run() {
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let manager = TaskManager::new(0);
        let nodes = vec![node(
            "fix",
            "rust-implementer",
            DevLoop::Inner,
            "Fix bug X.",
            &[],
        )];
        let mut run = PlanRun::new(approved(nodes).await, target(repo.path())).unwrap();
        run.advance(&manager).await;
        let ids = run.child_ids();
        assert_eq!(ids.len(), 1);
        let (audited, identity) = manager.bound_identity_of(ids[0].1).await.unwrap();
        assert!(audited);
        assert_eq!(identity.name(), "rust-implementer");
        let status = manager.status(ids[0].1).await.unwrap();
        assert!(matches!(status, TaskStatus::Pending));
        assert!(manager.status(&TaskId::new()).await.is_none());
    }

    #[tokio::test]
    async fn the_task_manager_runner_cancels_through_the_existing_cancel_path() {
        let repo = repo_with_origin("https://github.com/example/repo.git");
        let manager = TaskManager::new(0);
        let nodes = vec![node(
            "fix",
            "rust-implementer",
            DevLoop::Inner,
            "Fix bug X.",
            &[],
        )];
        let mut run = PlanRun::new(approved(nodes).await, target(repo.path())).unwrap();
        let (poll, budget) = (Duration::from_millis(5), Duration::from_millis(20));
        assert!(!run.run(&manager, poll, budget).await);
        assert_eq!(kind_of(&run, "fix"), "cancelled");
        let id = run.child_ids()[0].1.clone();
        assert!(manager.bound_identity_of(&id).await.is_none());
    }

    #[tokio::test]
    async fn the_task_manager_runner_refuses_a_repository_other_than_the_audited_one() {
        let other = repo_with_origin("https://github.com/example/other.git");
        let manager = TaskManager::new(0);
        let nodes = vec![node(
            "fix",
            "rust-implementer",
            DevLoop::Inner,
            "Fix bug X.",
            &[],
        )];
        let mut run = PlanRun::new(approved(nodes).await, target(other.path())).unwrap();
        run.advance(&manager).await;
        assert_eq!(kind_of(&run, "fix"), "refused");
        assert!(run.child_ids().is_empty());
        assert!(manager.list().await.is_empty());
    }
}
