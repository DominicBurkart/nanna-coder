use crate::auditor::Allowed;
use crate::effects::EffectClass;
use crate::planner::Spawn;
use crate::task::{SpawnRefused, TaskId, TaskManager, TaskResult, TaskStatus};
use async_trait::async_trait;
use model::ModelProvider;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub struct DispatchTarget {
    pub repo_path: PathBuf,
    pub branch: String,
    pub max_iterations: usize,
    pub provider: Arc<dyn ModelProvider>,
}

#[async_trait]
pub trait ChildRunner: Send + Sync {
    async fn start(&self, proof: Allowed, target: &DispatchTarget) -> Result<TaskId, SpawnRefused>;

    async fn status(&self, id: &TaskId) -> Option<TaskStatus>;
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
}

#[derive(Debug)]
enum ChildState {
    Waiting(Box<Allowed>),
    Running(TaskId),
    Completed {
        task: TaskId,
        result: Box<TaskResult>,
    },
    Failed {
        task: TaskId,
        error: String,
    },
    Cancelled(TaskId),
    Refused(SpawnRefused),
    Lost(TaskId),
    Skipped {
        blocked_by: String,
    },
}

#[derive(Debug)]
pub enum ChildOutcome<'a> {
    Waiting,
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
            ChildState::Waiting(_) => ChildOutcome::Waiting,
            ChildState::Running(task) => ChildOutcome::Running(task),
            ChildState::Completed { task, result } => ChildOutcome::Completed { task, result },
            ChildState::Failed { task, error } => ChildOutcome::Failed { task, error },
            ChildState::Cancelled(task) => ChildOutcome::Cancelled(task),
            ChildState::Refused(refusal) => ChildOutcome::Refused(refusal),
            ChildState::Lost(task) => ChildOutcome::Lost(task),
            ChildState::Skipped { blocked_by } => ChildOutcome::Skipped { blocked_by },
        }
    }
}

enum Readiness {
    Ready,
    NotYet,
    Blocked(String),
}

pub struct PlanRun {
    children: Vec<Child>,
    target: DispatchTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSummary {
    pub completed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub unfinished: usize,
    pub succeeded: bool,
    pub max_effect: Option<EffectClass>,
    pub files_modified: Vec<String>,
    pub denials: usize,
    pub tool_calls: usize,
}

impl PlanRun {
    pub fn new(approved: Vec<(Spawn, Allowed)>, target: DispatchTarget) -> Self {
        let children = approved
            .into_iter()
            .map(|(spawn, proof)| Child {
                spawn,
                state: ChildState::Waiting(Box::new(proof)),
            })
            .collect();
        Self { children, target }
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
        let waiting = |child: &Child| matches!(child.state, ChildState::Waiting(_));
        let running = |child: &Child| matches!(child.state, ChildState::Running(_));
        !self
            .children
            .iter()
            .any(|child| waiting(child) || running(child))
    }

    fn gate(&self, index: usize) -> Readiness {
        let mut pending = false;
        for dependency in &self.children[index].spawn.depends_on {
            let state = self
                .children
                .iter()
                .find(|c| c.spawn.id == *dependency)
                .map(|c| &c.state);
            match state {
                Some(ChildState::Completed { .. }) => {}
                Some(ChildState::Waiting(_) | ChildState::Running(_)) => pending = true,
                _ => return Readiness::Blocked(dependency.clone()),
            }
        }
        if pending {
            Readiness::NotYet
        } else {
            Readiness::Ready
        }
    }

    pub async fn advance(&mut self, runner: &dyn ChildRunner) {
        for index in 0..self.children.len() {
            let gate = self.gate(index);
            let placeholder = ChildState::Skipped {
                blocked_by: String::new(),
            };
            let state = std::mem::replace(&mut self.children[index].state, placeholder);
            self.children[index].state = step(state, gate, runner, &self.target).await;
        }
    }

    pub async fn run(
        &mut self,
        runner: &dyn ChildRunner,
        poll_every: Duration,
        budget: Duration,
    ) -> bool {
        let drive = async {
            while !self.is_finished() {
                self.advance(runner).await;
                tokio::time::sleep(poll_every).await;
            }
        };
        tokio::time::timeout(budget, drive).await.is_ok()
    }

    pub fn summary(&self) -> PlanSummary {
        let count = |wanted: fn(&ChildState) -> bool| {
            self.children.iter().filter(|c| wanted(&c.state)).count()
        };
        let results: Vec<&TaskResult> = self
            .children
            .iter()
            .filter_map(|child| match &child.state {
                ChildState::Completed { result, .. } => Some(result.as_ref()),
                _ => None,
            })
            .collect();
        let files: BTreeSet<&String> = results
            .iter()
            .flat_map(|r| r.files_modified.iter())
            .collect();
        let completed = results.len();
        PlanSummary {
            completed,
            failed: count(|s| {
                matches!(
                    s,
                    ChildState::Failed { .. }
                        | ChildState::Cancelled(_)
                        | ChildState::Refused(_)
                        | ChildState::Lost(_)
                )
            }),
            skipped: count(|s| matches!(s, ChildState::Skipped { .. })),
            unfinished: count(|s| matches!(s, ChildState::Waiting(_) | ChildState::Running(_))),
            succeeded: completed == self.children.len(),
            max_effect: results.iter().filter_map(|r| r.max_effect_class()).max(),
            files_modified: files.into_iter().cloned().collect(),
            denials: results.iter().map(|r| r.denial_count()).sum(),
            tool_calls: results.iter().map(|r| r.tool_calls_made.len()).sum(),
        }
    }
}

async fn step(
    state: ChildState,
    gate: Readiness,
    runner: &dyn ChildRunner,
    target: &DispatchTarget,
) -> ChildState {
    match (state, gate) {
        (ChildState::Waiting(_), Readiness::Blocked(blocked_by)) => {
            ChildState::Skipped { blocked_by }
        }
        (ChildState::Waiting(proof), Readiness::NotYet) => ChildState::Waiting(proof),
        (ChildState::Waiting(proof), Readiness::Ready) => {
            match runner.start(*proof, target).await {
                Ok(task) => ChildState::Running(task),
                Err(refusal) => ChildState::Refused(refusal),
            }
        }
        (ChildState::Running(task), _) => match runner.status(&task).await {
            Some(TaskStatus::Completed { result, .. }) => ChildState::Completed {
                task,
                result: Box::new(result),
            },
            Some(TaskStatus::Failed { error, .. }) => ChildState::Failed { task, error },
            Some(TaskStatus::Cancelled { .. }) => ChildState::Cancelled(task),
            Some(TaskStatus::Pending | TaskStatus::Running { .. }) => ChildState::Running(task),
            None => ChildState::Lost(task),
        },
        (settled, _) => settled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::test_support::{fixture_gate, repo_with_origin};
    use crate::auditor::TaskSummary;
    use crate::effects::EffectRecord;
    use crate::entities::context::types::ToolCallRecord;
    use crate::identity::DevLoop;
    use crate::planner::{audit_plan, Plan};
    use crate::task::FailureDiagnostics;
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
        PlanRun::new(
            approved(ship_nodes()).await,
            target(std::path::Path::new("/nonexistent")),
        )
    }

    struct Fake {
        started: Mutex<Vec<(String, String, TaskId)>>,
        statuses: Mutex<HashMap<TaskId, TaskStatus>>,
        refuse: Option<&'static str>,
        auto_complete: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                started: Mutex::new(Vec::new()),
                statuses: Mutex::new(HashMap::new()),
                refuse: None,
                auto_complete: false,
            }
        }

        fn identities(&self) -> Vec<String> {
            self.started
                .lock()
                .unwrap()
                .iter()
                .map(|s| s.0.clone())
                .collect()
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
            if self.refuse == Some(request.identity.as_str()) {
                return Err(SpawnRefused::ModelMismatch {
                    audited: "a".to_string(),
                    requested: "b".to_string(),
                });
            }
            let id = TaskId::new();
            let status = if self.auto_complete {
                completed(&["a.rs"], None)
            } else {
                TaskStatus::Pending
            };
            self.statuses.lock().unwrap().insert(id.clone(), status);
            let entry = (
                request.identity.clone(),
                request.subtask.clone(),
                id.clone(),
            );
            self.started.lock().unwrap().push(entry);
            Ok(id)
        }

        async fn status(&self, id: &TaskId) -> Option<TaskStatus> {
            self.statuses.lock().unwrap().get(id).cloned()
        }
    }

    fn result(files: &[&str], effect: Option<EffectClass>) -> TaskResult {
        let calls = effect.map(|class| ToolCallRecord {
            tool_name: "t".to_string(),
            arguments: serde_json::json!({}),
            call_id: "c".to_string(),
            result: String::new(),
            effect: Some(EffectRecord::new(class)),
        });
        TaskResult {
            result_summary: "done".to_string(),
            changes_patch: None,
            format_patch: None,
            files_modified: files.iter().map(|f| f.to_string()).collect(),
            tool_calls_made: calls.into_iter().collect(),
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

    fn failed() -> TaskStatus {
        TaskStatus::Failed {
            finished_at: Utc::now(),
            error: "boom".to_string(),
            diagnostics: FailureDiagnostics {
                error_type: "E".to_string(),
                iterations_completed: 0,
                last_tool_call: None,
                partial_changes: None,
                tool_call_history: vec![],
                last_agent_state: None,
                conversation_snapshot: None,
                denials: vec![],
                action_audit: vec![],
            },
        }
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
        run.children()
            .find(|(n, _)| *n == id)
            .and_then(|(_, outcome)| match outcome {
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
        assert_eq!(
            kinds(&run),
            [
                ("fix".into(), "running"),
                ("shepherd".into(), "waiting"),
                ("ship".into(), "waiting")
            ]
        );
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
        assert_eq!(
            fake.identities(),
            ["rust-implementer", "pr-shepherd", "deployer"]
        );
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
        assert_eq!(
            (summary.failed, summary.skipped, summary.succeeded),
            (1, 2, false)
        );
    }

    #[tokio::test]
    async fn a_cancelled_or_lost_dependency_also_blocks_its_dependents() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        run.advance(&fake).await;
        fake.set("rust-implementer", cancelled());
        run.advance(&fake).await;
        assert_eq!(kind_of(&run, "fix"), "cancelled");
        assert_eq!(kind_of(&run, "shepherd"), "skipped");
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
        assert!(!run.summary().succeeded);
    }

    #[tokio::test]
    async fn the_summary_aggregates_results_and_the_widest_effect_class() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        run.advance(&fake).await;
        fake.set(
            "rust-implementer",
            completed(&["b.rs", "a.rs"], Some(EffectClass::Workspace)),
        );
        run.advance(&fake).await;
        fake.set("pr-shepherd", completed(&["a.rs"], Some(EffectClass::Ci)));
        run.advance(&fake).await;
        fake.set("deployer", completed(&[], None));
        run.advance(&fake).await;
        let summary = run.summary();
        assert!(summary.succeeded && run.is_finished());
        assert_eq!(
            (
                summary.completed,
                summary.failed,
                summary.skipped,
                summary.unfinished
            ),
            (3, 0, 0, 0)
        );
        assert_eq!(summary.max_effect, Some(EffectClass::Ci));
        assert_eq!(summary.files_modified, ["a.rs", "b.rs"]);
        assert_eq!((summary.denials, summary.tool_calls), (0, 2));
    }

    #[tokio::test(start_paused = true)]
    async fn run_drives_the_whole_plan_in_dependency_order() {
        let mut fake = Fake::new();
        fake.auto_complete = true;
        let mut run = ship_run().await;
        let finished = run
            .run(&fake, Duration::from_millis(10), Duration::from_secs(5))
            .await;
        assert!(finished && run.summary().succeeded);
        assert_eq!(
            fake.identities(),
            ["rust-implementer", "pr-shepherd", "deployer"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn run_gives_up_at_its_budget_with_the_plan_unfinished() {
        let (mut run, fake) = (ship_run().await, Fake::new());
        let finished = run
            .run(&fake, Duration::from_millis(10), Duration::from_millis(100))
            .await;
        assert!(!finished && !run.is_finished());
        assert_eq!(run.summary().unfinished, 3);
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
        let mut run = PlanRun::new(approved(nodes).await, target(repo.path()));
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
        let mut run = PlanRun::new(approved(nodes).await, target(other.path()));
        run.advance(&manager).await;
        assert_eq!(kind_of(&run, "fix"), "refused");
        assert!(run.child_ids().is_empty());
        assert!(manager.list().await.is_empty());
    }
}
