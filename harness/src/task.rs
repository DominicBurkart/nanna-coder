use crate::agent::{AgentConfig, AgentContext, AgentError, AgentLoop};
use crate::entities::context::types::ToolCallRecord;
use crate::entities::InMemoryEntityStore;
use crate::scheduler::{
    BoxFuture, Dispatcher, HybridPolicy, InMemoryQueueStore, Launcher, QueueMetrics, QueueStore,
    QueueStoreError, QueuedTask, SchedulingPolicy, Side, TaskOrigin,
};
use crate::workspace::TaskWorkspace;
use chrono::{DateTime, Utc};
use model::provider::ModelProvider;
use model::types::ChatMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{watch, Mutex, RwLock};
use uuid::Uuid;

const MAX_DIFF_BYTES: usize = 1_000_000;
pub const DEFAULT_MAX_CONCURRENT_TASKS: usize = 8;

/// Default system prompt used for task-dispatched agent runs.
///
/// Kept in lock-step with `DEFAULT_SESSION_SYSTEM_PROMPT` in `main.rs`. They
/// are duplicated on purpose: `main.rs` is a `bin` target and cannot be
/// imported from here.
const DEFAULT_TASK_SYSTEM_PROMPT: &str = "You are a helpful coding assistant. Use the available tools to accomplish tasks. When you have completed the task, respond with a summary.";

/// Build the system prompt for a task run, appending any repo-level guidance
/// discovered under the task's workspace path (closes #231).
///
/// Precedence: `AGENTS.md` over `CLAUDE.md` (see
/// [`crate::agent::agents_md::load`]). Missing files produce no injection;
/// read errors are logged and swallowed so a broken guidance file never blocks
/// a task from starting.
fn build_task_system_prompt(workspace_path: &std::path::Path) -> String {
    match crate::agent::agents_md::load(workspace_path) {
        Ok(Some(doc)) => {
            tracing::info!(
                path = %doc.path.display(),
                source = doc.source.filename(),
                truncated = doc.truncated,
                "Loaded repo-level agent guidance into task system prompt"
            );
            format!(
                "{}\n\n{}",
                DEFAULT_TASK_SYSTEM_PROMPT,
                crate::agent::agents_md::format_system_prompt_fragment(&doc)
            )
        }
        Ok(None) => DEFAULT_TASK_SYSTEM_PROMPT.to_string(),
        Err(e) => {
            tracing::error!(
                error = %e,
                "Failed to read AGENTS.md / CLAUDE.md for task; continuing without repo guidance"
            );
            DEFAULT_TASK_SYSTEM_PROMPT.to_string()
        }
    }
}

/// Per-repo-path build lock map: prevents concurrent image builds for the same repo.
type BuildLocks = Arc<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub result_summary: String,
    pub changes_patch: Option<String>,
    pub format_patch: Option<String>,
    pub files_modified: Vec<String>,
    pub tool_calls_made: Vec<ToolCallRecord>,
    pub iterations: usize,
    pub model_used: String,
}

impl TaskResult {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "result_summary": self.result_summary,
            "changes_patch": self.changes_patch,
            "format_patch": self.format_patch,
            "files_modified": self.files_modified,
            "tool_calls_made": self.tool_calls_made,
            "iterations": self.iterations,
            "model_used": self.model_used,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureDiagnostics {
    pub error_type: String,
    pub iterations_completed: usize,
    pub last_tool_call: Option<ToolCallRecord>,
    pub partial_changes: Option<String>,
    pub tool_call_history: Vec<ToolCallRecord>,
    pub last_agent_state: Option<String>,
    pub conversation_snapshot: Option<Vec<ChatMessage>>,
}

impl FailureDiagnostics {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "error_type": self.error_type,
            "iterations_completed": self.iterations_completed,
            "last_tool_call": self.last_tool_call,
            "partial_changes": self.partial_changes,
            "tool_call_history": self.tool_call_history,
            "last_agent_state": self.last_agent_state,
            "conversation_snapshot": self.conversation_snapshot,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum TaskStatus {
    /// Accepted and queued (possibly parked) until the scheduler starts it.
    Pending,
    Running {
        started_at: DateTime<Utc>,
        iterations: usize,
    },
    Completed {
        finished_at: DateTime<Utc>,
        result: TaskResult,
    },
    Failed {
        finished_at: DateTime<Utc>,
        error: String,
        diagnostics: FailureDiagnostics,
    },
    Cancelled {
        finished_at: DateTime<Utc>,
        iterations_completed: usize,
    },
}

impl TaskStatus {
    /// Whether this status is terminal — the task will not transition further.
    ///
    /// Maps onto the MCP Tasks spec's terminal states (`completed`, `failed`,
    /// `cancelled`); `Pending`/`Running` correspond to the non-terminal
    /// `working` status.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed { .. } | TaskStatus::Failed { .. } | TaskStatus::Cancelled { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub description: String,
    pub repo_path: PathBuf,
    pub branch: String,
    pub model: String,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    /// Timestamp of the most recent status transition. Surfaced as
    /// `lastUpdatedAt` in the MCP Tasks wire representation.
    pub last_updated_at: DateTime<Utc>,
    /// Requested task lifetime in milliseconds (`None` = unlimited). Surfaced
    /// as `ttl` in the MCP Tasks wire representation.
    pub ttl_ms: Option<u64>,
    /// Earliest instant the scheduler may start the task; `None` when the
    /// task is not parked.
    #[serde(default)]
    pub not_before: Option<DateTime<Utc>>,
    /// Agent identity the task should run under, when known.
    #[serde(default)]
    pub identity_hint: Option<String>,
    /// Backlog origin, when the task was ingested from GitHub.
    #[serde(default)]
    pub origin: Option<TaskOrigin>,
}

/// Per-task terminal-completion channels. `wait_terminal` subscribes to the
/// watch for a task and awaits a terminal status; every status transition
/// broadcasts the new status here. A `watch` (rather than `Notify`) is used
/// deliberately: it retains the latest value, so a waiter that subscribes
/// after the terminal transition still observes it — there is no lost-wakeup
/// race.
///
/// The retained [`watch::Receiver`] in each entry is a deliberate keep-alive:
/// `watch::Sender::send` is a no-op (returns `Err`) when there are no live
/// receivers, so without it a status broadcast sent before any
/// `wait_terminal` subscriber existed would silently fail to update the
/// stored value, and a later subscriber would observe a stale status forever.
type StatusSenders =
    Arc<RwLock<HashMap<TaskId, (watch::Sender<TaskStatus>, watch::Receiver<TaskStatus>)>>>;

/// Runtime state of dispatched tasks, shared between the [`TaskManager`]
/// that reads it and the scheduler's dispatcher that launches through it.
struct TaskRunner {
    tasks: Arc<RwLock<HashMap<TaskId, Task>>>,
    progress: Arc<RwLock<HashMap<TaskId, Arc<AtomicUsize>>>>,
    image_cache: Arc<RwLock<HashMap<PathBuf, String>>>,
    /// Per-repo-path mutex to prevent concurrent image builds for the same repo.
    build_locks: BuildLocks,
    status_senders: StatusSenders,
    /// Provider supplied at submission, consumed when the task starts.
    providers: RwLock<HashMap<TaskId, Arc<dyn ModelProvider>>>,
    /// Provider for tasks restored from the queue store, which carry none.
    default_provider: Option<Arc<dyn ModelProvider>>,
}

/// Manages task submission, scheduling and lifecycle.
///
/// Submissions beyond `max_concurrent_tasks` are queued, not rejected, and
/// started by the scheduler as slots free (see [`crate::scheduler`]). The
/// queue is held in a [`QueueStore`]; [`TaskManager::restore`] rebuilds a
/// manager from a persisted store so queued work survives a restart.
pub struct TaskManager {
    runner: Arc<TaskRunner>,
    dispatcher: Arc<Dispatcher<Arc<TaskRunner>>>,
}

impl TaskManager {
    /// An in-memory manager with the default hybrid policy.
    pub fn new(max_concurrent_tasks: usize) -> Self {
        Self::build(
            max_concurrent_tasks,
            Box::new(HybridPolicy::default()),
            Box::new(InMemoryQueueStore::default()),
            None,
        )
        .expect("an empty in-memory queue store always loads")
    }

    /// Build a manager over `store`, re-queue every entry the store still
    /// holds and start dispatching. Restored entries run with `provider`;
    /// entries submitted later carry their own.
    pub async fn restore(
        max_concurrent_tasks: usize,
        policy: Box<dyn SchedulingPolicy>,
        store: Box<dyn QueueStore>,
        provider: Arc<dyn ModelProvider>,
    ) -> Result<Self, QueueStoreError> {
        let manager = Self::build(max_concurrent_tasks, policy, store, Some(provider))?;
        for queued in manager.dispatcher.queued().await {
            manager.runner.register(&queued, None).await;
        }
        manager.dispatcher.dispatch().await;
        Ok(manager)
    }

    fn build(
        max_concurrent_tasks: usize,
        policy: Box<dyn SchedulingPolicy>,
        store: Box<dyn QueueStore>,
        default_provider: Option<Arc<dyn ModelProvider>>,
    ) -> Result<Self, QueueStoreError> {
        let runner = Arc::new(TaskRunner {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            progress: Arc::new(RwLock::new(HashMap::new())),
            image_cache: Arc::new(RwLock::new(HashMap::new())),
            build_locks: Arc::new(Mutex::new(HashMap::new())),
            status_senders: Arc::new(RwLock::new(HashMap::new())),
            providers: RwLock::new(HashMap::new()),
            default_provider,
        });
        let dispatcher =
            Dispatcher::open(Arc::clone(&runner), policy, store, max_concurrent_tasks)?;
        Ok(Self { runner, dispatcher })
    }

    /// Backlog depth, parked count, age of the oldest queued task and
    /// per-side dispatch counts.
    pub async fn queue_metrics(&self) -> QueueMetrics {
        self.dispatcher.metrics(Utc::now()).await
    }

    async fn get_or_build_image(
        cache: &Arc<RwLock<HashMap<PathBuf, String>>>,
        build_locks: &BuildLocks,
        repo_path: &std::path::Path,
    ) -> Result<String, String> {
        Self::get_or_build_image_using(cache, build_locks, repo_path, |source| {
            let image_path =
                image_builder::build_dev_container(source).map_err(|e| e.to_string())?;
            let runtime = crate::container::detect_runtime();
            crate::container::load_image_from_path(&runtime, &image_path).map_err(|e| e.to_string())
        })
        .await
    }

    /// Inner implementation of image acquisition that accepts a custom build+load
    /// function.  Kept separate from `get_or_build_image` so the caching and
    /// locking logic can be exercised in unit tests without a real Nix/container
    /// environment.
    async fn get_or_build_image_using<F>(
        cache: &Arc<RwLock<HashMap<PathBuf, String>>>,
        build_locks: &BuildLocks,
        repo_path: &std::path::Path,
        build_fn: F,
    ) -> Result<String, String>
    where
        F: FnOnce(&std::path::Path) -> Result<String, String> + Send + 'static,
    {
        let canonical = repo_path.canonicalize().map_err(|e| e.to_string())?;

        // Fast path: return cached image if already built.
        {
            let cache_read = cache.read().await;
            if let Some(image_ref) = cache_read.get(&canonical) {
                return Ok(image_ref.clone());
            }
        }

        // Obtain (or create) a per-path build lock so only one task builds
        // the image for a given repo at a time.
        let path_lock = {
            let mut locks = build_locks.lock().await;
            Arc::clone(
                locks
                    .entry(canonical.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(()))),
            )
        };
        let _build_guard = path_lock.lock().await;

        // Re-check cache now that we hold the per-path lock — another task
        // may have completed the build while we were waiting.
        {
            let cache_read = cache.read().await;
            if let Some(image_ref) = cache_read.get(&canonical) {
                return Ok(image_ref.clone());
            }
        }

        let source = canonical.clone();
        let build_result = tokio::task::spawn_blocking(move || build_fn(&source))
            .await
            .map_err(|e| e.to_string());
        let image_ref = match build_result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) | Err(e) => {
                build_locks.lock().await.remove(&canonical);
                return Err(e);
            }
        };

        {
            let mut cache_write = cache.write().await;
            cache_write.insert(canonical.clone(), image_ref.clone());
        }
        // Remove the per-path build lock now that the image is cached; future
        // callers will hit the fast path and no longer need the lock.
        {
            let mut locks = build_locks.lock().await;
            locks.remove(&canonical);
        }
        Ok(image_ref)
    }

    /// Submit a task that runs as soon as a slot is free.
    pub async fn submit(
        &self,
        description: String,
        repo_path: PathBuf,
        branch: String,
        model: String,
        max_iterations: usize,
        provider: Arc<dyn ModelProvider>,
    ) -> TaskId {
        self.submit_task(
            QueuedTask::new(description, repo_path, branch, model, max_iterations),
            provider,
        )
        .await
    }

    /// Submit a fully described queue entry, which may be parked with
    /// `not_before` or carry an identity hint and backlog origin.
    ///
    /// The task is `Pending` until the scheduler starts it. If the queue
    /// store rejects the entry the task is recorded as `Failed` with error
    /// type `QueuePersistFailed` rather than dropped silently.
    pub async fn submit_task(
        &self,
        queued: QueuedTask,
        provider: Arc<dyn ModelProvider>,
    ) -> TaskId {
        let task_id = queued.id.clone();
        self.runner.register(&queued, Some(provider)).await;
        if let Err(e) = self.dispatcher.enqueue(queued).await {
            self.runner
                .fail(&task_id, e.to_string(), "QueuePersistFailed")
                .await;
        }
        task_id
    }

    pub async fn poll(&self, task_id: &TaskId) -> Option<Task> {
        let tasks = self.runner.tasks.read().await;
        let mut task = tasks.get(task_id)?.clone();
        drop(tasks);

        if let TaskStatus::Running { started_at, .. } = task.status {
            let progress = self.runner.progress.read().await;
            if let Some(counter) = progress.get(task_id) {
                let current_iterations = counter.load(Ordering::Relaxed);
                task.status = TaskStatus::Running {
                    started_at,
                    iterations: current_iterations,
                };
            }
        }

        Some(task)
    }

    pub async fn get_result(&self, task_id: &TaskId) -> Option<TaskResult> {
        let tasks = self.runner.tasks.read().await;
        tasks.get(task_id).and_then(|t| {
            if let TaskStatus::Completed { result, .. } = &t.status {
                Some(result.clone())
            } else {
                None
            }
        })
    }

    pub async fn list(&self) -> Vec<Task> {
        let tasks = self.runner.tasks.read().await;
        tasks.values().cloned().collect()
    }

    /// Cancel a queued or running task. A queued task leaves the queue and
    /// the store; a running one is aborted and its slot handed to the next
    /// entry.
    pub async fn cancel(&self, task_id: &TaskId) -> Result<Task, String> {
        self.dispatcher
            .cancel(task_id)
            .await
            .map_err(|e| e.to_string())?;

        let iterations_completed = {
            let mut progress = self.runner.progress.write().await;
            let count = progress
                .get(task_id)
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0);
            progress.remove(task_id);
            count
        };

        let cancelled = {
            let mut tasks = self.runner.tasks.write().await;
            let task = tasks
                .get_mut(task_id)
                .ok_or_else(|| format!("Task not found: {}", task_id))?;

            if task.status.is_terminal() {
                return Err(format!(
                    "Task {} cannot be cancelled: already finished",
                    task_id
                ));
            }

            let now = Utc::now();
            task.status = TaskStatus::Cancelled {
                finished_at: now,
                iterations_completed,
            };
            task.last_updated_at = now;
            task.clone()
        };

        // Broadcast the terminal transition to any `wait_terminal` subscribers.
        {
            let senders = self.runner.status_senders.read().await;
            if let Some((tx, _keepalive)) = senders.get(task_id) {
                let _ = tx.send(cancelled.status.clone());
            }
        }

        Ok(cancelled)
    }

    /// Set the requested TTL (milliseconds) for a task. Called by the MCP layer
    /// after `submit` to record the client-requested lifetime so `tasks/get`
    /// can echo the actual `ttl`.
    pub async fn set_ttl(&self, task_id: &TaskId, ttl_ms: Option<u64>) {
        let mut tasks = self.runner.tasks.write().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.ttl_ms = ttl_ms;
        }
    }

    /// Await the terminal status of a task, returning it once reached.
    ///
    /// Returns immediately if the task is already terminal, and `None` if the
    /// task id is unknown. Backs the MCP `tasks/result` method, which must
    /// block until the underlying request reaches a terminal state.
    pub async fn wait_terminal(&self, task_id: &TaskId) -> Option<TaskStatus> {
        let mut rx = {
            let senders = self.runner.status_senders.read().await;
            senders.get(task_id)?.0.subscribe()
        };
        loop {
            {
                let current = rx.borrow_and_update();
                if current.is_terminal() {
                    return Some(current.clone());
                }
            }
            // `changed()` only errors if every sender has dropped; the sender is
            // retained in `status_senders` for the task's lifetime, so this is
            // effectively infallible, but fall back to the stored status rather
            // than hanging if it ever does.
            if rx.changed().await.is_err() {
                let tasks = self.runner.tasks.read().await;
                return tasks.get(task_id).map(|t| t.status.clone());
            }
        }
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CONCURRENT_TASKS)
    }
}

impl Launcher for Arc<TaskRunner> {
    fn launch(&self, task: &QueuedTask, side: Side) -> BoxFuture {
        let runner = Arc::clone(self);
        let task = task.clone();
        Box::pin(async move { runner.run(task, side).await })
    }
}

impl TaskRunner {
    /// Record a queued entry as a `Pending` task and open its status watch.
    /// The receiver is retained as a keep-alive (see `StatusSenders`).
    async fn register(&self, queued: &QueuedTask, provider: Option<Arc<dyn ModelProvider>>) {
        let task = Task {
            id: queued.id.clone(),
            description: queued.description.clone(),
            repo_path: queued.repo_path.clone(),
            branch: queued.branch.clone(),
            model: queued.model.clone(),
            status: TaskStatus::Pending,
            created_at: queued.submitted_at,
            last_updated_at: queued.submitted_at,
            ttl_ms: None,
            not_before: queued.not_before,
            identity_hint: queued.identity_hint.clone(),
            origin: queued.origin.clone(),
        };
        self.tasks.write().await.insert(queued.id.clone(), task);
        let (tx, rx) = watch::channel(TaskStatus::Pending);
        self.status_senders
            .write()
            .await
            .insert(queued.id.clone(), (tx, rx));
        if let Some(provider) = provider {
            self.providers
                .write()
                .await
                .insert(queued.id.clone(), provider);
        }
    }

    /// Transition a task to a new status: update the stored `Task` (status +
    /// `last_updated_at`) and broadcast the new status to any `wait_terminal`
    /// subscribers. This is the single choke point for status changes so the
    /// watch channel can never drift from the stored task.
    async fn set_status(&self, task_id: &TaskId, status: TaskStatus) {
        {
            let mut tasks = self.tasks.write().await;
            if let Some(task) = tasks.get_mut(task_id) {
                task.status = status.clone();
                task.last_updated_at = Utc::now();
            }
        }
        let senders = self.status_senders.read().await;
        if let Some((tx, _keepalive)) = senders.get(task_id) {
            let _ = tx.send(status);
        }
    }

    /// Mark a task `Failed` before any agent iteration ran.
    async fn fail(&self, task_id: &TaskId, error: String, error_type: &str) {
        self.progress.write().await.remove(task_id);
        self.set_status(
            task_id,
            TaskStatus::Failed {
                finished_at: Utc::now(),
                error,
                diagnostics: FailureDiagnostics {
                    error_type: error_type.to_string(),
                    iterations_completed: 0,
                    last_tool_call: None,
                    partial_changes: None,
                    tool_call_history: vec![],
                    last_agent_state: None,
                    conversation_snapshot: None,
                },
            },
        )
        .await;
    }

    async fn run(self: Arc<Self>, queued: QueuedTask, side: Side) {
        let task_id = queued.id.clone();
        let provider = {
            let mut providers = self.providers.write().await;
            providers
                .remove(&task_id)
                .or_else(|| self.default_provider.clone())
        };
        let Some(provider) = provider else {
            self.fail(
                &task_id,
                "no model provider registered for the task".to_string(),
                "NoProvider",
            )
            .await;
            return;
        };
        tracing::info!(
            task_id = %task_id,
            side = side.label(),
            identity_hint = ?queued.identity_hint,
            "Starting dispatched task"
        );
        let progress_counter = Arc::new(AtomicUsize::new(0));
        self.progress
            .write()
            .await
            .insert(task_id.clone(), Arc::clone(&progress_counter));

        // Require both flake.nix AND .devcontainer/ to opt in to the
        // container path, so that repos that merely happen to have a
        // flake.nix are not affected. Use tokio::fs to avoid blocking.
        let use_container = tokio::fs::try_exists(queued.repo_path.join("flake.nix"))
            .await
            .unwrap_or(false)
            && tokio::fs::try_exists(queued.repo_path.join(".devcontainer"))
                .await
                .unwrap_or(false);

        self.set_status(
            &task_id,
            TaskStatus::Running {
                started_at: Utc::now(),
                iterations: 0,
            },
        )
        .await;

        let workspace_result = if use_container {
            let image_result = TaskManager::get_or_build_image(
                &self.image_cache,
                &self.build_locks,
                &queued.repo_path,
            )
            .await;
            let image_ref = match image_result {
                Ok(r) => r,
                Err(e) => {
                    self.fail(&task_id, e, "ContainerSetupFailed").await;
                    return;
                }
            };
            TaskWorkspace::create_with_container(
                &queued.repo_path,
                &task_id.0,
                &queued.branch,
                &image_ref,
            )
            .await
            .map_err(|e| e.to_string())
        } else {
            TaskWorkspace::create(&queued.repo_path, &task_id.0, &queued.branch)
                .map_err(|e| e.to_string())
        };

        let mut workspace = match workspace_result {
            Ok(workspace) => workspace,
            Err(e) => {
                self.fail(&task_id, e, "WorkspaceCreationFailed").await;
                return;
            }
        };

        let tool_registry = workspace.build_tool_registry();
        let entity_store = InMemoryEntityStore::new();
        let agent_config = AgentConfig {
            max_iterations: queued.max_iterations,
            verbose: false,
            system_prompt: build_task_system_prompt(&workspace.workspace_path),
            model_name: queued.model.clone(),
        };
        let context = AgentContext {
            user_prompt: queued.description.clone(),
            conversation_history: vec![ChatMessage::user(&queued.description)],
            app_state_id: task_id.0.clone(),
        };

        let mut agent = AgentLoop::with_tools(agent_config, entity_store, provider, tool_registry);
        agent.set_progress_counter(Arc::clone(&progress_counter));
        let run_result = agent.run(context).await;

        let changes_patch = workspace.extract_changes().ok().and_then(bound_patch);

        let format_patch = workspace.format_patch().ok().flatten();

        let _ = workspace.cleanup();

        self.progress.write().await.remove(&task_id);

        match run_result {
            Ok(result) => {
                let files_modified = parse_modified_files(changes_patch.as_deref());
                let task_result = TaskResult {
                    result_summary: result.result_summary,
                    changes_patch,
                    format_patch,
                    files_modified,
                    tool_calls_made: result.tool_calls_made,
                    iterations: result.iterations,
                    model_used: queued.model,
                };
                self.set_status(
                    &task_id,
                    TaskStatus::Completed {
                        finished_at: Utc::now(),
                        result: task_result,
                    },
                )
                .await;
            }
            Err(e) => {
                let partial_changes = changes_patch;
                let (tool_calls_slice, conv_slice, iterations_completed, diag_state) =
                    e.diagnostics();
                let tool_call_history: Vec<ToolCallRecord> = tool_calls_slice.to_vec();
                let conversation_snapshot: Vec<ChatMessage> = conv_slice.to_vec();
                let last_agent_state = Some(format!("{:?}", diag_state));
                let last_tool_call = tool_call_history.last().cloned();
                let diagnostics = FailureDiagnostics {
                    error_type: agent_error_type(&e).to_string(),
                    iterations_completed,
                    last_tool_call,
                    partial_changes,
                    tool_call_history,
                    last_agent_state,
                    conversation_snapshot: Some(conversation_snapshot),
                };
                self.set_status(
                    &task_id,
                    TaskStatus::Failed {
                        finished_at: Utc::now(),
                        error: e.to_string(),
                        diagnostics,
                    },
                )
                .await;
            }
        }
    }
}

/// Stable `error_type` label for an agent failure.
fn agent_error_type(error: &AgentError) -> &'static str {
    match error {
        AgentError::MaxIterationsExceeded { .. } => "MaxIterationsExceeded",
        AgentError::StateError { .. } => "StateError",
        AgentError::TaskCheckFailed { .. } => "TaskCheckFailed",
    }
}

/// Drop an empty patch and truncate one larger than `MAX_DIFF_BYTES`.
fn bound_patch(patch: String) -> Option<String> {
    if patch.is_empty() {
        None
    } else if patch.len() > MAX_DIFF_BYTES {
        Some(patch[..MAX_DIFF_BYTES].to_string())
    } else {
        Some(patch)
    }
}

fn parse_modified_files(diff: Option<&str>) -> Vec<String> {
    let Some(diff) = diff else {
        return vec![];
    };
    diff.lines()
        .filter(|line| line.starts_with("+++ b/"))
        .map(|line| line.trim_start_matches("+++ b/").to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentConfig, AgentContext, AgentLoop};
    use crate::entities::InMemoryEntityStore;
    use crate::tools::{EchoTool, ToolRegistry};
    use async_trait::async_trait;
    use model::provider::{ModelError, ModelResult};
    use model::types::{
        ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, MessageRole, ModelInfo,
    };
    use std::sync::Mutex;

    pub(super) struct MockProvider {
        responses: Mutex<Vec<ChatResponse>>,
    }

    impl MockProvider {
        pub(super) fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses),
            })
        }
    }

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err(ModelError::Unknown {
                    message: "No more responses".to_string(),
                });
            }
            Ok(responses.remove(0))
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "mock"
        }
    }

    pub(super) fn stop_response(content: &str) -> ChatResponse {
        ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: MessageRole::Assistant,
                    content: Some(content.to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some(FinishReason::Stop),
            }],
            usage: None,
        }
    }

    /// Wrap tool-loop responses with state machine responses for plan/perform/check.
    fn wrap_with_state_machine_responses(tool_responses: Vec<ChatResponse>) -> Vec<ChatResponse> {
        // EnrichingEntities: no LLM call
        let mut responses = vec![
            stop_response("Plan: execute the task"), // PlanningEntityModification
        ];
        responses.extend(tool_responses); // PerformingEntityModification
                                          // UpdatingEntities: no LLM call
        responses.push(stop_response("COMPLETE - task done")); // CheckingTaskCompletion
        responses
    }

    #[test]
    fn test_task_id_uniqueness() {
        let id1 = TaskId::new();
        let id2 = TaskId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_task_result_to_json() {
        let result = TaskResult {
            result_summary: "Done".to_string(),
            changes_patch: Some("diff --git a/foo".to_string()),
            format_patch: Some("From abc Mon Sep 17 00:00:00 2001\n".to_string()),
            files_modified: vec!["foo.rs".to_string()],
            tool_calls_made: vec![],
            iterations: 3,
            model_used: "qwen3:0.6b".to_string(),
        };
        let json = result.to_json();
        assert_eq!(json["result_summary"], "Done");
        assert_eq!(json["iterations"], 3);
        assert!(json["changes_patch"].is_string());
        assert!(json["format_patch"].is_string());
    }

    #[test]
    fn test_failure_diagnostics_to_json() {
        let diag = FailureDiagnostics {
            error_type: "MaxIterationsExceeded".to_string(),
            iterations_completed: 100,
            last_tool_call: None,
            partial_changes: None,
            tool_call_history: vec![],
            last_agent_state: None,
            conversation_snapshot: None,
        };
        let json = diag.to_json();
        assert_eq!(json["error_type"], "MaxIterationsExceeded");
        assert_eq!(json["iterations_completed"], 100);
    }

    #[test]
    fn test_failure_diagnostics_with_full_context() {
        use crate::entities::context::types::ToolCallRecord;

        let tool_call = ToolCallRecord {
            tool_name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
            call_id: "call_1".to_string(),
            result: "fn main() {}".to_string(),
        };
        let diag = FailureDiagnostics {
            error_type: "StateError".to_string(),
            iterations_completed: 5,
            last_tool_call: Some(tool_call.clone()),
            partial_changes: Some("diff --git a/foo".to_string()),
            tool_call_history: vec![tool_call],
            last_agent_state: Some("Performing".to_string()),
            conversation_snapshot: Some(vec![ChatMessage::user("do something")]),
        };
        let json = diag.to_json();
        assert_eq!(json["error_type"], "StateError");
        assert_eq!(json["iterations_completed"], 5);
        assert!(json["last_tool_call"].is_object());
        assert_eq!(json["tool_call_history"].as_array().unwrap().len(), 1);
        assert_eq!(json["last_agent_state"], "Performing");
        assert!(json["conversation_snapshot"].is_array());
    }

    #[test]
    fn test_parse_modified_files_from_diff() {
        let diff = "+++ b/src/main.rs\n+++ b/src/lib.rs\n--- a/src/main.rs\n";
        let files = parse_modified_files(Some(diff));
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"src/lib.rs".to_string()));
    }

    #[test]
    fn test_agent_error_type_labels_every_variant() {
        use crate::agent::AgentState;
        let state_error = AgentError::StateError {
            message: "m".to_string(),
            iterations_completed: 1,
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            last_agent_state: AgentState::PlanningEntityModification,
        };
        let check_failed = AgentError::TaskCheckFailed {
            message: "m".to_string(),
            iterations_completed: 2,
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            last_agent_state: AgentState::CheckingTaskCompletion,
        };
        let exceeded = AgentError::MaxIterationsExceeded {
            iterations_completed: 3,
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            last_agent_state: AgentState::PerformingEntityModification,
        };
        assert_eq!(agent_error_type(&state_error), "StateError");
        assert_eq!(agent_error_type(&check_failed), "TaskCheckFailed");
        assert_eq!(agent_error_type(&exceeded), "MaxIterationsExceeded");
        assert_eq!(state_error.diagnostics().2, 1);
        assert_eq!(check_failed.diagnostics().2, 2);
        assert_eq!(exceeded.diagnostics().2, 3);
    }

    #[test]
    fn test_bound_patch_drops_empty_and_truncates_large() {
        assert_eq!(bound_patch(String::new()), None);
        assert_eq!(bound_patch("+x".to_string()).as_deref(), Some("+x"));
        let huge = "a".repeat(MAX_DIFF_BYTES + 10);
        assert_eq!(bound_patch(huge).unwrap().len(), MAX_DIFF_BYTES);
    }

    #[test]
    fn test_parse_modified_files_empty_diff() {
        let files = parse_modified_files(None);
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn test_task_manager_poll_returns_none_for_invalid_id() {
        let manager = TaskManager::default();
        let result = manager.poll(&TaskId("nonexistent".to_string())).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_task_manager_list_empty() {
        let manager = TaskManager::default();
        let tasks = manager.list().await;
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_cancel_nonexistent_task() {
        let manager = TaskManager::default();
        let result = manager.cancel(&TaskId("nonexistent".to_string())).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn test_cancel_running_task() {
        let manager = TaskManager::default();
        let task_id = TaskId::new();
        let task = Task {
            id: task_id.clone(),
            description: "test".to_string(),
            repo_path: PathBuf::from("/tmp"),
            branch: "HEAD".to_string(),
            model: "mock".to_string(),
            status: TaskStatus::Running {
                started_at: Utc::now(),
                iterations: 0,
            },
            created_at: Utc::now(),
            last_updated_at: Utc::now(),
            ttl_ms: None,
            not_before: None,
            identity_hint: None,
            origin: None,
        };
        {
            let mut tasks = manager.runner.tasks.write().await;
            tasks.insert(task_id.clone(), task);
        }
        let result = manager.cancel(&task_id).await;
        assert!(result.is_ok());
        let task = result.unwrap();
        assert!(matches!(&task.status, TaskStatus::Cancelled { .. }));
    }

    #[tokio::test]
    async fn test_cancel_pending_task() {
        let manager = TaskManager::new(0);
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let id = manager
            .submit(
                "test".to_string(),
                PathBuf::from("/tmp"),
                "HEAD".to_string(),
                "mock".to_string(),
                1,
                provider,
            )
            .await;

        tokio::task::yield_now().await;

        let result = manager.cancel(&id).await;
        assert!(result.is_ok());
        let task = result.unwrap();
        assert!(matches!(&task.status, TaskStatus::Cancelled { .. }));
    }

    #[tokio::test]
    async fn test_concurrency_limit_keeps_tasks_pending() {
        let manager = TaskManager::new(0);
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let id = manager
            .submit(
                "test".to_string(),
                PathBuf::from("/tmp"),
                "HEAD".to_string(),
                "mock".to_string(),
                1,
                provider,
            )
            .await;

        tokio::task::yield_now().await;

        let task = manager.poll(&id).await.unwrap();
        assert!(matches!(task.status, TaskStatus::Pending));
    }

    #[tokio::test]
    async fn test_agent_completes_with_mock_provider() {
        let provider: Arc<dyn ModelProvider> = MockProvider::new(
            wrap_with_state_machine_responses(vec![stop_response("Task complete!")]),
        );
        let config = AgentConfig {
            max_iterations: 20,
            ..Default::default()
        };
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);
        let context = AgentContext {
            user_prompt: "Test task".to_string(),
            conversation_history: vec![ChatMessage::user("Test task")],
            app_state_id: "test".to_string(),
        };
        let result = agent.run(context).await.unwrap();
        assert!(result.task_completed);
    }

    #[tokio::test]
    async fn test_agent_fails_with_max_iterations() {
        let responses: Vec<ChatResponse> = (0..5).map(|_| stop_response("not done yet")).collect();
        let provider: Arc<dyn ModelProvider> = MockProvider::new(responses);
        let config = AgentConfig {
            max_iterations: 0,
            ..Default::default()
        };
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);
        let context = AgentContext {
            user_prompt: "Test task".to_string(),
            conversation_history: vec![ChatMessage::user("Test task")],
            app_state_id: "test".to_string(),
        };
        let result = agent.run(context).await;
        assert!(matches!(
            result,
            Err(AgentError::MaxIterationsExceeded { .. })
        ));
    }

    #[tokio::test]
    async fn test_progress_counter_accessible_after_run() {
        let counter = Arc::new(AtomicUsize::new(0));
        let provider: Arc<dyn ModelProvider> = MockProvider::new(
            wrap_with_state_machine_responses(vec![stop_response("Task complete!")]),
        );
        let config = AgentConfig {
            max_iterations: 20,
            ..Default::default()
        };
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);
        agent.set_progress_counter(Arc::clone(&counter));
        let context = AgentContext {
            user_prompt: "Test task".to_string(),
            conversation_history: vec![ChatMessage::user("Test task")],
            app_state_id: "test".to_string(),
        };
        let result = agent.run(context).await.unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), result.iterations);
    }

    #[tokio::test]
    async fn test_submit_records_workspace_creation_failure() {
        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let provider: Arc<dyn ModelProvider> =
            MockProvider::new(vec![stop_response("Task complete!")]);

        let nonexistent_repo = PathBuf::from("/nonexistent/repo/path/that/does/not/exist");
        let task_id = manager
            .submit(
                "Test task".to_string(),
                nonexistent_repo,
                "HEAD".to_string(),
                "test-model".to_string(),
                10,
                provider,
            )
            .await;

        let deadline = std::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            let task = manager.poll(&task_id).await.unwrap();
            if !matches!(
                task.status,
                TaskStatus::Pending | TaskStatus::Running { .. }
            ) {
                assert!(matches!(task.status, TaskStatus::Failed { .. }));
                if let TaskStatus::Failed { diagnostics, .. } = &task.status {
                    assert_eq!(diagnostics.error_type, "WorkspaceCreationFailed");
                }
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "task did not complete"
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn test_submit_records_container_setup_failure() {
        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let provider: Arc<dyn ModelProvider> =
            MockProvider::new(vec![stop_response("Task complete!")]);

        let repo_dir = tempfile::tempdir().unwrap();
        std::fs::write(repo_dir.path().join("flake.nix"), "{}").unwrap();
        // Both flake.nix and .devcontainer/ must exist to trigger the container path.
        std::fs::create_dir(repo_dir.path().join(".devcontainer")).unwrap();

        let task_id = manager
            .submit(
                "Test task".to_string(),
                repo_dir.path().to_path_buf(),
                "HEAD".to_string(),
                "test-model".to_string(),
                10,
                provider,
            )
            .await;

        let deadline = std::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            let task = manager.poll(&task_id).await.unwrap();
            if !matches!(
                task.status,
                TaskStatus::Pending | TaskStatus::Running { .. }
            ) {
                assert!(matches!(task.status, TaskStatus::Failed { .. }));
                if let TaskStatus::Failed { diagnostics, .. } = &task.status {
                    assert_eq!(diagnostics.error_type, "ContainerSetupFailed");
                }
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "task did not complete"
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    // ---- get_or_build_image_using unit tests ----

    fn make_build_locks() -> BuildLocks {
        // BuildLocks uses tokio::sync::Mutex; use the fully-qualified path to
        // avoid the std::sync::Mutex that is imported for MockProvider above.
        Arc::new(tokio::sync::Mutex::new(HashMap::new()))
    }

    #[tokio::test]
    async fn test_get_or_build_image_using_success() {
        let cache: Arc<RwLock<HashMap<PathBuf, String>>> = Arc::new(RwLock::new(HashMap::new()));
        let build_locks = make_build_locks();
        let dir = tempfile::tempdir().unwrap();

        let result =
            TaskManager::get_or_build_image_using(&cache, &build_locks, dir.path(), |_source| {
                Ok("built-image:v1".to_string())
            })
            .await;

        assert_eq!(result.unwrap(), "built-image:v1");

        // Cache should be populated.
        let canonical = dir.path().canonicalize().unwrap();
        let cache_read = cache.read().await;
        assert_eq!(
            cache_read.get(&canonical).map(String::as_str),
            Some("built-image:v1")
        );

        // Build lock entry should be cleaned up.
        let locks = build_locks.lock().await;
        assert!(!locks.contains_key(&canonical));
    }

    #[tokio::test]
    async fn test_get_or_build_image_using_cache_hit() {
        let cache: Arc<RwLock<HashMap<PathBuf, String>>> = Arc::new(RwLock::new(HashMap::new()));
        let build_locks = make_build_locks();
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().canonicalize().unwrap();

        // Pre-populate the cache to trigger the fast-path return.
        cache
            .write()
            .await
            .insert(canonical, "cached-image:fast".to_string());

        let result =
            TaskManager::get_or_build_image_using(&cache, &build_locks, dir.path(), |_| {
                panic!("build_fn must not be called on a cache hit")
            })
            .await;

        assert_eq!(result.unwrap(), "cached-image:fast");
    }

    #[tokio::test]
    async fn test_get_or_build_image_using_second_check_cache_hit() {
        // Populate the cache WHILE holding the per-path lock, then release.
        // Any concurrent call that gets past the first cache check will block
        // on the lock; once released it sees the cached value on the second
        // check.  Calls that hit the first check also return the cached value.
        // Either way build_fn is never called — no sleep needed; the ordering
        // guarantee comes from lock acquisition, not timing.
        let cache: Arc<RwLock<HashMap<PathBuf, String>>> = Arc::new(RwLock::new(HashMap::new()));
        let build_locks = make_build_locks();
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().canonicalize().unwrap();

        let path_lock: Arc<tokio::sync::Mutex<()>> = Arc::new(tokio::sync::Mutex::new(()));
        {
            let mut locks = build_locks.lock().await;
            locks.insert(canonical.clone(), Arc::clone(&path_lock));
        }
        // Hold the per-path lock and populate the cache before spawning.
        let held = path_lock.lock().await;
        cache
            .write()
            .await
            .insert(canonical, "second-check-hit:latest".to_string());

        let cache_clone = Arc::clone(&cache);
        let build_locks_clone = Arc::clone(&build_locks);
        let dir_path = dir.path().to_path_buf();

        let handle = tokio::spawn(async move {
            TaskManager::get_or_build_image_using(
                &cache_clone,
                &build_locks_clone,
                &dir_path,
                |_| panic!("build_fn must not be called on a cache hit"),
            )
            .await
        });

        // Release the lock so the task can proceed if it was blocked.
        drop(held);

        let result = handle.await.unwrap();
        assert_eq!(result.unwrap(), "second-check-hit:latest");
    }

    #[tokio::test]
    async fn test_get_or_build_image_using_build_failure() {
        let cache: Arc<RwLock<HashMap<PathBuf, String>>> = Arc::new(RwLock::new(HashMap::new()));
        let build_locks = make_build_locks();
        let dir = tempfile::tempdir().unwrap();

        let result =
            TaskManager::get_or_build_image_using(&cache, &build_locks, dir.path(), |_| {
                Err("build exploded".to_string())
            })
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "build exploded");

        // Cache must remain empty after a build failure.
        let canonical = dir.path().canonicalize().unwrap();
        let cache_read = cache.read().await;
        assert!(cache_read.get(&canonical).is_none());
    }

    // ---- build_task_system_prompt unit tests ----

    /// Install a process-global tracing subscriber once so the info/error
    /// macro bodies in `build_task_system_prompt` actually execute under
    /// coverage. Without a subscriber at a live level the tracing crate
    /// short-circuits before evaluating the field expressions, leaving lines
    /// inside the macro uncovered.
    fn ensure_tracing_subscriber() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_test_writer()
                .with_max_level(tracing::Level::TRACE)
                .try_init();
        });
    }

    #[test]
    fn test_build_task_system_prompt_no_guidance_returns_default() {
        ensure_tracing_subscriber();
        let dir = tempfile::tempdir().unwrap();
        let prompt = build_task_system_prompt(dir.path());
        assert_eq!(prompt, DEFAULT_TASK_SYSTEM_PROMPT);
    }

    #[test]
    fn test_build_task_system_prompt_appends_agents_md() {
        ensure_tracing_subscriber();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "# Repo rules\nUse nextest.\n").unwrap();
        let prompt = build_task_system_prompt(dir.path());
        assert!(prompt.starts_with(DEFAULT_TASK_SYSTEM_PROMPT));
        assert!(prompt.contains("<repo-guidance source=\"AGENTS.md\">"));
        assert!(prompt.contains("Use nextest."));
        assert!(prompt.contains("</repo-guidance>"));
    }

    #[test]
    fn test_build_task_system_prompt_appends_claude_md_fallback() {
        ensure_tracing_subscriber();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "legacy rules").unwrap();
        let prompt = build_task_system_prompt(dir.path());
        assert!(prompt.starts_with(DEFAULT_TASK_SYSTEM_PROMPT));
        assert!(prompt.contains("<repo-guidance source=\"CLAUDE.md\">"));
        assert!(prompt.contains("legacy rules"));
    }

    /// Initialise a temporary directory as a git repo with a single initial
    /// commit so it can be used as the source repository for
    /// `TaskManager::submit` in tests. Mirrors `workspace::tests::init_git_repo`.
    pub(super) fn init_test_git_repo(dir: &std::path::Path) {
        for args in &[
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
        }
        std::fs::write(dir.join("README.md"), "# Test").unwrap();
        std::process::Command::new("git")
            .current_dir(dir)
            .args(["add", "."])
            .output()
            .unwrap();
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["commit", "-m", "init"])
            .output()
            .unwrap();
        assert!(out.status.success(), "init commit failed");
    }

    #[tokio::test]
    async fn test_submit_injects_agents_md_into_task_prompt() {
        // Exercises the production call site of `build_task_system_prompt`
        // (inside the `Ok(mut workspace)` branch of `submit`) so the
        // AGENTS.md-injection path is covered end-to-end, not just by the
        // direct unit tests above.
        ensure_tracing_subscriber();

        let repo_dir = tempfile::tempdir().unwrap();
        init_test_git_repo(repo_dir.path());
        std::fs::write(repo_dir.path().join("AGENTS.md"), "# repo rules\n").unwrap();
        std::process::Command::new("git")
            .current_dir(repo_dir.path())
            .args(["add", "AGENTS.md"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo_dir.path())
            .args(["commit", "-m", "add guidance"])
            .output()
            .unwrap();

        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let provider: Arc<dyn ModelProvider> = MockProvider::new(
            wrap_with_state_machine_responses(vec![stop_response("done")]),
        );
        let task_id = manager
            .submit(
                "Test".to_string(),
                repo_dir.path().to_path_buf(),
                "HEAD".to_string(),
                "mock".to_string(),
                20,
                provider,
            )
            .await;

        let deadline = std::time::Instant::now() + tokio::time::Duration::from_secs(10);
        loop {
            let task = manager.poll(&task_id).await.unwrap();
            if matches!(
                task.status,
                TaskStatus::Completed { .. } | TaskStatus::Failed { .. }
            ) {
                assert!(
                    matches!(task.status, TaskStatus::Completed { .. }),
                    "expected task to complete successfully, got {:?}",
                    task.status
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "submit task did not finish"
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn test_build_task_system_prompt_swallows_read_errors() {
        // Non-UTF8 AGENTS.md makes the loader return Err; the prompt builder
        // must log and fall back to the default system prompt without
        // propagating the error.
        ensure_tracing_subscriber();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), [0x48u8, 0xFFu8, 0x49u8]).unwrap();
        let prompt = build_task_system_prompt(dir.path());
        assert_eq!(prompt, DEFAULT_TASK_SYSTEM_PROMPT);
    }

    #[tokio::test]
    async fn test_submit_container_path_workspace_fail_with_cached_image() {
        // Inject a pre-built image into the cache so that get_or_build_image
        // returns immediately, then verify that a subsequent workspace-creation
        // failure (non-git directory) is correctly recorded as
        // WorkspaceCreationFailed.
        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let provider: Arc<dyn ModelProvider> =
            MockProvider::new(vec![stop_response("Task complete!")]);

        let repo_dir = tempfile::tempdir().unwrap();
        std::fs::write(repo_dir.path().join("flake.nix"), "{}").unwrap();
        std::fs::create_dir(repo_dir.path().join(".devcontainer")).unwrap();

        // Pre-populate the image cache so get_or_build_image does not try to
        // run nix.
        let canonical = repo_dir.path().canonicalize().unwrap();
        {
            let mut cache = manager.runner.image_cache.write().await;
            cache.insert(canonical, "pre-built:latest".to_string());
        }

        let task_id = manager
            .submit(
                "Test task".to_string(),
                repo_dir.path().to_path_buf(),
                "HEAD".to_string(),
                "test-model".to_string(),
                10,
                provider,
            )
            .await;

        let deadline = std::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            let task = manager.poll(&task_id).await.unwrap();
            if !matches!(
                task.status,
                TaskStatus::Pending | TaskStatus::Running { .. }
            ) {
                assert!(matches!(task.status, TaskStatus::Failed { .. }));
                // repo_dir is not a git repo so the worktree creation fails,
                // which is reported as WorkspaceCreationFailed.
                if let TaskStatus::Failed { diagnostics, .. } = &task.status {
                    assert_eq!(diagnostics.error_type, "WorkspaceCreationFailed");
                }
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "task did not complete within 5 s"
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn test_wait_terminal_unknown_id_returns_none() {
        let manager = TaskManager::default();
        let unknown = TaskId("does-not-exist".to_string());
        assert!(manager.wait_terminal(&unknown).await.is_none());
    }

    #[tokio::test]
    async fn test_wait_terminal_returns_terminal_status() {
        // Zero permits keeps the task queued; cancelling drives it terminal and
        // `wait_terminal` observes the `Cancelled` transition.
        let manager = Arc::new(TaskManager::new(0));
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let id = manager
            .submit(
                "t".to_string(),
                PathBuf::from("/tmp"),
                "HEAD".to_string(),
                "mock".to_string(),
                1,
                provider,
            )
            .await;
        let m = Arc::clone(&manager);
        let idc = id.clone();
        let waiter = tokio::spawn(async move { m.wait_terminal(&idc).await });
        tokio::task::yield_now().await;
        manager.cancel(&id).await.unwrap();
        let status = waiter.await.unwrap();
        assert!(matches!(status, Some(TaskStatus::Cancelled { .. })));
    }

    #[tokio::test]
    async fn test_wait_terminal_falls_back_to_store_if_sender_dropped() {
        // Exercises the defensive fallback: if the status watch sender is
        // dropped while a waiter is blocked, `wait_terminal` returns the last
        // stored status instead of hanging.
        let manager = Arc::new(TaskManager::new(0));
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let id = manager
            .submit(
                "t".to_string(),
                PathBuf::from("/tmp"),
                "HEAD".to_string(),
                "mock".to_string(),
                1,
                provider,
            )
            .await;
        let m = Arc::clone(&manager);
        let idc = id.clone();
        let waiter = tokio::spawn(async move { m.wait_terminal(&idc).await });
        tokio::task::yield_now().await;
        // Drop the sender (and its keep-alive receiver) out from under the waiter.
        {
            let mut senders = manager.runner.status_senders.write().await;
            senders.remove(&id);
        }
        let status = waiter.await.unwrap();
        assert!(matches!(status, Some(TaskStatus::Pending)));
    }

    #[tokio::test]
    async fn test_set_ttl_updates_task() {
        let manager = Arc::new(TaskManager::new(0));
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let id = manager
            .submit(
                "t".to_string(),
                PathBuf::from("/tmp"),
                "HEAD".to_string(),
                "mock".to_string(),
                1,
                provider,
            )
            .await;
        manager.set_ttl(&id, Some(1234)).await;
        assert_eq!(manager.poll(&id).await.unwrap().ttl_ms, Some(1234));
        // set_ttl on an unknown id is a no-op (does not panic).
        manager.set_ttl(&TaskId("nope".to_string()), Some(1)).await;
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::tests::{stop_response, MockProvider};
    use super::*;
    use crate::scheduler::{InMemoryQueueStore, JsonlQueueStore, QueueStore, QueueStoreError};
    use async_trait::async_trait;
    use model::provider::{ModelError, ModelResult};
    use model::types::{ChatRequest, ChatResponse, ModelInfo};
    use std::path::Path;

    struct GatedProvider {
        gate: watch::Receiver<bool>,
    }

    #[async_trait]
    impl ModelProvider for GatedProvider {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            let mut gate = self.gate.clone();
            gate.wait_for(|open| *open)
                .await
                .map_err(|e| ModelError::Unknown {
                    message: e.to_string(),
                })?;
            Ok(stop_response("COMPLETE - task done"))
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "gated"
        }
    }

    struct RejectingStore;

    impl QueueStore for RejectingStore {
        fn load(&self) -> Result<Vec<QueuedTask>, QueueStoreError> {
            Ok(vec![])
        }
        fn insert(&self, _task: &QueuedTask) -> Result<(), QueueStoreError> {
            Err(QueueStoreError::Rejected("disk full".to_string()))
        }
        fn remove(&self, _id: &TaskId) -> Result<(), QueueStoreError> {
            Ok(())
        }
    }

    fn git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        super::tests::init_test_git_repo(dir.path());
        dir
    }

    async fn wait_for(
        manager: &TaskManager,
        id: &TaskId,
        pred: impl Fn(&TaskStatus) -> bool,
    ) -> Task {
        let mut last = None;
        for _ in 0..200 {
            let task = manager.poll(id).await.unwrap();
            if pred(&task.status) {
                return task;
            }
            last = Some(task.status);
            tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
        }
        panic!("task {id} did not reach the expected status within 5s; last status: {last:?}");
    }

    fn queued(repo: &std::path::Path) -> QueuedTask {
        QueuedTask::new("task", repo.to_path_buf(), "HEAD", "mock", 10)
    }

    #[tokio::test]
    async fn test_queued_task_starts_after_completion() {
        let repo = git_repo();
        let manager = TaskManager::new(1);
        let (open, gate) = watch::channel(false);
        let provider: Arc<dyn ModelProvider> = Arc::new(GatedProvider { gate });
        let first = manager
            .submit_task(queued(repo.path()), Arc::clone(&provider))
            .await;
        let second = manager
            .submit_task(queued(repo.path()), Arc::clone(&provider))
            .await;

        wait_for(&manager, &first, |s| {
            matches!(s, TaskStatus::Running { .. })
        })
        .await;
        assert!(matches!(
            manager.poll(&second).await.unwrap().status,
            TaskStatus::Pending
        ));
        let metrics = manager.queue_metrics().await;
        assert_eq!(metrics.queued, 1);
        assert_eq!(metrics.running, 1);
        assert_eq!(metrics.dispatched_newest, 1);

        open.send(true).unwrap();
        let done = wait_for(&manager, &first, TaskStatus::is_terminal).await;
        assert!(matches!(done.status, TaskStatus::Completed { .. }));
        let done = wait_for(&manager, &second, TaskStatus::is_terminal).await;
        assert!(matches!(done.status, TaskStatus::Completed { .. }));
        assert!(manager.get_result(&second).await.is_some());
        let metrics = manager.queue_metrics().await;
        assert_eq!(metrics.queued, 0);
        assert_eq!(metrics.running, 0);
        assert_eq!(metrics.dispatched_newest + metrics.dispatched_oldest, 2);
    }

    #[tokio::test]
    async fn test_agent_failure_records_diagnostics_and_no_result() {
        let repo = git_repo();
        let manager = TaskManager::new(1);
        let (open, gate) = watch::channel(true);
        let provider: Arc<dyn ModelProvider> = Arc::new(GatedProvider { gate });
        let mut entry = queued(repo.path());
        entry.max_iterations = 1;
        let id = manager.submit_task(entry, provider).await;
        let done = wait_for(&manager, &id, TaskStatus::is_terminal).await;
        match done.status {
            TaskStatus::Failed { diagnostics, .. } => {
                assert_eq!(diagnostics.error_type, "MaxIterationsExceeded");
                assert!(diagnostics.conversation_snapshot.is_some());
            }
            other => panic!("unexpected status {other:?}"),
        }
        assert!(manager.get_result(&id).await.is_none());
        assert!(manager
            .get_result(&TaskId("missing".to_string()))
            .await
            .is_none());
        drop(open);
    }

    #[tokio::test]
    async fn test_restore_requeues_tasks_from_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.jsonl");
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let first = TaskManager::restore(
            0,
            Box::new(HybridPolicy::default()),
            Box::new(JsonlQueueStore::open(&path).unwrap()),
            Arc::clone(&provider),
        )
        .await
        .unwrap();
        let a = first
            .submit_task(
                queued(Path::new("/nonexistent")).with_identity_hint(Some("dev".to_string())),
                Arc::clone(&provider),
            )
            .await;
        let b = first
            .submit(
                "b".to_string(),
                PathBuf::from("/nonexistent"),
                "HEAD".to_string(),
                "mock".to_string(),
                1,
                Arc::clone(&provider),
            )
            .await;
        assert_eq!(first.queue_metrics().await.queued, 2);
        drop(first);

        let second = TaskManager::restore(
            2,
            Box::new(HybridPolicy::default()),
            Box::new(JsonlQueueStore::open(&path).unwrap()),
            Arc::clone(&provider),
        )
        .await
        .unwrap();
        let mut ids: Vec<TaskId> = second.list().await.into_iter().map(|t| t.id).collect();
        ids.sort_by(|x, y| x.0.cmp(&y.0));
        let mut expected = vec![a.clone(), b.clone()];
        expected.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(ids, expected);
        assert_eq!(
            second.poll(&a).await.unwrap().identity_hint.as_deref(),
            Some("dev")
        );
        for id in [&a, &b] {
            let done = wait_for(&second, id, TaskStatus::is_terminal).await;
            assert!(matches!(done.status, TaskStatus::Failed { .. }));
        }
        assert!(JsonlQueueStore::open(&path)
            .unwrap()
            .load()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_restore_propagates_store_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.jsonl");
        std::fs::write(&path, "garbage\n").unwrap();
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let result = TaskManager::restore(
            1,
            Box::new(HybridPolicy::default()),
            Box::new(JsonlQueueStore::open(&path).unwrap()),
            provider,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_parked_task_does_not_consume_slot() {
        let manager = TaskManager::new(1);
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let later = Utc::now() + chrono::Duration::hours(1);
        let parked = manager
            .submit_task(
                queued(Path::new("/nonexistent")).with_not_before(Some(later)),
                Arc::clone(&provider),
            )
            .await;
        let ready = manager
            .submit_task(queued(Path::new("/nonexistent")), provider)
            .await;
        wait_for(&manager, &ready, TaskStatus::is_terminal).await;
        let parked_task = manager.poll(&parked).await.unwrap();
        assert!(matches!(parked_task.status, TaskStatus::Pending));
        assert_eq!(parked_task.not_before, Some(later));
        let metrics = manager.queue_metrics().await;
        assert_eq!(metrics.queued, 1);
        assert_eq!(metrics.parked, 1);
        assert_eq!(metrics.running, 0);
        let cancelled = manager.cancel(&parked).await.unwrap();
        assert!(matches!(cancelled.status, TaskStatus::Cancelled { .. }));
        assert_eq!(manager.queue_metrics().await.queued, 0);
    }

    #[tokio::test]
    async fn test_submit_records_queue_persist_failure() {
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let manager = TaskManager::restore(
            1,
            Box::new(HybridPolicy::default()),
            Box::new(RejectingStore),
            Arc::clone(&provider),
        )
        .await
        .unwrap();
        let id = manager
            .submit_task(queued(Path::new("/nonexistent")), provider)
            .await;
        let task = manager.poll(&id).await.unwrap();
        match task.status {
            TaskStatus::Failed {
                error, diagnostics, ..
            } => {
                assert_eq!(diagnostics.error_type, "QueuePersistFailed");
                assert!(error.contains("disk full"));
            }
            other => panic!("unexpected status {other:?}"),
        }
        assert!(manager.cancel(&id).await.is_err());
    }

    #[tokio::test]
    async fn test_dispatched_task_without_provider_fails() {
        let manager = TaskManager::new(1);
        let entry = queued(Path::new("/nonexistent"));
        manager.runner.register(&entry, None).await;
        manager.dispatcher.enqueue(entry.clone()).await.unwrap();
        let task = wait_for(&manager, &entry.id, TaskStatus::is_terminal).await;
        match task.status {
            TaskStatus::Failed { diagnostics, .. } => {
                assert_eq!(diagnostics.error_type, "NoProvider");
            }
            other => panic!("unexpected status {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_cancel_queued_task_removes_it_from_store() {
        let store = InMemoryQueueStore::default();
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![stop_response("unused")]);
        let manager = TaskManager::restore(
            0,
            Box::new(HybridPolicy::default()),
            Box::new(store.clone()),
            Arc::clone(&provider),
        )
        .await
        .unwrap();
        let id = manager
            .submit_task(queued(Path::new("/nonexistent")), provider)
            .await;
        assert_eq!(store.load().unwrap().len(), 1);
        let cancelled = manager.cancel(&id).await.unwrap();
        assert!(matches!(cancelled.status, TaskStatus::Cancelled { .. }));
        assert!(store.load().unwrap().is_empty());
        assert_eq!(
            manager.wait_terminal(&id).await.map(|s| s.is_terminal()),
            Some(true)
        );
    }
}
