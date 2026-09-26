//! Auditing every planned spawn before dispatch, and tracking the resulting
//! child tasks.

use super::{Plan, PlanError, PlanNodeId, SpawnNode};
use crate::auditor::{
    Allowed, AuditContext, Auditor, CardSuggestion, Gate, Reason, Refused, SpawnEscalationHook,
    SpawnRequest, SpawnVerdict, TaskSummary,
};
use crate::effects::EffectClass;
use crate::task::{TaskId, TaskManager, TaskStatus};
use async_trait::async_trait;
use model::ModelProvider;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What became of one [`SpawnNode`] when its [`Plan`] was executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeOutcome {
    /// The auditor allowed the spawn and it was submitted to the task
    /// manager as `task_id`.
    Dispatched {
        /// The child task the spawn was submitted as.
        task_id: TaskId,
        /// The effect the spawn was audited and dispatched at.
        effect: EffectClass,
    },
    /// The auditor blocked the spawn; it never reached the task manager.
    Blocked {
        /// Why.
        reasons: Vec<Reason>,
    },
    /// No identity in the catalog fit the spawn; a human should decide.
    Escalated {
        /// Why.
        reasons: Vec<Reason>,
        /// What a fitting card would look like.
        suggested_identity_change: CardSuggestion,
    },
    /// The auditor itself failed to produce a verdict.
    AuditFailed {
        /// The rendered audit failure.
        detail: String,
    },
    /// A dependency did not dispatch, or did not complete, so this node was
    /// never audited or dispatched.
    Skipped {
        /// The dependency whose failure caused the skip.
        blocked_dependency: PlanNodeId,
    },
}

/// The result of running every node of a [`Plan`] through the auditor and
/// dispatching the ones it allowed.
#[derive(Debug, Clone, Default)]
pub struct PlanExecution {
    outcomes: Vec<(PlanNodeId, NodeOutcome)>,
}

impl PlanExecution {
    /// The outcome recorded for `id`, if the plan had a node with that id.
    pub fn outcome(&self, id: &PlanNodeId) -> Option<&NodeOutcome> {
        self.outcomes
            .iter()
            .find(|(node_id, _)| node_id == id)
            .map(|(_, outcome)| outcome)
    }

    /// Every node's outcome, in the order nodes were executed
    /// (dependencies before dependents).
    pub fn outcomes(&self) -> &[(PlanNodeId, NodeOutcome)] {
        &self.outcomes
    }

    /// The child [`TaskId`]s of every dispatched node, in execution order.
    /// A caller polls or awaits each with `TaskManager::poll` /
    /// `TaskManager::get_result` / `TaskManager::wait_terminal`.
    pub fn task_ids(&self) -> Vec<&TaskId> {
        self.outcomes
            .iter()
            .filter_map(|(_, outcome)| match outcome {
                NodeOutcome::Dispatched { task_id, .. } => Some(task_id),
                _ => None,
            })
            .collect()
    }

    /// The widest [`EffectClass`] any dispatched node was audited and
    /// dispatched at, or `None` when nothing was dispatched.
    ///
    /// This is the requested effect each dispatched spawn was allowed
    /// under, known synchronously at dispatch time; a fuller rollup that
    /// waits for every child [`crate::task::TaskResult`] and reports the
    /// effect it actually observed is a natural extension once callers
    /// need it.
    pub fn aggregate_effect_class(&self) -> Option<EffectClass> {
        self.outcomes
            .iter()
            .filter_map(|(_, outcome)| match outcome {
                NodeOutcome::Dispatched { effect, .. } => Some(*effect),
                _ => None,
            })
            .max()
    }
}

/// The entry point a [`Plan`] execution uses to submit an audited spawn and
/// to learn whether a dependency finished. Implemented for
/// [`TaskManager`](crate::task::TaskManager); a test double may implement it
/// directly to assert that a blocked spawn never reaches dispatch.
#[async_trait]
pub trait SpawnDispatcher: Send + Sync {
    /// Submit `allowed` as a new task and return its id.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch(
        &self,
        allowed: Allowed,
        repo_path: PathBuf,
        branch: String,
        model: String,
        max_iterations: usize,
        provider: Arc<dyn ModelProvider>,
    ) -> TaskId;

    /// Await `task_id`'s terminal status, or `None` if it is unknown.
    async fn wait_terminal(&self, task_id: &TaskId) -> Option<TaskStatus>;
}

#[async_trait]
impl SpawnDispatcher for TaskManager {
    async fn dispatch(
        &self,
        allowed: Allowed,
        repo_path: PathBuf,
        branch: String,
        model: String,
        max_iterations: usize,
        provider: Arc<dyn ModelProvider>,
    ) -> TaskId {
        self.submit_spawn(allowed, repo_path, branch, model, max_iterations, provider)
            .await
    }

    async fn wait_terminal(&self, task_id: &TaskId) -> Option<TaskStatus> {
        TaskManager::wait_terminal(self, task_id).await
    }
}

/// Run every node of `plan`, in dependency order, through `gate`, and
/// dispatch each `Allow`ed node via `dispatcher`.
///
/// A node whose dependency was blocked, escalated, failed to audit, or
/// itself skipped is skipped in turn without being audited or dispatched
/// (issue #640's recommended policy: a failure never silently lets its
/// dependents run). A node whose dependency dispatched but did not
/// terminate as `Completed` is skipped as well, since a dependent's subtask
/// text typically assumes the dependency's work landed.
///
/// The only failure mode for the execution itself is a [`PlanError::Cycle`]
/// from [`Plan::topological_order`]; every node's own audit/dispatch result
/// is instead recorded as a [`NodeOutcome`] in the returned
/// [`PlanExecution`].
///
/// `model` and `max_iterations` for a dispatched node are taken from the
/// identity the auditor allowed the spawn against (`Allowed::identity`),
/// not from a caller argument, so the identity that was audited is exactly
/// the one that runs (see issue #690: a proof must bind the terms it was
/// audited under).
#[allow(clippy::too_many_arguments)]
pub async fn execute_plan<A, H, D>(
    plan: &Plan,
    gate: &Gate<A, H>,
    context: &AuditContext,
    dispatcher: &D,
    parent_task: &TaskSummary,
    repo_path: &Path,
    branch: &str,
    provider: Arc<dyn ModelProvider>,
) -> Result<PlanExecution, PlanError>
where
    A: Auditor,
    H: SpawnEscalationHook,
    D: SpawnDispatcher,
{
    let order = plan.topological_order()?;
    let mut execution = PlanExecution::default();
    for id in order {
        let node = plan
            .node(&id)
            .expect("topological_order only returns known ids");
        let blocked_dependency =
            first_failed_dependency(&execution.outcomes, node, dispatcher).await;
        let outcome = match blocked_dependency {
            Some(blocked_dependency) => NodeOutcome::Skipped { blocked_dependency },
            None => {
                run_node(
                    node,
                    gate,
                    context,
                    dispatcher,
                    parent_task,
                    repo_path,
                    branch,
                    Arc::clone(&provider),
                )
                .await
            }
        };
        execution.outcomes.push((id, outcome));
    }
    Ok(execution)
}

async fn first_failed_dependency<D: SpawnDispatcher>(
    outcomes: &[(PlanNodeId, NodeOutcome)],
    node: &SpawnNode,
    dispatcher: &D,
) -> Option<PlanNodeId> {
    for dep in &node.depends_on {
        if dependency_failed(outcomes, dep, dispatcher).await {
            return Some(dep.clone());
        }
    }
    None
}

async fn dependency_failed<D: SpawnDispatcher>(
    outcomes: &[(PlanNodeId, NodeOutcome)],
    dep: &PlanNodeId,
    dispatcher: &D,
) -> bool {
    let recorded = outcomes
        .iter()
        .find(|(id, _)| id == dep)
        .map(|(_, outcome)| outcome);
    match recorded {
        Some(NodeOutcome::Dispatched { task_id, .. }) => !matches!(
            dispatcher.wait_terminal(task_id).await,
            Some(TaskStatus::Completed { .. })
        ),
        _ => true,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_node<A: Auditor, H: SpawnEscalationHook, D: SpawnDispatcher>(
    node: &SpawnNode,
    gate: &Gate<A, H>,
    context: &AuditContext,
    dispatcher: &D,
    parent_task: &TaskSummary,
    repo_path: &Path,
    branch: &str,
    provider: Arc<dyn ModelProvider>,
) -> NodeOutcome {
    let requested_effect = context
        .catalog()
        .get(&node.identity)
        .map(|identity| identity.scope.max_effect)
        .unwrap_or(EffectClass::None);
    let request = SpawnRequest {
        parent_task: parent_task.clone(),
        identity: node.identity.clone(),
        subtask: node.subtask.clone(),
        dev_loop: node.dev_loop,
        requested_effect,
    };
    match gate.check(request, context).await {
        Ok(allowed) => {
            let model = allowed.identity().identity.model.clone();
            let max_iterations = allowed.identity().limits.max_iterations;
            let task_id = dispatcher
                .dispatch(
                    allowed,
                    repo_path.to_path_buf(),
                    branch.to_string(),
                    model,
                    max_iterations,
                    provider,
                )
                .await;
            NodeOutcome::Dispatched {
                task_id,
                effect: requested_effect,
            }
        }
        Err(Refused::Verdict { verdict, .. }) => {
            if let SpawnVerdict::Escalate {
                reasons,
                suggested_identity_change,
            } = verdict
            {
                NodeOutcome::Escalated {
                    reasons,
                    suggested_identity_change,
                }
            } else {
                NodeOutcome::Blocked {
                    reasons: verdict.reasons().to_vec(),
                }
            }
        }
        Err(Refused::AuditFailed(error)) => NodeOutcome::AuditFailed {
            detail: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::{AuditLog, RuleAuditor};
    use crate::identity::DevLoop;
    use crate::planner::{Plan, SpawnNode};
    use crate::task::DEFAULT_MAX_CONCURRENT_TASKS;
    use std::sync::Mutex;

    struct RecordingDispatcher {
        dispatched: Mutex<Vec<String>>,
        terminal: TaskStatus,
    }

    impl RecordingDispatcher {
        fn completing() -> Self {
            Self {
                dispatched: Mutex::new(Vec::new()),
                terminal: completed_status(),
            }
        }

        fn failing() -> Self {
            Self {
                dispatched: Mutex::new(Vec::new()),
                terminal: TaskStatus::Failed {
                    finished_at: chrono::Utc::now(),
                    error: "boom".to_string(),
                    diagnostics: crate::task::FailureDiagnostics {
                        error_type: "Test".to_string(),
                        iterations_completed: 0,
                        last_tool_call: None,
                        partial_changes: None,
                        tool_call_history: vec![],
                        last_agent_state: None,
                        conversation_snapshot: None,
                        denials: vec![],
                        action_audit: vec![],
                    },
                },
            }
        }

        fn dispatch_count(&self) -> usize {
            self.dispatched.lock().unwrap().len()
        }
    }

    fn completed_status() -> TaskStatus {
        TaskStatus::Completed {
            finished_at: chrono::Utc::now(),
            result: crate::task::TaskResult {
                result_summary: "done".to_string(),
                changes_patch: None,
                format_patch: None,
                files_modified: vec![],
                tool_calls_made: vec![],
                denials: vec![],
                action_audit: vec![],
                iterations: 1,
                model_used: "mock".to_string(),
                qa_summary: crate::qa::QaSummary::default(),
            },
        }
    }

    #[async_trait]
    impl SpawnDispatcher for RecordingDispatcher {
        async fn dispatch(
            &self,
            allowed: Allowed,
            _repo_path: PathBuf,
            _branch: String,
            _model: String,
            _max_iterations: usize,
            _provider: Arc<dyn ModelProvider>,
        ) -> TaskId {
            let id = TaskId::new();
            self.dispatched
                .lock()
                .unwrap()
                .push(allowed.identity().name().to_string());
            id
        }

        async fn wait_terminal(&self, _task_id: &TaskId) -> Option<TaskStatus> {
            Some(self.terminal.clone())
        }
    }

    fn node(
        id: &str,
        identity: &str,
        dev_loop: DevLoop,
        subtask: &str,
        depends_on: &[&str],
    ) -> SpawnNode {
        SpawnNode {
            id: PlanNodeId::new(id),
            identity: identity.to_string(),
            subtask: subtask.to_string(),
            dev_loop,
            depends_on: depends_on.iter().map(|d| PlanNodeId::new(*d)).collect(),
        }
    }

    fn context() -> AuditContext {
        crate::auditor::rules::tests::context(true)
    }

    fn provider() -> Arc<dyn ModelProvider> {
        crate::planner::model::tests::MockProvider::replying(&[])
    }

    #[tokio::test]
    async fn a_fitting_spawn_is_allowed_and_dispatched() {
        let plan = Plan::new(vec![node(
            "a",
            "rust-implementer",
            DevLoop::Inner,
            "Add a regression test for the empty-password path.",
            &[],
        )])
        .unwrap();
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let dispatcher = RecordingDispatcher::completing();
        let parent = TaskSummary::new("task-1", "Fix bug X", "github.com/example/repo");
        let execution = execute_plan(
            &plan,
            &gate,
            &context(),
            &dispatcher,
            &parent,
            Path::new("/repo"),
            "HEAD",
            provider(),
        )
        .await
        .unwrap();
        assert_eq!(dispatcher.dispatch_count(), 1);
        match execution.outcome(&PlanNodeId::new("a")).unwrap() {
            NodeOutcome::Dispatched { effect, .. } => assert_eq!(*effect, EffectClass::Repository),
            other => panic!("expected Dispatched, got {other:?}"),
        }
        assert_eq!(execution.task_ids().len(), 1);
        assert_eq!(
            execution.aggregate_effect_class(),
            Some(EffectClass::Repository)
        );
    }

    #[tokio::test]
    async fn a_blocked_spawn_never_reaches_dispatch() {
        let plan = Plan::new(vec![node(
            "a",
            "rust-implementer",
            DevLoop::Outer,
            "Add a regression test.",
            &[],
        )])
        .unwrap();
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let dispatcher = RecordingDispatcher::completing();
        let parent = TaskSummary::new("task-1", "Fix bug X", "github.com/example/repo");
        let execution = execute_plan(
            &plan,
            &gate,
            &context(),
            &dispatcher,
            &parent,
            Path::new("/repo"),
            "HEAD",
            provider(),
        )
        .await
        .unwrap();
        assert_eq!(dispatcher.dispatch_count(), 0);
        match execution.outcome(&PlanNodeId::new("a")).unwrap() {
            NodeOutcome::Blocked { reasons } => assert!(!reasons.is_empty()),
            other => panic!("expected Blocked, got {other:?}"),
        }
        assert!(execution.task_ids().is_empty());
        assert_eq!(execution.aggregate_effect_class(), None);
    }

    #[tokio::test]
    async fn an_escalated_spawn_never_reaches_dispatch() {
        let plan = Plan::new(vec![node(
            "a",
            "rust-implementer",
            DevLoop::Inner,
            "Deploy the build to the sandbox cluster.",
            &[],
        )])
        .unwrap();
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let dispatcher = RecordingDispatcher::completing();
        let parent = TaskSummary::new("task-1", "Fix bug X", "github.com/example/repo");
        let execution = execute_plan(
            &plan,
            &gate,
            &crate::auditor::rules::tests::context(false),
            &dispatcher,
            &parent,
            Path::new("/repo"),
            "HEAD",
            provider(),
        )
        .await
        .unwrap();
        assert_eq!(dispatcher.dispatch_count(), 0);
        match execution.outcome(&PlanNodeId::new("a")).unwrap() {
            NodeOutcome::Escalated {
                suggested_identity_change,
                ..
            } => {
                assert_eq!(suggested_identity_change.name, "rust-implementer-sandbox");
            }
            other => panic!("expected Escalated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_audit_failure_never_reaches_dispatch() {
        let plan = Plan::new(vec![node(
            "a",
            "rust-implementer",
            DevLoop::Inner,
            "Add a test.",
            &[],
        )])
        .unwrap();
        let gate = Gate::new(
            crate::auditor::ModelAuditor::new(
                crate::planner::model::tests::MockProvider::replying(&[]),
                "m",
            ),
            AuditLog::in_memory(),
        );
        let dispatcher = RecordingDispatcher::completing();
        let parent = TaskSummary::new("task-1", "Fix bug X", "github.com/example/repo");
        let execution = execute_plan(
            &plan,
            &gate,
            &context(),
            &dispatcher,
            &parent,
            Path::new("/repo"),
            "HEAD",
            provider(),
        )
        .await
        .unwrap();
        assert_eq!(dispatcher.dispatch_count(), 0);
        match execution.outcome(&PlanNodeId::new("a")).unwrap() {
            NodeOutcome::AuditFailed { detail } => assert!(!detail.is_empty()),
            other => panic!("expected AuditFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dependents_of_a_blocked_node_are_skipped_transitively_but_a_sibling_still_runs() {
        let plan = Plan::new(vec![
            node(
                "blocked",
                "rust-implementer",
                DevLoop::Outer,
                "Add a test.",
                &[],
            ),
            node(
                "dependent",
                "pr-shepherd",
                DevLoop::Middle,
                "Watch the PR.",
                &["blocked"],
            ),
            node(
                "grandchild",
                "deployer",
                DevLoop::Outer,
                "Deploy the fix to the sandbox.",
                &["dependent"],
            ),
            node(
                "sibling",
                "pr-shepherd",
                DevLoop::Middle,
                "Watch a different PR.",
                &[],
            ),
        ])
        .unwrap();
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let dispatcher = RecordingDispatcher::completing();
        let parent = TaskSummary::new("task-1", "Fix bug X", "github.com/example/repo");
        let execution = execute_plan(
            &plan,
            &gate,
            &context(),
            &dispatcher,
            &parent,
            Path::new("/repo"),
            "HEAD",
            provider(),
        )
        .await
        .unwrap();
        assert!(matches!(
            execution.outcome(&PlanNodeId::new("blocked")).unwrap(),
            NodeOutcome::Blocked { .. }
        ));
        match execution.outcome(&PlanNodeId::new("dependent")).unwrap() {
            NodeOutcome::Skipped { blocked_dependency } => {
                assert_eq!(blocked_dependency, &PlanNodeId::new("blocked"));
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        match execution.outcome(&PlanNodeId::new("grandchild")).unwrap() {
            NodeOutcome::Skipped { blocked_dependency } => {
                assert_eq!(blocked_dependency, &PlanNodeId::new("dependent"));
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        assert!(matches!(
            execution.outcome(&PlanNodeId::new("sibling")).unwrap(),
            NodeOutcome::Dispatched { .. }
        ));
        assert_eq!(dispatcher.dispatch_count(), 1);
    }

    #[tokio::test]
    async fn a_dependency_that_dispatches_but_does_not_complete_skips_its_dependent() {
        let plan = Plan::new(vec![
            node("a", "rust-implementer", DevLoop::Inner, "Add a test.", &[]),
            node("b", "pr-shepherd", DevLoop::Middle, "Watch the PR.", &["a"]),
        ])
        .unwrap();
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let dispatcher = RecordingDispatcher::failing();
        let parent = TaskSummary::new("task-1", "Fix bug X", "github.com/example/repo");
        let execution = execute_plan(
            &plan,
            &gate,
            &context(),
            &dispatcher,
            &parent,
            Path::new("/repo"),
            "HEAD",
            provider(),
        )
        .await
        .unwrap();
        assert!(matches!(
            execution.outcome(&PlanNodeId::new("a")).unwrap(),
            NodeOutcome::Dispatched { .. }
        ));
        match execution.outcome(&PlanNodeId::new("b")).unwrap() {
            NodeOutcome::Skipped { blocked_dependency } => {
                assert_eq!(blocked_dependency, &PlanNodeId::new("a"));
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        assert_eq!(dispatcher.dispatch_count(), 1);
    }

    #[tokio::test]
    async fn a_cyclic_plan_is_refused_before_anything_dispatches() {
        let plan = Plan::new(vec![
            node("a", "rust-implementer", DevLoop::Inner, "x", &["b"]),
            node("b", "rust-implementer", DevLoop::Inner, "y", &["a"]),
        ])
        .unwrap();
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let dispatcher = RecordingDispatcher::completing();
        let parent = TaskSummary::new("task-1", "Fix bug X", "github.com/example/repo");
        let err = execute_plan(
            &plan,
            &gate,
            &context(),
            &dispatcher,
            &parent,
            Path::new("/repo"),
            "HEAD",
            provider(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, PlanError::Cycle(_)));
        assert_eq!(dispatcher.dispatch_count(), 0);
    }

    #[tokio::test]
    async fn a_node_naming_an_identity_absent_from_the_catalog_is_blocked_not_panicked() {
        let plan = Plan::new(vec![node("a", "ghost", DevLoop::Inner, "do work", &[])]).unwrap();
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let dispatcher = RecordingDispatcher::completing();
        let parent = TaskSummary::new("task-1", "Fix bug X", "github.com/example/repo");
        let execution = execute_plan(
            &plan,
            &gate,
            &context(),
            &dispatcher,
            &parent,
            Path::new("/repo"),
            "HEAD",
            provider(),
        )
        .await
        .unwrap();
        assert!(matches!(
            execution.outcome(&PlanNodeId::new("a")).unwrap(),
            NodeOutcome::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn task_manager_spawn_dispatcher_delegates_to_submit_spawn() {
        use crate::auditor::context::tests::auditor_identity;
        use crate::auditor::rules::tests::catalog;

        let repo_dir = tempfile::tempdir().unwrap();
        for args in &[
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            std::process::Command::new("git")
                .current_dir(repo_dir.path())
                .args(args)
                .output()
                .unwrap();
        }
        std::fs::write(repo_dir.path().join("README.md"), "# Test").unwrap();
        std::process::Command::new("git")
            .current_dir(repo_dir.path())
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo_dir.path())
            .args(["commit", "-m", "init"])
            .output()
            .unwrap();

        let audit_context = AuditContext::new(catalog(true), auditor_identity()).unwrap();
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let request = SpawnRequest {
            parent_task: TaskSummary::new("parent-1", "Fix bug X", "github.com/example/repo"),
            identity: "rust-implementer".to_string(),
            subtask: "Add a regression test.".to_string(),
            dev_loop: DevLoop::Inner,
            requested_effect: EffectClass::Workspace,
        };
        let allowed = gate.check(request, &audit_context).await.unwrap();

        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let stop_provider: Arc<dyn ModelProvider> = provider();
        let task_id = SpawnDispatcher::dispatch(
            &manager,
            allowed,
            repo_dir.path().to_path_buf(),
            "HEAD".to_string(),
            "mock".to_string(),
            1,
            stop_provider,
        )
        .await;
        let status = SpawnDispatcher::wait_terminal(&manager, &task_id).await;
        assert!(status.is_some());
    }
}
