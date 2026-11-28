use super::{
    pi_bind_error, pi_bind_failed, AuditedIdentity, BoundIdentity, FailureDiagnostics,
    ModelFacingLayers, TaskId, TaskManager, TaskResult, TaskRunner, TaskStatus,
};
use crate::auditor::Allowed;
use crate::harness_adapter::{
    pi_container_name, require_audited, PiContainerGuard, PiLaunch, PiRoute, ResolvedAgent,
    ScopedCapabilities,
};
use crate::hub::index::register_librarian;
use crate::hub::librarian::{Librarian, LibrarianFailure, LibrarianReply, LibrarianRun};
use crate::hub::lineage::LineageError;
use crate::hub::query::RawLibrarianAnswer;
use crate::identity::{AgentIdentity, LIBRARIAN_IDENTITY};
use crate::scheduler::{LaunchTarget, QueuedTask, Side};
use crate::tools::{ActionSubject, ToolRegistry};
use chrono::Utc;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

/// Fills a registry with the tools a librarian run is granted and served. It is called twice,
/// once for the capability grant and once for the registry the broker serves, so both come from
/// the same function.
pub(crate) type LibrarianTools = Arc<dyn Fn(&mut ToolRegistry) + Send + Sync>;

/// Why a librarian launch was refused before anything was queued.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum LibrarianRefused {
    /// The audited identity is not the built-in librarian.
    #[error("the audited identity is not the session librarian")]
    NotLibrarian,
    /// The spawn's parent names a malformed root or sits too deep.
    #[error(transparent)]
    Lineage(#[from] LineageError),
    /// The session registry could not record the librarian, so nothing was queued.
    #[error("the session registry could not record the librarian")]
    SessionRegistry,
}

struct LibrarianPrepared {
    route: Arc<PiRoute>,
    agent: ResolvedAgent,
    binding: crate::harness_adapter::BoundWorkspace,
}

type Refusal = (&'static str, String);

fn refusal(label: &'static str, message: &str) -> Refusal {
    (label, message.to_string())
}

impl TaskManager {
    /// Queue a librarian run for an audited spawn of the built-in `session-librarian` identity.
    /// The entry has no repository, no worktree and no scope check, it is never written to the
    /// queue store, its session record is a `Librarian` record that stores only the query length,
    /// and it runs only through the pi container route: with no route the task fails closed with
    /// `LibrarianUnavailable`. There is no in-process fallback.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn submit_librarian(
        &self,
        allowed: Allowed,
        tools: LibrarianTools,
    ) -> Result<TaskId, LibrarianRefused> {
        if allowed.identity().name() != LIBRARIAN_IDENTITY {
            return Err(LibrarianRefused::NotLibrarian);
        }
        let max_iterations = allowed.identity().limits.max_iterations;
        let model = allowed.model().to_string();
        let name = allowed.identity().name().to_string();
        let (request, identity, lineage) = {
            let tasks = self.runner.tasks.read().await;
            AuditedIdentity::split(allowed, &|id| tasks.get(id).and_then(|t| t.lineage.clone()))
        };
        let queued = QueuedTask::new(request.subtask, PathBuf::new(), "", model, max_iterations)
            .with_identity_hint(Some(name))
            .with_lineage(lineage?)
            .with_launch(LaunchTarget::Librarian);
        let task_id = queued.id.clone();
        if self.runner.enroll(&queued, &identity, false).is_err() {
            return Err(LibrarianRefused::SessionRegistry);
        }
        let bound = BoundIdentity::Audited(identity);
        self.runner
            .bound
            .write()
            .await
            .insert(task_id.clone(), bound);
        self.runner.stash_tools(&task_id, tools);
        self.runner.register(&queued, None).await;
        self.dispatcher.enqueue_ephemeral(queued).await;
        Ok(task_id)
    }
}

impl TaskRunner {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn stash_tools(&self, task_id: &TaskId, tools: LibrarianTools) {
        let lock = &self.librarian_tools;
        let mut stash = lock.lock().unwrap_or_else(PoisonError::into_inner);
        stash.insert(task_id.clone(), tools);
    }

    pub(super) fn take_tools(&self, task_id: &TaskId) -> Option<LibrarianTools> {
        let lock = &self.librarian_tools;
        let mut stash = lock.lock().unwrap_or_else(PoisonError::into_inner);
        stash.remove(task_id)
    }

    pub(super) fn librarian_layers(
        &self,
        task_id: &TaskId,
        identity: &AgentIdentity,
        repo: &str,
    ) -> ModelFacingLayers {
        let subject = ActionSubject {
            task_id: task_id.clone(),
            max_effect: identity.scope.max_effect,
            window: None,
            repo: repo.to_string(),
            branch: None,
            pr: None,
            environment: None,
            paths: identity.scope.paths.clone(),
            description: String::new(),
        };
        ModelFacingLayers {
            escalation_log: self.escalation_log(),
            action_gate: self.action_gate(),
            action_escalator: self.action_escalator(),
            subject,
            hub: None,
        }
    }

    fn prepare_librarian(
        &self,
        queued: &QueuedTask,
        bound: Option<&BoundIdentity>,
        tools: Option<LibrarianTools>,
    ) -> Result<LibrarianPrepared, Refusal> {
        let none = "the librarian runs only in the pi container route and none is configured";
        let route = self.pi_route.read().unwrap().clone();
        let route = route.ok_or(refusal("LibrarianUnavailable", none))?;
        let tools = tools.ok_or(refusal("StateError", "the librarian has no tool set"))?;
        let no_identity = refusal(
            "IdentityUnavailable",
            "the librarian has no audited identity",
        );
        let bound = bound.ok_or(no_identity)?;
        let audited_ref = require_audited(bound).map_err(pi_bind_error)?;
        let unaudited = refusal(
            "IdentityUnavailable",
            "the librarian identity was not audited",
        );
        let audited = bound.audited().ok_or(unaudited)?;
        let other = refusal(
            "IdentityUnavailable",
            "the launch target names another identity",
        );
        let is_librarian = audited.identity().name() == LIBRARIAN_IDENTITY;
        let audited = is_librarian.then_some(audited).ok_or(other)?;
        let identity = audited.identity();
        let layers = self.librarian_layers(&queued.id, identity, audited.repo());
        let mut grant = ToolRegistry::new();
        tools(&mut grant);
        let scoped = ScopedCapabilities::scope(grant, identity);
        let socket = route.broker_socket(&queued.id.0);
        let endpoint = route.endpoint().clone();
        let image = route.image();
        let resolved = ResolvedAgent::resolve(audited.allowed(), &scoped, endpoint, image, socket);
        let agent = resolved.map_err(pi_bind_failed("StateError"))?;
        let mut served = ToolRegistry::new();
        tools(&mut served);
        let repo = audited.repo();
        let attempt = agent.bind_registry(audited_ref, repo, &queued.id, served, layers);
        let binding = attempt.map_err(pi_bind_error)?;
        Ok(LibrarianPrepared {
            route,
            agent,
            binding,
        })
    }

    pub(super) async fn run_librarian(self: Arc<Self>, queued: QueuedTask, side: Side) {
        let task_id = queued.id.clone();
        let tools = self.take_tools(&task_id);
        let bound = self.bound.write().await.remove(&task_id);
        match self.prepare_librarian(&queued, bound.as_ref(), tools) {
            Ok(prepared) => self.launch_librarian(queued, side, prepared).await,
            Err((label, message)) => self.fail(&task_id, message, label).await,
        }
    }

    async fn launch_librarian(
        self: Arc<Self>,
        queued: QueuedTask,
        side: Side,
        prepared: LibrarianPrepared,
    ) {
        let LibrarianPrepared {
            route,
            agent,
            binding,
        } = prepared;
        let task_id = queued.id.clone();
        tracing::info!(task_id = %task_id, side = side.label(), "Starting librarian run");
        let started = TaskStatus::Running {
            started_at: Utc::now(),
            iterations: 0,
        };
        self.set_status(&task_id, started).await;
        let progress = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&progress);
        self.progress.write().await.insert(task_id.clone(), counter);
        let guard = PiContainerGuard::arm(pi_container_name(&task_id.0));
        let wall = Duration::from_secs(agent.limits().max_wall_clock_secs);
        let (tid, name) = (&task_id.0, guard.name());
        let launch = PiLaunch::new(tid, name, &agent, binding, &route, progress, wall);
        let outcome = route.executor().run(launch).await;
        drop(guard);
        self.progress.write().await.remove(&task_id);
        let status = match outcome.result {
            Ok(result) => {
                let budget = self
                    .cost_accountant()
                    .task_summary(agent.name(), &task_id.0);
                let result = TaskResult {
                    result_summary: result.result_summary,
                    changes_patch: None,
                    format_patch: None,
                    files_modified: Vec::new(),
                    tool_calls_made: result.tool_calls_made,
                    denials: result.denials,
                    action_audit: outcome.action_audit,
                    iterations: result.iterations,
                    model_used: queued.model,
                    blast_radius: None,
                    qa_summary: Default::default(),
                    budget,
                };
                TaskStatus::Completed {
                    finished_at: Utc::now(),
                    result,
                }
            }
            Err(e) => {
                let (calls, _, iterations_completed, state) = e.diagnostics();
                let diagnostics = FailureDiagnostics {
                    error_type: super::agent_error_type(&e).to_string(),
                    iterations_completed,
                    last_tool_call: calls.last().cloned(),
                    partial_changes: None,
                    tool_call_history: calls.to_vec(),
                    last_agent_state: Some(format!("{state:?}")),
                    conversation_snapshot: None,
                    denials: Vec::new(),
                    action_audit: outcome.action_audit,
                };
                TaskStatus::Failed {
                    finished_at: Utc::now(),
                    error: e.to_string(),
                    diagnostics,
                }
            }
        };
        self.set_status(&task_id, status).await;
    }
}

#[cfg_attr(not(test), allow(dead_code))]
const UNAVAILABLE: &str = "LibrarianUnavailable";

/// The task layer's implementation of the hub's librarian seam. It submits the run through
/// [`TaskManager::submit_librarian`], so it runs only in the pi container route, and returns the
/// finished task's reply for the hub's strict parser. A future dropped before the task finishes
/// cancels the task.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct TaskLibrarian {
    manager: Arc<TaskManager>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl TaskLibrarian {
    pub(crate) fn new(manager: Arc<TaskManager>) -> Self {
        Self { manager }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
struct CancelOnDrop {
    manager: Arc<TaskManager>,
    task: TaskId,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let handle = tokio::runtime::Handle::try_current();
        if let (true, Ok(handle)) = (self.armed, handle) {
            handle.spawn(cancel_task(Arc::clone(&self.manager), self.task.clone()));
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
async fn cancel_task(manager: Arc<TaskManager>, task: TaskId) {
    let _ = manager.cancel(&task).await;
}

#[cfg_attr(not(test), allow(dead_code))]
fn reply_of(status: Option<TaskStatus>, task: TaskId) -> Result<LibrarianReply, LibrarianFailure> {
    let none = LibrarianFailure::Unavailable;
    let any = LibrarianFailure::Failed;
    let mut failure = any;
    if let Some(TaskStatus::Failed { diagnostics, .. }) = &status {
        if diagnostics.error_type == UNAVAILABLE {
            failure = none;
        }
    }
    let done = if let Some(TaskStatus::Completed { result, .. }) = status {
        Some(result.result_summary)
    } else {
        None
    };
    let text = done.ok_or(failure)?;
    let raw = RawLibrarianAnswer::new(text).map_err(|_| any)?;
    Ok(LibrarianReply { raw, task })
}

#[async_trait::async_trait]
impl Librarian for TaskLibrarian {
    async fn answer(&self, run: LibrarianRun) -> Result<LibrarianReply, LibrarianFailure> {
        let (allowed, view) = run.unpack();
        let tools: LibrarianTools = Arc::new(move |registry: &mut ToolRegistry| {
            register_librarian(registry, Arc::clone(&view))
        });
        let launched = self.manager.submit_librarian(allowed, tools).await;
        let task = launched.map_err(|_| LibrarianFailure::Failed)?;
        let manager = Arc::clone(&self.manager);
        let mut guard = CancelOnDrop {
            manager,
            task: task.clone(),
            armed: true,
        };
        let status = self.manager.wait_terminal(&task).await;
        guard.armed = false;
        reply_of(status, task)
    }
}

#[cfg(test)]
mod tests {
    use super::{LibrarianRefused, LibrarianTools};
    use crate::agent::AgentState;
    use crate::auditor::{AuditContext, AuditLog, Gate, RuleAuditor, SpawnRequest, TaskSummary};
    use crate::harness_adapter::{PiExecutor, PiLaunch, PiLimits, PiRoute, PiRunOutcome};
    use crate::hub::index::{register_librarian, IndexView};
    use crate::hub::lineage::LineageError;
    use crate::hub::registry::test_support::Flaky;
    use crate::hub::session::{SessionKind, SessionStatus};
    use crate::hub::store::{InMemorySessionStore, SessionStore};
    use crate::identity::{DevLoop, IdentityCatalog, LIBRARIAN_IDENTITY};
    use crate::task::scheduler_tests::wait_for;
    use crate::task::tests::MockProvider;
    use crate::task::*;
    use std::sync::atomic::Ordering as AtomicOrdering;
    use std::time::Duration;

    const IMAGE: &str =
        "nanna-pi@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const QUERY: &str = "which session touched ghp_a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8 last";
    const REPLY: &str = "{\"matches\": []}";

    #[derive(Debug, Clone, Copy)]
    enum Mode {
        Complete,
        Fail,
        Wait,
    }

    #[derive(Debug, Clone, PartialEq)]
    struct Seen {
        task_id: String,
        container_name: String,
        repo: String,
        capability_names: Vec<String>,
        image: String,
        model: String,
    }

    #[derive(Debug)]
    struct Stub {
        mode: Mode,
        reply: String,
        seen: std::sync::Mutex<Vec<Seen>>,
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    impl Stub {
        fn new(mode: Mode) -> Arc<Self> {
            Self::replying(mode, REPLY)
        }

        fn replying(mode: Mode, reply: &str) -> Arc<Self> {
            Arc::new(Self {
                mode,
                reply: reply.to_string(),
                seen: std::sync::Mutex::new(Vec::new()),
                started: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            })
        }

        fn launches(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn completed(summary: &str) -> AgentRunResult {
        AgentRunResult {
            final_state: AgentState::Completed,
            iterations: 2,
            task_completed: true,
            result_summary: summary.to_string(),
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            token_usage: None,
            denials: vec![],
            action_audit: vec![],
        }
    }

    fn pi_failure() -> AgentError {
        AgentError::StateError {
            message: "pi container exited".to_string(),
            iterations_completed: 1,
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            last_agent_state: AgentState::Error("pi".to_string()),
        }
    }

    #[async_trait::async_trait]
    impl PiExecutor for Stub {
        async fn run(&self, launch: PiLaunch<'_>) -> PiRunOutcome {
            let agent = launch.agent().clone();
            let names = agent.capability_names();
            self.seen.lock().unwrap().push(Seen {
                task_id: launch.task_id().to_string(),
                container_name: launch.container_name().to_string(),
                repo: agent.audited_repo().to_string(),
                capability_names: names.into_iter().map(String::from).collect(),
                image: launch.route().image().to_string(),
                model: agent.model().to_string(),
            });
            self.started.notify_one();
            let result = match self.mode {
                Mode::Complete => Ok(completed(&self.reply)),
                Mode::Fail => Err(pi_failure()),
                Mode::Wait => {
                    self.release.notified().await;
                    Ok(completed(&self.reply))
                }
            };
            PiRunOutcome {
                result,
                action_audit: vec![],
            }
        }
    }

    async fn started(stub: &Arc<Stub>) {
        let limit = Duration::from_secs(5);
        let seen = tokio::time::timeout(limit, stub.started.notified()).await;
        assert!(seen.is_ok(), "the pi executor was never started");
    }

    fn route(stub: &Arc<Stub>) -> PiRoute {
        let executor = stub.clone() as Arc<dyn PiExecutor>;
        PiRoute::new(
            IMAGE,
            "/run/user/1000/nanna/broker",
            PiLimits::default(),
            executor,
        )
        .unwrap()
    }

    fn manager_with(stub: &Arc<Stub>) -> TaskManager {
        TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS).with_pi_route(Some(route(stub)))
    }

    fn provider() -> Arc<dyn ModelProvider> {
        MockProvider::new(vec![])
    }

    fn base_catalog() -> IdentityCatalog {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/identities/global"
        );
        IdentityCatalog::load(dir).unwrap()
    }

    fn context() -> AuditContext {
        let auditor = base_catalog().get("auditor").unwrap().clone();
        let catalog = base_catalog().with_builtins().unwrap();
        AuditContext::new(catalog, auditor).unwrap()
    }

    async fn allowed_as(identity: &str, subtask: &str, effect: EffectClass) -> Allowed {
        allowed_under("parent-1", identity, subtask, effect).await
    }

    async fn allowed_under(
        parent: &str,
        identity: &str,
        subtask: &str,
        effect: EffectClass,
    ) -> Allowed {
        let request = SpawnRequest {
            parent_task: TaskSummary::new(parent, "Ask", "builtin"),
            identity: identity.to_string(),
            subtask: subtask.to_string(),
            dev_loop: DevLoop::Inner,
            requested_effect: effect,
        };
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        gate.check(request, &context()).await.unwrap()
    }

    async fn librarian_allowed(query: &str) -> Allowed {
        allowed_as(LIBRARIAN_IDENTITY, query, EffectClass::None).await
    }

    fn empty_view() -> Arc<IndexView> {
        let identity = crate::identity::librarian_identity().unwrap();
        let lineage = Lineage::root_child(RootId::legacy(TaskId("caller".into())));
        let caller = TaskId("caller".into());
        let binding = SessionBinding::from_audited(&identity, caller, lineage);
        let store = InMemorySessionStore::default();
        Arc::new(IndexView::freeze(&store, None, &binding).unwrap())
    }

    fn tools() -> LibrarianTools {
        let view = empty_view();
        Arc::new(move |registry: &mut crate::tools::ToolRegistry| {
            register_librarian(registry, Arc::clone(&view))
        })
    }

    fn failed(status: &TaskStatus) -> (&str, &str) {
        match status {
            TaskStatus::Failed {
                error, diagnostics, ..
            } => (diagnostics.error_type.as_str(), error.as_str()),
            other => panic!("expected a failed task, got {other:?}"),
        }
    }

    fn session_of(store: &InMemorySessionStore, id: &TaskId) -> crate::hub::session::SessionRecord {
        let sid = SessionId::from_task(id.clone());
        store.get(sid).unwrap().unwrap()
    }

    #[tokio::test]
    async fn a_librarian_runs_in_the_pi_container_with_no_repository_or_worktree() {
        let stub = Stub::new(Mode::Complete);
        let manager = manager_with(&stub);
        let id = manager
            .submit_librarian(librarian_allowed(QUERY).await, tools())
            .await
            .unwrap();
        let task = wait_for(&manager, &id, |status| status.is_terminal()).await;
        match &task.status {
            TaskStatus::Completed { result, .. } => assert_eq!(result.result_summary, REPLY),
            other => panic!("expected completion, got {other:?}"),
        }
        assert_eq!(task.repo_path, PathBuf::new());
        let seen = stub.launches();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].task_id, id.0);
        assert_eq!(seen[0].image, IMAGE);
        assert_eq!(seen[0].container_name, format!("nanna-pi-{}", id.0));
        assert_eq!(seen[0].repo, "builtin");
        assert_eq!(seen[0].capability_names, vec!["index_read", "index_search"]);
    }

    #[tokio::test]
    async fn a_finished_librarian_task_carries_no_patch_no_files_and_no_blast_radius() {
        let stub = Stub::new(Mode::Complete);
        let manager = manager_with(&stub);
        let allowed = librarian_allowed(QUERY).await;
        let id = manager.submit_librarian(allowed, tools()).await.unwrap();
        let task = wait_for(&manager, &id, |status| status.is_terminal()).await;
        let completed = matches!(task.status, TaskStatus::Completed { .. });
        assert!(completed, "{:?}", task.status);
        if let TaskStatus::Completed { result, .. } = &task.status {
            assert!(result.changes_patch.is_none());
            assert!(result.format_patch.is_none());
            assert!(result.files_modified.is_empty());
            assert!(result.blast_radius.is_none());
        }
    }

    #[tokio::test]
    async fn a_pi_failure_fails_the_librarian_task_and_never_falls_back() {
        let stub = Stub::new(Mode::Fail);
        let manager = manager_with(&stub);
        let allowed = librarian_allowed("where is the parser").await;
        let id = manager.submit_librarian(allowed, tools()).await.unwrap();
        let task = wait_for(&manager, &id, |status| status.is_terminal()).await;
        assert_eq!(failed(&task.status).0, "StateError");
        assert_eq!(stub.launches().len(), 1);
    }

    #[tokio::test]
    async fn without_a_pi_route_the_librarian_fails_closed() {
        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        assert!(!manager.has_pi_route());
        let allowed = librarian_allowed("where is the parser").await;
        let id = manager.submit_librarian(allowed, tools()).await.unwrap();
        let task = wait_for(&manager, &id, |status| status.is_terminal()).await;
        let (label, error) = failed(&task.status);
        assert_eq!(label, "LibrarianUnavailable");
        assert!(error.contains("pi container route"), "{error}");
    }

    #[tokio::test]
    async fn a_librarian_entry_is_never_written_to_the_queue_store() {
        let stub = Stub::new(Mode::Wait);
        let store = InMemoryQueueStore::default();
        let manager = TaskManager::with_stores(
            DEFAULT_MAX_CONCURRENT_TASKS,
            Box::new(HybridPolicy::default()),
            Box::new(store.clone()),
            Arc::new(InMemoryLeaseStore::default()),
        )
        .unwrap()
        .with_pi_route(Some(route(&stub)));
        let allowed = librarian_allowed("where is the parser").await;
        let id = manager.submit_librarian(allowed, tools()).await.unwrap();
        started(&stub).await;
        assert!(manager.dispatcher.is_running(&id).await);
        assert!(store.load().unwrap().is_empty());
        stub.release.notify_one();
        let task = wait_for(&manager, &id, |status| status.is_terminal()).await;
        assert!(matches!(task.status, TaskStatus::Completed { .. }));
        assert!(store.load().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_restart_drops_a_stored_librarian_entry_without_running_or_registering_it() {
        let store = InMemoryQueueStore::default();
        let librarian = QueuedTask::new("forged query", PathBuf::new(), "", "m", 8)
            .with_identity_hint(Some(LIBRARIAN_IDENTITY.to_string()))
            .with_launch(LaunchTarget::Librarian);
        store.insert(&librarian).unwrap();
        let stub = Stub::new(Mode::Complete);
        let manager = TaskManager::restore(
            DEFAULT_MAX_CONCURRENT_TASKS,
            Box::new(HybridPolicy::default()),
            Box::new(store.clone()),
            Arc::new(InMemoryLeaseStore::default()),
            provider(),
        )
        .await
        .unwrap()
        .with_pi_route(Some(route(&stub)));
        assert!(store.load().unwrap().is_empty());
        assert!(manager.poll(&librarian.id).await.is_none());
        assert!(!manager.dispatcher.is_queued(&librarian.id).await);
        assert!(stub.launches().is_empty());
    }

    #[tokio::test]
    async fn the_librarian_session_record_withholds_the_query_and_the_reply() {
        let stub = Stub::new(Mode::Complete);
        let store = Arc::new(InMemorySessionStore::default());
        let registry = Arc::new(SessionRegistry::new(store.clone()));
        let manager = manager_with(&stub).with_session_registry(registry);
        let id = manager
            .submit_librarian(librarian_allowed(QUERY).await, tools())
            .await
            .unwrap();
        let marker = format!("[query withheld; {} chars]", QUERY.chars().count());
        wait_for(&manager, &id, |status| status.is_terminal()).await;
        let record = session_of(&store, &id);
        assert_eq!(record.kind, SessionKind::Librarian);
        assert_eq!(record.status, SessionStatus::Completed);
        assert_eq!(record.subtask, marker);
        assert_eq!(record.result_summary.as_deref(), Some("[reply withheld]"));
        let shown = format!("{record:?}{}", serde_json::to_string(&record).unwrap());
        assert!(!shown.contains("ghp_"), "{shown}");
    }

    #[tokio::test]
    async fn a_failed_librarian_session_closes_and_is_no_longer_tracked() {
        let stub = Stub::new(Mode::Fail);
        let store = Arc::new(InMemorySessionStore::default());
        let registry = Arc::new(SessionRegistry::new(store.clone()));
        let manager = manager_with(&stub).with_session_registry(Arc::clone(&registry));
        let allowed = librarian_allowed("where is the parser").await;
        let id = manager.submit_librarian(allowed, tools()).await.unwrap();
        let sid = SessionId::from_task(id.clone());
        wait_for(&manager, &id, |status| status.is_terminal()).await;
        let record = session_of(&store, &id);
        assert_eq!(record.kind, SessionKind::Librarian);
        assert_eq!(record.status, SessionStatus::Failed);
        assert!(!registry.tracks(&sid));
    }

    #[tokio::test]
    async fn only_the_built_in_librarian_identity_may_be_submitted() {
        let stub = Stub::new(Mode::Complete);
        let manager = manager_with(&stub);
        let other = allowed_as("rust-implementer", "Add a test.", EffectClass::Workspace).await;
        let refused = manager.submit_librarian(other, tools()).await;
        assert_eq!(refused.unwrap_err(), LibrarianRefused::NotLibrarian);
        assert!(stub.launches().is_empty());
    }

    async fn forged(manager: &TaskManager, bound: BoundIdentity, with_tools: bool) -> TaskId {
        let queued = QueuedTask::new("q", PathBuf::new(), "", "m", 8)
            .with_identity_hint(Some(LIBRARIAN_IDENTITY.to_string()))
            .with_launch(LaunchTarget::Librarian);
        let id = queued.id.clone();
        manager.runner.bound.write().await.insert(id.clone(), bound);
        if with_tools {
            manager.runner.stash_tools(&id, tools());
        }
        manager.submit_task(queued, provider()).await;
        id
    }

    #[tokio::test]
    async fn the_launch_target_cannot_run_an_identity_other_than_the_librarian() {
        let stub = Stub::new(Mode::Complete);
        let manager = manager_with(&stub);
        let other = allowed_as("rust-implementer", "Add a test.", EffectClass::Workspace).await;
        let (_, identity, _) = AuditedIdentity::split(other, &|_| None);
        let id = forged(&manager, BoundIdentity::Audited(identity), true).await;
        let task = wait_for(&manager, &id, |status| status.is_terminal()).await;
        assert_eq!(failed(&task.status).0, "IdentityUnavailable");
        assert!(stub.launches().is_empty());
    }

    #[tokio::test]
    async fn an_unaudited_librarian_entry_is_refused_before_any_container() {
        let stub = Stub::new(Mode::Complete);
        let manager = manager_with(&stub);
        let identity = crate::identity::librarian_identity().unwrap();
        let id = forged(&manager, BoundIdentity::Unaudited(identity), true).await;
        let task = wait_for(&manager, &id, |status| status.is_terminal()).await;
        assert_eq!(failed(&task.status).0, "IdentityUnavailable");
        assert!(stub.launches().is_empty());
    }

    #[tokio::test]
    async fn a_librarian_entry_without_a_tool_set_fails_before_any_container() {
        let stub = Stub::new(Mode::Complete);
        let manager = manager_with(&stub);
        let allowed = librarian_allowed("where is the parser").await;
        let (_, identity, _) = AuditedIdentity::split(allowed, &|_| None);
        let id = forged(&manager, BoundIdentity::Audited(identity), false).await;
        let task = wait_for(&manager, &id, |status| status.is_terminal()).await;
        assert_eq!(failed(&task.status).0, "StateError");
        assert!(stub.launches().is_empty());
    }

    #[tokio::test]
    async fn a_librarian_entry_with_no_identity_fails_before_any_container() {
        let stub = Stub::new(Mode::Complete);
        let manager = manager_with(&stub);
        let queued =
            QueuedTask::new("q", PathBuf::new(), "", "m", 8).with_launch(LaunchTarget::Librarian);
        let id = queued.id.clone();
        manager.runner.stash_tools(&id, tools());
        manager.submit_task(queued, provider()).await;
        let task = wait_for(&manager, &id, |status| status.is_terminal()).await;
        assert_eq!(failed(&task.status).0, "IdentityUnavailable");
        assert!(stub.launches().is_empty());
    }

    #[test]
    fn the_launch_target_serialises_only_when_it_is_not_a_repo() {
        let queued = QueuedTask::new("t", PathBuf::from("/r"), "HEAD", "m", 1);
        assert_eq!(queued.launch, LaunchTarget::Repo);
        let json = serde_json::to_string(&queued).unwrap();
        assert!(!json.contains("launch"), "{json}");
        let back: QueuedTask = serde_json::from_str(&json).unwrap();
        assert_eq!(back.launch, LaunchTarget::Repo);
        let librarian = queued.with_launch(LaunchTarget::Librarian);
        let json = serde_json::to_string(&librarian).unwrap();
        assert!(json.contains("\"launch\":\"librarian\""), "{json}");
        let back: QueuedTask = serde_json::from_str(&json).unwrap();
        assert_eq!(back.launch, LaunchTarget::Librarian);
    }

    #[tokio::test]
    async fn a_librarian_chain_deeper_than_the_cap_is_refused_and_queues_nothing() {
        let manager = TaskManager::new(0);
        let mut parent = format!("mcp-{}", uuid::Uuid::new_v4());
        for _ in 0..crate::hub::lineage::MAX_DEPTH {
            let allowed =
                allowed_under(&parent, LIBRARIAN_IDENTITY, "where", EffectClass::None).await;
            parent = manager.submit_librarian(allowed, tools()).await.unwrap().0;
        }
        let before = manager.list().await.len();
        let allowed = allowed_under(&parent, LIBRARIAN_IDENTITY, "where", EffectClass::None).await;
        let refused = manager
            .submit_librarian(allowed, tools())
            .await
            .unwrap_err();
        let depth = crate::hub::lineage::MAX_DEPTH;
        assert_eq!(
            refused,
            LibrarianRefused::Lineage(LineageError::TooDeep(depth))
        );
        assert_eq!(manager.list().await.len(), before);
    }

    #[tokio::test]
    async fn a_session_registry_that_cannot_record_the_librarian_refuses_it_and_queues_nothing() {
        let stub = Stub::new(Mode::Complete);
        let flaky = Flaky::new();
        flaky.writes.store(true, AtomicOrdering::SeqCst);
        let registry = Arc::new(SessionRegistry::new(flaky));
        let manager = manager_with(&stub).with_session_registry(registry);
        let allowed = librarian_allowed("where is the parser").await;
        let refused = manager
            .submit_librarian(allowed, tools())
            .await
            .unwrap_err();
        assert_eq!(refused, LibrarianRefused::SessionRegistry);
        assert!(manager.list().await.is_empty());
        assert!(stub.launches().is_empty());
    }

    #[tokio::test]
    async fn a_librarian_run_and_cancel_leave_the_queue_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.jsonl");
        let stub = Stub::new(Mode::Wait);
        let store = crate::scheduler::JsonlQueueStore::open(&path).unwrap();
        let manager = TaskManager::with_stores(
            DEFAULT_MAX_CONCURRENT_TASKS,
            Box::new(HybridPolicy::default()),
            Box::new(store),
            Arc::new(InMemoryLeaseStore::default()),
        )
        .unwrap()
        .with_pi_route(Some(route(&stub)));
        let before = std::fs::read(&path).unwrap();
        let first = librarian_allowed("where is the parser").await;
        let first = manager.submit_librarian(first, tools()).await.unwrap();
        started(&stub).await;
        assert_eq!(std::fs::read(&path).unwrap(), before);
        stub.release.notify_one();
        wait_for(&manager, &first, |status| status.is_terminal()).await;
        let second = librarian_allowed("where is the lexer").await;
        let second = manager.submit_librarian(second, tools()).await.unwrap();
        started(&stub).await;
        manager.cancel(&second).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[tokio::test]
    async fn cancelling_a_queued_librarian_drops_its_tool_set_and_identity() {
        let manager = TaskManager::new(0);
        let allowed = librarian_allowed("where is the parser").await;
        let id = manager.submit_librarian(allowed, tools()).await.unwrap();
        assert!(manager
            .runner
            .librarian_tools
            .lock()
            .unwrap()
            .contains_key(&id));
        manager.cancel(&id).await.unwrap();
        assert!(manager.runner.librarian_tools.lock().unwrap().is_empty());
        assert!(manager.bound_identity_of(&id).await.is_none());
        assert!(!manager.dispatcher.is_queued(&id).await);
    }

    #[tokio::test]
    async fn the_librarian_action_subject_names_the_audited_label_and_never_the_query() {
        let manager = TaskManager::new(0);
        let identity = crate::identity::librarian_identity().unwrap();
        let id = TaskId("t-subject".to_string());
        let layers = manager.runner.librarian_layers(&id, &identity, "builtin");
        assert_eq!(layers.subject.repo, "builtin");
        assert_eq!(layers.subject.task_id, id);
        assert_eq!(layers.subject.max_effect, EffectClass::None);
        assert!(layers.subject.description.is_empty());
        assert!(layers.subject.branch.is_none());
        assert!(layers.subject.paths.is_empty());
        assert!(layers.hub.is_none());
    }

    use super::TaskLibrarian;
    use crate::hub::error::HubError;
    use crate::hub::librarian::{
        librarian_gate, Librarian, LibrarianFailure, LibrarianRun, QueryGate,
    };
    use crate::hub::limits::HubLimits;
    use crate::hub::query_flow::QueryRequest;
    use crate::hub::send::{AuditorProvider, MessageHub};
    use crate::hub::store::InMemoryMessageStore;

    fn seam_run_with(view: Arc<IndexView>, allowed: Allowed) -> LibrarianRun {
        LibrarianRun::new(allowed, view)
    }

    async fn seam_run() -> LibrarianRun {
        seam_run_with(empty_view(), librarian_allowed(QUERY).await)
    }

    fn seam(manager: TaskManager) -> (Arc<TaskManager>, TaskLibrarian) {
        let shared = Arc::new(manager);
        let seam = TaskLibrarian::new(Arc::clone(&shared));
        (shared, seam)
    }

    #[tokio::test]
    async fn the_seam_returns_the_finished_librarian_tasks_reply_and_its_task() {
        let stub = Stub::replying(Mode::Complete, "{\"matches\": []}");
        let (manager, librarian) = seam(manager_with(&stub));
        let reply = librarian.answer(seam_run().await).await.unwrap();
        let launches = stub.launches();
        assert_eq!(launches.len(), 1);
        assert_eq!(reply.task.0, launches[0].task_id);
        let known = std::collections::BTreeSet::new();
        let parsed = crate::hub::query::parse_strict(&reply.raw, &known, 1).unwrap();
        assert!(parsed.is_empty());
        assert_eq!(manager.list().await.len(), 1);
    }

    #[tokio::test]
    async fn the_seam_without_a_pi_route_is_unavailable_and_runs_nothing() {
        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let (_, librarian) = seam(manager);
        let failure = librarian.answer(seam_run().await).await.unwrap_err();
        assert_eq!(failure, LibrarianFailure::Unavailable);
    }

    #[tokio::test]
    async fn the_seam_reports_a_failed_container_run_as_failed() {
        let stub = Stub::new(Mode::Fail);
        let (_, librarian) = seam(manager_with(&stub));
        let failure = librarian.answer(seam_run().await).await.unwrap_err();
        assert_eq!(failure, LibrarianFailure::Failed);
    }

    #[tokio::test]
    async fn the_seam_refuses_a_reply_over_the_size_cap() {
        let big = "x".repeat(crate::hub::query::MAX_RAW_BYTES + 1);
        let stub = Stub::replying(Mode::Complete, &big);
        let (_, librarian) = seam(manager_with(&stub));
        let failure = librarian.answer(seam_run().await).await.unwrap_err();
        assert_eq!(failure, LibrarianFailure::Failed);
    }

    #[tokio::test]
    async fn the_seam_refuses_an_identity_other_than_the_librarian() {
        let stub = Stub::new(Mode::Complete);
        let (_, librarian) = seam(manager_with(&stub));
        let other = allowed_as("rust-implementer", "Add a test.", EffectClass::Workspace).await;
        let run = seam_run_with(empty_view(), other);
        let failure = librarian.answer(run).await.unwrap_err();
        assert_eq!(failure, LibrarianFailure::Failed);
        assert!(stub.launches().is_empty());
    }

    #[tokio::test]
    async fn the_seam_reports_a_session_registry_that_cannot_record_the_run_as_failed() {
        let stub = Stub::new(Mode::Complete);
        let flaky = Flaky::new();
        flaky.writes.store(true, AtomicOrdering::SeqCst);
        let registry = Arc::new(SessionRegistry::new(flaky));
        let manager = manager_with(&stub).with_session_registry(registry);
        let (_, librarian) = seam(manager);
        let failure = librarian.answer(seam_run().await).await.unwrap_err();
        assert_eq!(failure, LibrarianFailure::Failed);
        assert!(stub.launches().is_empty());
    }

    #[tokio::test]
    async fn a_dropped_seam_future_cancels_the_librarian_task_and_closes_its_session() {
        let stub = Stub::new(Mode::Wait);
        let store = Arc::new(InMemorySessionStore::default());
        let registry = Arc::new(SessionRegistry::new(store.clone()));
        let manager = manager_with(&stub).with_session_registry(registry);
        let (manager, librarian) = seam(manager);
        let run = seam_run().await;
        let waiting = tokio::time::timeout(Duration::from_millis(300), librarian.answer(run));
        assert!(waiting.await.is_err());
        let launches = stub.launches();
        assert_eq!(launches.len(), 1);
        let id = TaskId(launches[0].task_id.clone());
        let task = wait_for(&manager, &id, |status| status.is_terminal()).await;
        assert!(
            matches!(task.status, TaskStatus::Cancelled { .. }),
            "{:?}",
            task.status
        );
        let record = session_of(&store, &id);
        assert_eq!(record.kind, SessionKind::Librarian);
        assert!(!record.status.is_active(), "{:?}", record.status);
    }

    fn query_gate() -> QueryGate {
        let auditor = base_catalog().get("auditor").unwrap().clone();
        let rules: Box<dyn crate::auditor::Auditor> = Box::new(RuleAuditor::new());
        librarian_gate(rules, AuditLog::in_memory(), auditor, "configured").unwrap()
    }

    const PEER: &str = "peer-task";
    const PLANT: &str = "PLANTEDxq7Zk";

    struct World {
        store: Arc<dyn SessionStore>,
        registry: Arc<SessionRegistry>,
        hub: MessageHub,
        stub: Arc<Stub>,
        manager: Arc<TaskManager>,
    }

    fn peer_reply() -> String {
        let why = "edits the parser";
        format!("{{\"matches\":[{{\"id\":\"{PEER}\",\"relevance\":0.8,\"why\":\"{why}\"}}]}}")
    }

    fn world(store: Arc<dyn SessionStore>) -> World {
        let stub = Stub::replying(Mode::Complete, &peer_reply());
        let registry = Arc::new(SessionRegistry::new(store.clone()));
        let manager = manager_with(&stub).with_session_registry(Arc::clone(&registry));
        let (manager, librarian) = seam(manager);
        let auditors: Arc<dyn crate::action_auditor::MessageAuditor> =
            Arc::new(crate::action_auditor::RuleMessageAuditor::new());
        let provider: Arc<dyn AuditorProvider> = Arc::new(auditors);
        let hub = MessageHub::new(
            store.clone(),
            Arc::new(InMemoryMessageStore::new()),
            provider,
            Arc::new(crate::leases::SystemClock),
            HubLimits::default(),
            Arc::clone(&registry),
        );
        let hub = hub.with_librarian(query_gate(), Arc::new(librarian));
        World {
            store,
            registry,
            hub,
            stub,
            manager,
        }
    }

    async fn task_caller(world: &World, root: &RootId) -> SessionBinding {
        let lineage = Lineage::root_child(root.clone());
        let queued = QueuedTask::new("caller work", PathBuf::new(), "", "m", 8)
            .with_lineage(Some(lineage.clone()));
        world.manager.runner.register(&queued, None).await;
        let card = crate::identity::example();
        let binding = SessionBinding::from_audited(&card, queued.id.clone(), lineage);
        world
            .registry
            .register(&binding, "work", Utc::now())
            .unwrap();
        binding
    }

    fn add_peer(world: &World, root: &RootId) -> SessionBinding {
        let lineage = Lineage::root_child(root.clone());
        let card = crate::identity::example();
        let binding = SessionBinding::from_audited(&card, TaskId(PEER.into()), lineage);
        world
            .registry
            .register(&binding, "parser work", Utc::now())
            .unwrap();
        binding
    }

    fn librarian_id(world: &World) -> TaskId {
        let launches = world.stub.launches();
        assert_eq!(launches.len(), 1);
        TaskId(launches[0].task_id.clone())
    }

    #[tokio::test]
    async fn a_hub_query_runs_through_the_container_launch_and_leaves_a_finished_librarian_record()
    {
        let root = RootId::mcp(uuid::Uuid::from_u128(9));
        let world = world(Arc::new(InMemorySessionStore::default()));
        let me = task_caller(&world, &root).await;
        let peer = add_peer(&world, &root);
        let request = QueryRequest::new("who edits the parser", 3);
        let answer = world.hub.query(&me, &request).await.unwrap();
        assert_eq!(answer.matches.len(), 1);
        assert_eq!(&answer.matches[0].id, peer.session());
        let launches = world.stub.launches();
        assert_eq!(
            launches[0].capability_names,
            vec!["index_read", "index_search"]
        );
        assert_eq!(launches[0].model, "configured");
        let record = world
            .store
            .get(SessionId::from_task(librarian_id(&world)))
            .unwrap();
        let record = record.unwrap();
        assert_eq!(record.kind, SessionKind::Librarian);
        assert_eq!(record.status, SessionStatus::Completed);
        assert!(record.subtask.starts_with("[query withheld;"));
    }

    #[tokio::test]
    async fn the_librarian_of_a_task_caller_is_a_child_of_the_caller_in_the_callers_root() {
        let root = RootId::mcp(uuid::Uuid::from_u128(9));
        let world = world(Arc::new(InMemorySessionStore::default()));
        let me = task_caller(&world, &root).await;
        add_peer(&world, &root);
        let request = QueryRequest::new("who edits the parser", 3);
        world.hub.query(&me, &request).await.unwrap();
        let record = world
            .store
            .get(SessionId::from_task(librarian_id(&world)))
            .unwrap();
        let expected = Lineage::child_of(me.session().clone(), me.lineage()).unwrap();
        assert_eq!(record.unwrap().lineage, expected);
    }

    #[tokio::test]
    async fn a_caller_unknown_to_the_task_manager_gets_a_librarian_outside_its_root_and_the_query_fails_closed(
    ) {
        let root = RootId::mcp(uuid::Uuid::from_u128(9));
        let world = world(Arc::new(InMemorySessionStore::default()));
        let lineage = Lineage::root_child(root.clone());
        let card = crate::identity::example();
        let me = SessionBinding::from_audited(&card, TaskId("loose".into()), lineage);
        world.registry.register(&me, "work", Utc::now()).unwrap();
        add_peer(&world, &root);
        let request = QueryRequest::new("who edits the parser", 3);
        let refused = world.hub.query(&me, &request).await.unwrap_err();
        assert!(matches!(refused, HubError::LibrarianFailed), "{refused:?}");
        let record = world
            .store
            .get(SessionId::from_task(librarian_id(&world)))
            .unwrap();
        let record = record.unwrap();
        assert_ne!(record.lineage.root(), &root);
    }

    #[derive(Clone, Default)]
    struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_planted_query_token_reaches_no_journal_log_listing_or_description() {
        let dir = tempfile::tempdir().unwrap();
        let lock = crate::hub::store::HubLock::acquire(dir.path()).unwrap();
        let path = dir.path().join("sessions.jsonl");
        let jsonl = crate::hub::store::JsonlSessionStore::open(&path, &lock).unwrap();
        let root = RootId::mcp(uuid::Uuid::from_u128(9));
        let world = world(Arc::new(jsonl));
        let me = task_caller(&world, &root).await;
        add_peer(&world, &root);
        let logs = Logs::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let request = QueryRequest::new(&format!("who edits {PLANT} code"), 3);
        world.hub.query(&me, &request).await.unwrap();
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(file.contains("[query withheld;"), "{file}");
        assert!(!file.contains(PLANT), "{file}");
        let logged = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(!logged.contains(PLANT), "{logged}");
        let id = SessionId::from_task(librarian_id(&world));
        let messages = InMemoryMessageStore::new();
        let detail = crate::hub::view::session_describe(&*world.store, &messages, &me, &id);
        let detail = detail.unwrap();
        let shown = format!("{detail:?}{}", serde_json::to_string(&detail).unwrap());
        assert!(
            !shown.contains(PLANT) && shown.contains("query withheld;"),
            "{shown}"
        );
        let cursors = crate::hub::view::CursorTable::new();
        let mut request = crate::hub::view::ListRequest::default();
        request.filter.include_completed = true;
        let page =
            crate::hub::view::sessions_list(&*world.store, &cursors, &me, &request, Utc::now());
        let hidden = page.unwrap().items.iter().any(|item| item.id == id);
        assert!(!hidden, "a librarian is listed by default");
        request.filter.include_librarians = true;
        let page =
            crate::hub::view::sessions_list(&*world.store, &cursors, &me, &request, Utc::now());
        let shown = page.unwrap().items.iter().any(|item| item.id == id);
        assert!(shown, "include_librarians does not list the librarian");
    }
}
