use crate::agent::{AgentConfig, AgentContext, AgentError, AgentLoop, AgentRunResult};
use crate::container::NetworkPolicy;
use crate::effects::EffectClass;
use crate::entities::context::types::ToolCallRecord;
use crate::entities::InMemoryEntityStore;
use crate::identity::AgentIdentity;
use crate::protected::{AuditHook, NoopAuditHook, ProtectedPathViolation};
use crate::scope::ScopeDenial;
use crate::workspace::TaskWorkspace;
use crate::workspace::WorkspaceError;
use chrono::{DateTime, Utc};
use model::provider::ModelProvider;
use model::types::ChatMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{watch, Mutex, RwLock, Semaphore};
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
    /// Every call the identity scope refused during the run, in order.
    /// Empty when the task ran without an identity.
    #[serde(default)]
    pub denials: Vec<ScopeDenial>,
    pub iterations: usize,
    pub model_used: String,
}

impl TaskResult {
    /// Number of refused calls; repeated denials are the auditor's signal
    /// that the identity lacks a capability the task needs.
    pub fn denial_count(&self) -> usize {
        self.denials.len()
    }

    /// The widest blast radius any attributed tool call in this result
    /// reached, or `None` when no call carried an effect attribution.
    pub fn max_effect_class(&self) -> Option<EffectClass> {
        self.tool_calls_made
            .iter()
            .filter_map(|call| call.effect.as_ref().map(|effect| effect.class))
            .max()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "result_summary": self.result_summary,
            "changes_patch": self.changes_patch,
            "format_patch": self.format_patch,
            "files_modified": self.files_modified,
            "tool_calls_made": self.tool_calls_made,
            "max_effect_class": self.max_effect_class(),
            "denials": self.denials,
            "denial_count": self.denial_count(),
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
    #[serde(default)]
    pub denials: Vec<ScopeDenial>,
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
            "denials": self.denials,
        })
    }
}

fn protected_failure(
    violation: ProtectedPathViolation,
    identity: Option<&str>,
    run_result: &Result<AgentRunResult, AgentError>,
) -> (String, FailureDiagnostics) {
    let (iterations_completed, tool_call_history, mut denials) = match run_result {
        Ok(result) => (
            result.iterations,
            result.tool_calls_made.clone(),
            result.denials.clone(),
        ),
        Err(e) => (e.diagnostics().2, e.diagnostics().0.to_vec(), vec![]),
    };
    let mut denial = ScopeDenial::protected("extract_changes", &violation);
    if let Some(identity) = identity {
        denial.identity = identity.to_string();
    }
    denials.push(denial);
    let diagnostics = FailureDiagnostics {
        error_type: "ProtectedPathViolation".to_string(),
        iterations_completed,
        last_tool_call: tool_call_history.last().cloned(),
        partial_changes: None,
        tool_call_history,
        last_agent_state: None,
        conversation_snapshot: None,
        denials,
    };
    (violation.to_string(), diagnostics)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum TaskStatus {
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

pub struct TaskManager {
    tasks: Arc<RwLock<HashMap<TaskId, Task>>>,
    handles: Arc<RwLock<HashMap<TaskId, tokio::task::AbortHandle>>>,
    max_concurrent: Arc<Semaphore>,
    progress: Arc<RwLock<HashMap<TaskId, Arc<AtomicUsize>>>>,
    image_cache: Arc<RwLock<HashMap<PathBuf, String>>>,
    /// Per-repo-path mutex to prevent concurrent image builds for the same repo.
    build_locks: BuildLocks,
    status_senders: StatusSenders,
    audit: Arc<dyn AuditHook>,
}

impl TaskManager {
    pub fn new(max_concurrent_tasks: usize) -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            handles: Arc::new(RwLock::new(HashMap::new())),
            max_concurrent: Arc::new(Semaphore::new(max_concurrent_tasks)),
            progress: Arc::new(RwLock::new(HashMap::new())),
            image_cache: Arc::new(RwLock::new(HashMap::new())),
            build_locks: Arc::new(Mutex::new(HashMap::new())),
            status_senders: Arc::new(RwLock::new(HashMap::new())),
            audit: Arc::new(NoopAuditHook),
        }
    }

    /// Deliver protected-path violations from every task's workspace to `hook`.
    pub fn with_audit_hook(mut self, hook: Arc<dyn AuditHook>) -> Self {
        self.audit = hook;
        self
    }

    /// Transition a task to a new status: update the stored `Task` (status +
    /// `last_updated_at`) and broadcast the new status to any `wait_terminal`
    /// subscribers. This is the single choke point for status changes so the
    /// watch channel can never drift from the stored task.
    async fn set_status(
        tasks: &Arc<RwLock<HashMap<TaskId, Task>>>,
        senders: &StatusSenders,
        task_id: &TaskId,
        status: TaskStatus,
    ) {
        {
            let mut tasks = tasks.write().await;
            if let Some(task) = tasks.get_mut(task_id) {
                task.status = status.clone();
                task.last_updated_at = Utc::now();
            }
        }
        let senders = senders.read().await;
        if let Some((tx, _keepalive)) = senders.get(task_id) {
            let _ = tx.send(status);
        }
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

    pub async fn submit(
        &self,
        description: String,
        repo_path: PathBuf,
        branch: String,
        model: String,
        max_iterations: usize,
        provider: Arc<dyn ModelProvider>,
    ) -> TaskId {
        let identity = None;
        self.submit_with_identity(
            description,
            repo_path,
            branch,
            model,
            max_iterations,
            provider,
            identity,
        )
        .await
    }

    /// Submit a task that, when `identity` is present, runs under that
    /// identity's scope: the tool registry is
    /// [`TaskWorkspace::build_tool_registry_for`] the identity and a dev
    /// container gets [`NetworkPolicy::for_ceiling`] of its
    /// `scope.max_effect`. Without an identity this is exactly
    /// [`TaskManager::submit`].
    #[allow(clippy::too_many_arguments)]
    pub async fn submit_with_identity(
        &self,
        description: String,
        repo_path: PathBuf,
        branch: String,
        model: String,
        max_iterations: usize,
        provider: Arc<dyn ModelProvider>,
        identity: Option<AgentIdentity>,
    ) -> TaskId {
        let task_id = TaskId::new();
        let now = Utc::now();
        let task = Task {
            id: task_id.clone(),
            description: description.clone(),
            repo_path: repo_path.clone(),
            branch: branch.clone(),
            model: model.clone(),
            status: TaskStatus::Pending,
            created_at: now,
            last_updated_at: now,
            ttl_ms: None,
        };
        {
            let mut tasks = self.tasks.write().await;
            tasks.insert(task_id.clone(), task);
        }

        // Register the terminal-completion watch (seeded with `Pending`) so
        // `wait_terminal` callers can await this task's terminal transition.
        // The receiver is retained as a keep-alive (see `StatusSenders`).
        {
            let (tx, rx) = watch::channel(TaskStatus::Pending);
            let mut senders = self.status_senders.write().await;
            senders.insert(task_id.clone(), (tx, rx));
        }

        let progress_counter = Arc::new(AtomicUsize::new(0));
        {
            let mut progress = self.progress.write().await;
            progress.insert(task_id.clone(), Arc::clone(&progress_counter));
        }

        let tasks_ref = Arc::clone(&self.tasks);
        let handles_ref = Arc::clone(&self.handles);
        let progress_ref = Arc::clone(&self.progress);
        let senders_ref = Arc::clone(&self.status_senders);
        let audit_ref = Arc::clone(&self.audit);
        let semaphore = Arc::clone(&self.max_concurrent);
        let image_cache_ref = Arc::clone(&self.image_cache);
        let build_locks_ref = Arc::clone(&self.build_locks);
        let task_id_clone = task_id.clone();

        let mut handles_guard = self.handles.write().await;
        let join_handle = tokio::spawn(async move {
            let _permit = semaphore.acquire_owned().await.expect("Semaphore closed");

            // Require both flake.nix AND .devcontainer/ to opt in to the
            // container path, so that repos that merely happen to have a
            // flake.nix are not affected. Use tokio::fs to avoid blocking.
            let use_container = tokio::fs::try_exists(repo_path.join("flake.nix"))
                .await
                .unwrap_or(false)
                && tokio::fs::try_exists(repo_path.join(".devcontainer"))
                    .await
                    .unwrap_or(false);

            Self::set_status(
                &tasks_ref,
                &senders_ref,
                &task_id_clone,
                TaskStatus::Running {
                    started_at: Utc::now(),
                    iterations: 0,
                },
            )
            .await;

            let workspace_result = if use_container {
                let image_result =
                    Self::get_or_build_image(&image_cache_ref, &build_locks_ref, &repo_path).await;
                let image_ref = match image_result {
                    Ok(r) => r,
                    Err(e) => {
                        {
                            let mut h = handles_ref.write().await;
                            h.remove(&task_id_clone);
                        }
                        {
                            let mut p = progress_ref.write().await;
                            p.remove(&task_id_clone);
                        }
                        Self::set_status(
                            &tasks_ref,
                            &senders_ref,
                            &task_id_clone,
                            TaskStatus::Failed {
                                finished_at: Utc::now(),
                                error: e.clone(),
                                diagnostics: FailureDiagnostics {
                                    error_type: "ContainerSetupFailed".to_string(),
                                    iterations_completed: 0,
                                    last_tool_call: None,
                                    partial_changes: None,
                                    tool_call_history: vec![],
                                    last_agent_state: None,
                                    conversation_snapshot: None,
                                    denials: vec![],
                                },
                            },
                        )
                        .await;
                        return;
                    }
                };
                let network = network_policy_for(identity.as_ref());
                TaskWorkspace::create_with_container_networked(
                    &repo_path,
                    &task_id_clone.0,
                    &branch,
                    &image_ref,
                    network,
                )
                .await
                .map_err(|e| (e.to_string(), "WorkspaceCreationFailed"))
            } else {
                TaskWorkspace::create(&repo_path, &task_id_clone.0, &branch)
                    .map_err(|e| (e.to_string(), "WorkspaceCreationFailed"))
            };
            let workspace_result = workspace_result.map(|ws| ws.with_audit_hook(audit_ref));

            match workspace_result {
                Err((e, error_type)) => {
                    {
                        let mut handles = handles_ref.write().await;
                        handles.remove(&task_id_clone);
                    }
                    {
                        let mut progress = progress_ref.write().await;
                        progress.remove(&task_id_clone);
                    }
                    Self::set_status(
                        &tasks_ref,
                        &senders_ref,
                        &task_id_clone,
                        TaskStatus::Failed {
                            finished_at: Utc::now(),
                            error: e,
                            diagnostics: FailureDiagnostics {
                                error_type: error_type.to_string(),
                                iterations_completed: 0,
                                last_tool_call: None,
                                partial_changes: None,
                                tool_call_history: vec![],
                                last_agent_state: None,
                                conversation_snapshot: None,
                                denials: vec![],
                            },
                        },
                    )
                    .await;
                }
                Ok(mut workspace) => {
                    let tool_registry = match registry_for(&workspace, identity.as_ref()) {
                        Ok(registry) => registry,
                        Err(e) => {
                            let _ = workspace.cleanup();
                            {
                                let mut handles = handles_ref.write().await;
                                handles.remove(&task_id_clone);
                            }
                            {
                                let mut progress = progress_ref.write().await;
                                progress.remove(&task_id_clone);
                            }
                            Self::set_status(
                                &tasks_ref,
                                &senders_ref,
                                &task_id_clone,
                                TaskStatus::Failed {
                                    finished_at: Utc::now(),
                                    error: e.to_string(),
                                    diagnostics: FailureDiagnostics {
                                        error_type: "ScopeError".to_string(),
                                        iterations_completed: 0,
                                        last_tool_call: None,
                                        partial_changes: None,
                                        tool_call_history: vec![],
                                        last_agent_state: None,
                                        conversation_snapshot: None,
                                        denials: vec![],
                                    },
                                },
                            )
                            .await;
                            return;
                        }
                    };
                    let entity_store = InMemoryEntityStore::new();
                    let agent_config = AgentConfig {
                        max_iterations,
                        verbose: false,
                        system_prompt: build_task_system_prompt(&workspace.workspace_path),
                        model_name: model.clone(),
                    };
                    let context = AgentContext {
                        user_prompt: description.clone(),
                        conversation_history: vec![ChatMessage::user(&description)],
                        app_state_id: task_id_clone.0.clone(),
                    };

                    let mut agent =
                        AgentLoop::with_tools(agent_config, entity_store, provider, tool_registry);
                    agent.set_progress_counter(Arc::clone(&progress_counter));
                    let run_result = agent.run(context).await;

                    let extracted = workspace.extract_changes();
                    let changes_patch = extracted.as_ref().ok().and_then(|patch| {
                        if patch.is_empty() {
                            None
                        } else if patch.len() > MAX_DIFF_BYTES {
                            Some(patch[..MAX_DIFF_BYTES].to_string())
                        } else {
                            Some(patch.clone())
                        }
                    });

                    let format_patch = match &extracted {
                        Err(WorkspaceError::ProtectedPath(_)) => None,
                        _ => workspace.format_patch().ok().flatten(),
                    };

                    let _ = workspace.cleanup();

                    {
                        let mut handles = handles_ref.write().await;
                        handles.remove(&task_id_clone);
                    }
                    {
                        let mut progress = progress_ref.write().await;
                        progress.remove(&task_id_clone);
                    }

                    if let Err(WorkspaceError::ProtectedPath(violation)) = extracted {
                        let name = identity.as_ref().map(|i| i.name());
                        let (error, diagnostics) = protected_failure(violation, name, &run_result);
                        let finished_at = Utc::now();
                        let status = TaskStatus::Failed {
                            finished_at,
                            error,
                            diagnostics,
                        };
                        Self::set_status(&tasks_ref, &senders_ref, &task_id_clone, status).await;
                        return;
                    }

                    match run_result {
                        Ok(result) => {
                            let files_modified = parse_modified_files(changes_patch.as_deref());
                            let task_result = TaskResult {
                                result_summary: result.result_summary,
                                changes_patch,
                                format_patch,
                                files_modified,
                                tool_calls_made: result.tool_calls_made,
                                denials: result.denials,
                                iterations: result.iterations,
                                model_used: model,
                            };
                            Self::set_status(
                                &tasks_ref,
                                &senders_ref,
                                &task_id_clone,
                                TaskStatus::Completed {
                                    finished_at: Utc::now(),
                                    result: task_result,
                                },
                            )
                            .await;
                        }
                        Err(e) => {
                            let partial_changes = changes_patch;
                            let (tool_calls_slice, conv_slice, diag_iters, diag_state) =
                                e.diagnostics();
                            let tool_call_history: Vec<ToolCallRecord> = tool_calls_slice.to_vec();
                            let conversation_snapshot: Vec<ChatMessage> = conv_slice.to_vec();
                            let last_agent_state = Some(format!("{:?}", diag_state));
                            let last_tool_call = tool_call_history.last().cloned();
                            let (error_type, iterations_completed) = match &e {
                                AgentError::MaxIterationsExceeded {
                                    iterations_completed,
                                    ..
                                } => ("MaxIterationsExceeded".to_string(), *iterations_completed),
                                AgentError::StateError { .. } => {
                                    ("StateError".to_string(), diag_iters)
                                }
                                AgentError::TaskCheckFailed { .. } => {
                                    ("TaskCheckFailed".to_string(), diag_iters)
                                }
                            };
                            let diagnostics = FailureDiagnostics {
                                error_type,
                                iterations_completed,
                                last_tool_call,
                                partial_changes,
                                tool_call_history,
                                last_agent_state,
                                conversation_snapshot: Some(conversation_snapshot),
                                denials: vec![],
                            };
                            Self::set_status(
                                &tasks_ref,
                                &senders_ref,
                                &task_id_clone,
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
        });

        handles_guard.insert(task_id.clone(), join_handle.abort_handle());
        drop(handles_guard);

        task_id
    }

    pub async fn poll(&self, task_id: &TaskId) -> Option<Task> {
        let tasks = self.tasks.read().await;
        let mut task = tasks.get(task_id)?.clone();
        drop(tasks);

        if let TaskStatus::Running { started_at, .. } = task.status {
            let progress = self.progress.read().await;
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
        let tasks = self.tasks.read().await;
        tasks.get(task_id).and_then(|t| {
            if let TaskStatus::Completed { result, .. } = &t.status {
                Some(result.clone())
            } else {
                None
            }
        })
    }

    pub async fn list(&self) -> Vec<Task> {
        let tasks = self.tasks.read().await;
        tasks.values().cloned().collect()
    }

    pub async fn cancel(&self, task_id: &TaskId) -> Result<Task, String> {
        let had_handle = {
            let mut handles = self.handles.write().await;
            if let Some(handle) = handles.remove(task_id) {
                handle.abort();
                true
            } else {
                false
            }
        };

        let iterations_completed = {
            let mut progress = self.progress.write().await;
            let count = progress
                .get(task_id)
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0);
            progress.remove(task_id);
            count
        };

        let cancelled = {
            let mut tasks = self.tasks.write().await;
            let task = tasks
                .get_mut(task_id)
                .ok_or_else(|| format!("Task not found: {}", task_id))?;

            if task.status.is_terminal() {
                let _ = had_handle;
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
            let senders = self.status_senders.read().await;
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
        let mut tasks = self.tasks.write().await;
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
            let senders = self.status_senders.read().await;
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
                let tasks = self.tasks.read().await;
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

/// The network policy a task's dev container gets: the identity's ceiling
/// when running under one, otherwise the runtime default.
fn network_policy_for(identity: Option<&AgentIdentity>) -> NetworkPolicy {
    match identity {
        Some(identity) => NetworkPolicy::for_ceiling(identity.scope.max_effect),
        None => NetworkPolicy::Enabled,
    }
}

fn registry_for(
    workspace: &TaskWorkspace,
    identity: Option<&AgentIdentity>,
) -> Result<crate::tools::ToolRegistry, crate::scope::ScopeError> {
    match identity {
        Some(identity) => workspace.build_tool_registry_for(identity),
        None => Ok(workspace.build_tool_registry()),
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

    type ChatHook = Box<dyn Fn() + Send + Sync>;

    struct MockProvider {
        responses: Mutex<Vec<ChatResponse>>,
        on_chat: Option<ChatHook>,
    }

    impl MockProvider {
        fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses),
                on_chat: None,
            })
        }

        fn with_hook(responses: Vec<ChatResponse>, on_chat: ChatHook) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses),
                on_chat: Some(on_chat),
            })
        }
    }

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            if let Some(hook) = &self.on_chat {
                hook();
            }
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

    fn tool_call_response(tool_name: &str, args: serde_json::Value) -> ChatResponse {
        use model::types::{FunctionCall, ToolCall};
        ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: MessageRole::Assistant,
                    content: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_0".to_string(),
                        function: FunctionCall {
                            name: tool_name.to_string(),
                            arguments: args,
                        },
                    }]),
                    tool_call_id: None,
                },
                finish_reason: Some(FinishReason::ToolCalls),
            }],
            usage: None,
        }
    }

    fn stop_response(content: &str) -> ChatResponse {
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
            denials: vec![crate::scope::ScopeDenial {
                identity: "rust-implementer".to_string(),
                tool: "write_file".to_string(),
                reason: crate::scope::DenialReason::ToolNotInScope,
            }],
            iterations: 3,
            model_used: "qwen3:0.6b".to_string(),
        };
        assert_eq!(result.denial_count(), 1);
        let json = result.to_json();
        assert_eq!(json["result_summary"], "Done");
        assert_eq!(json["iterations"], 3);
        assert!(json["changes_patch"].is_string());
        assert!(json["format_patch"].is_string());
        assert_eq!(json["denial_count"], 1);
        assert_eq!(json["denials"][0]["identity"], "rust-implementer");
        assert_eq!(json["denials"][0]["tool"], "write_file");
        assert_eq!(json["denials"][0]["reason"]["kind"], "tool_not_in_scope");
        let legacy: TaskResult = serde_json::from_value(serde_json::json!({
            "result_summary": "", "changes_patch": null, "format_patch": null,
            "files_modified": [], "tool_calls_made": [], "iterations": 0, "model_used": "m"
        }))
        .unwrap();
        assert_eq!(legacy.denial_count(), 0);
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
            denials: vec![],
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
            effect: Some(crate::effects::EffectRecord::new(EffectClass::None)),
        };
        let diag = FailureDiagnostics {
            error_type: "StateError".to_string(),
            iterations_completed: 5,
            last_tool_call: Some(tool_call.clone()),
            partial_changes: Some("diff --git a/foo".to_string()),
            tool_call_history: vec![tool_call],
            last_agent_state: Some("Performing".to_string()),
            conversation_snapshot: Some(vec![ChatMessage::user("do something")]),
            denials: vec![],
        };
        let json = diag.to_json();
        assert_eq!(json["error_type"], "StateError");
        assert_eq!(json["iterations_completed"], 5);
        assert!(json["last_tool_call"].is_object());
        assert_eq!(json["tool_call_history"].as_array().unwrap().len(), 1);
        assert_eq!(json["last_agent_state"], "Performing");
        assert!(json["conversation_snapshot"].is_array());
        assert_eq!(json["tool_call_history"][0]["effect"]["class"], "none");
    }

    fn record(name: &str, class: Option<EffectClass>) -> ToolCallRecord {
        ToolCallRecord {
            tool_name: name.to_string(),
            arguments: serde_json::json!({}),
            call_id: format!("call_{name}"),
            result: String::new(),
            effect: class.map(crate::effects::EffectRecord::new),
        }
    }

    fn result_with_calls(tool_calls_made: Vec<ToolCallRecord>) -> TaskResult {
        TaskResult {
            result_summary: "done".to_string(),
            changes_patch: None,
            format_patch: None,
            files_modified: vec![],
            tool_calls_made,
            denials: vec![],
            iterations: 1,
            model_used: "mock".to_string(),
        }
    }

    #[test]
    fn test_task_result_json_exposes_effect_on_every_tool_call() {
        let result = result_with_calls(vec![
            record("read_file", Some(EffectClass::None)),
            record("write_file", Some(EffectClass::Workspace)),
            record("unregistered", None),
        ]);
        let json = result.to_json();
        let calls = json["tool_calls_made"].as_array().unwrap();
        assert_eq!(calls[0]["effect"]["class"], "none");
        assert_eq!(calls[1]["effect"]["class"], "workspace");
        assert!(calls[2]["effect"].is_null());
        assert_eq!(json["max_effect_class"], "workspace");
        assert_eq!(result.max_effect_class(), Some(EffectClass::Workspace));
    }

    #[test]
    fn test_task_result_max_effect_class_is_none_without_attributed_calls() {
        assert_eq!(result_with_calls(vec![]).max_effect_class(), None);
        let unattributed = result_with_calls(vec![record("x", None)]);
        assert_eq!(unattributed.max_effect_class(), None);
        assert!(unattributed.to_json()["max_effect_class"].is_null());
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
        };
        {
            let mut tasks = manager.tasks.write().await;
            tasks.insert(task_id.clone(), task);
        }
        let dummy = tokio::spawn(std::future::pending::<()>());
        let abort_handle = dummy.abort_handle();
        {
            let mut handles = manager.handles.write().await;
            handles.insert(task_id.clone(), abort_handle);
        }
        let result = manager.cancel(&task_id).await;
        assert!(result.is_ok());
        let task = result.unwrap();
        assert!(matches!(&task.status, TaskStatus::Cancelled { .. }));
        dummy.abort();
    }

    #[tokio::test]
    async fn test_queued_task_starts_after_completion() {
        let sem = Arc::new(Semaphore::new(1));
        let permit = sem.clone().acquire_owned().await.unwrap();
        let sem2 = Arc::clone(&sem);
        let handle = tokio::spawn(async move {
            let _p = sem2.acquire_owned().await.unwrap();
        });
        tokio::task::yield_now().await;
        assert!(!handle.is_finished());
        drop(permit);
        handle.await.unwrap();
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
    fn init_test_git_repo(dir: &std::path::Path) {
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

    fn scoped_identity() -> AgentIdentity {
        let mut identity = crate::identity::example();
        identity.scope.max_effect = EffectClass::Workspace;
        identity.scope.tools = vec!["write_file".parse().unwrap(), "read_file".parse().unwrap()];
        identity
    }

    async fn wait_for_terminal(manager: &TaskManager, task_id: &TaskId) -> TaskStatus {
        let deadline = std::time::Instant::now() + tokio::time::Duration::from_secs(10);
        loop {
            let task = manager.poll(task_id).await.unwrap();
            if task.status.is_terminal() {
                return task.status;
            }
            assert!(std::time::Instant::now() < deadline, "task did not finish");
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn test_network_policy_follows_the_identity_ceiling() {
        assert_eq!(network_policy_for(None), NetworkPolicy::Enabled);
        let mut identity = scoped_identity();
        assert_eq!(network_policy_for(Some(&identity)), NetworkPolicy::Disabled);
        identity.scope.max_effect = EffectClass::Repository;
        assert_eq!(network_policy_for(Some(&identity)), NetworkPolicy::Enabled);
    }

    #[tokio::test]
    async fn test_submit_with_identity_records_denials_in_the_result() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_git_repo(repo_dir.path());

        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let provider: Arc<dyn ModelProvider> =
            MockProvider::new(wrap_with_state_machine_responses(vec![
                tool_call_response(
                    "write_file",
                    serde_json::json!({"path": "README.md", "content": "x"}),
                ),
                tool_call_response(
                    "write_file",
                    serde_json::json!({"path": "api/new.rs", "content": "ok"}),
                ),
                stop_response("done"),
            ]));
        let task_id = manager
            .submit_with_identity(
                "Test".to_string(),
                repo_dir.path().to_path_buf(),
                "HEAD".to_string(),
                "mock".to_string(),
                20,
                provider,
                Some(scoped_identity()),
            )
            .await;

        let status = wait_for_terminal(&manager, &task_id).await;
        assert!(matches!(status, TaskStatus::Completed { .. }), "{status:?}");
        let result = manager.get_result(&task_id).await.unwrap();
        assert_eq!(result.denial_count(), 1);
        assert_eq!(result.denials[0].identity, "rust-implementer");
        assert_eq!(result.denials[0].tool, "write_file");
        assert_eq!(
            result.to_json()["denials"][0]["reason"]["path"],
            "README.md"
        );
        assert_eq!(result.files_modified, vec!["api/new.rs".to_string()]);
    }

    #[tokio::test]
    async fn test_submit_with_identity_fails_on_an_invalid_scope_glob() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_git_repo(repo_dir.path());

        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![stop_response("done")]);
        let mut identity = scoped_identity();
        identity.scope.paths = vec!["[".to_string()];
        let task_id = manager
            .submit_with_identity(
                "Test".to_string(),
                repo_dir.path().to_path_buf(),
                "HEAD".to_string(),
                "mock".to_string(),
                20,
                provider,
                Some(identity),
            )
            .await;

        match wait_for_terminal(&manager, &task_id).await {
            TaskStatus::Failed {
                diagnostics, error, ..
            } => {
                assert_eq!(diagnostics.error_type, "ScopeError");
                assert!(error.contains("scope.paths"), "{error}");
            }
            other => panic!("expected ScopeError failure, got {other:?}"),
        }
        assert!(manager.handles.read().await.get(&task_id).is_none());
        assert!(manager.progress.read().await.get(&task_id).is_none());
    }

    #[tokio::test]
    async fn test_submit_with_identity_starts_the_container_with_its_network_policy() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_git_repo(repo_dir.path());
        std::fs::write(repo_dir.path().join("flake.nix"), "{}").unwrap();
        std::fs::create_dir(repo_dir.path().join(".devcontainer")).unwrap();

        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let canonical = repo_dir.path().canonicalize().unwrap();
        {
            let mut cache = manager.image_cache.write().await;
            cache.insert(canonical, "nanna-missing-image-for-tests:none".to_string());
        }
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![stop_response("done")]);
        let task_id = manager
            .submit_with_identity(
                "Test".to_string(),
                repo_dir.path().to_path_buf(),
                "HEAD".to_string(),
                "mock".to_string(),
                20,
                provider,
                Some(scoped_identity()),
            )
            .await;

        match wait_for_terminal(&manager, &task_id).await {
            TaskStatus::Failed { diagnostics, .. } => {
                assert_eq!(diagnostics.error_type, "WorkspaceCreationFailed");
            }
            other => panic!("expected the missing image to fail the task, got {other:?}"),
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
            let mut cache = manager.image_cache.write().await;
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
            let mut senders = manager.status_senders.write().await;
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

    #[test]
    fn failure_diagnostics_json_carries_denials_and_tolerates_their_absence() {
        let violation = crate::protected::ProtectedPathViolation {
            path: "codecov.yml".to_string(),
            rule: "codecov.yml".to_string(),
        };
        let diagnostics = FailureDiagnostics {
            error_type: "ProtectedPathViolation".to_string(),
            iterations_completed: 1,
            last_tool_call: None,
            partial_changes: None,
            tool_call_history: vec![],
            last_agent_state: None,
            conversation_snapshot: None,
            denials: vec![crate::scope::ScopeDenial::protected(
                "extract_changes",
                &violation,
            )],
        };
        let json = diagnostics.to_json();
        assert_eq!(json["denials"][0]["reason"]["kind"], "protected_path");
        assert_eq!(json["denials"][0]["tool"], "extract_changes");
        let legacy = serde_json::json!({
            "error_type": "StateError", "iterations_completed": 0, "last_tool_call": null,
            "partial_changes": null, "tool_call_history": [], "last_agent_state": null,
            "conversation_snapshot": null
        });
        let parsed: FailureDiagnostics = serde_json::from_value(legacy).unwrap();
        assert!(parsed.denials.is_empty());
    }

    fn plant_protected_file(source_repo: &std::path::Path) {
        let out = std::process::Command::new("git")
            .current_dir(source_repo)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&out.stdout);
        let worktree = listing
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .find(|path| path.contains("nanna-task-"))
            .expect("task worktree present");
        let agents = std::path::Path::new(worktree).join(".nanna/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("x.toml"), "[identity]").unwrap();
    }

    struct RecordingHook {
        seen: std::sync::Mutex<Vec<(String, crate::protected::ProtectedPathViolation)>>,
    }

    impl crate::protected::AuditHook for RecordingHook {
        fn on_protected_path_violation(
            &self,
            task_id: &str,
            violation: &crate::protected::ProtectedPathViolation,
        ) {
            let entry = (task_id.to_string(), violation.clone());
            self.seen.lock().unwrap().push(entry);
        }
    }

    fn planting_provider(repo: std::path::PathBuf) -> Arc<dyn ModelProvider> {
        let responses = wrap_with_state_machine_responses(vec![
            tool_call_response(
                "write_file",
                serde_json::json!({"path": "api/new.rs", "content": "ok"}),
            ),
            stop_response("done"),
        ]);
        MockProvider::with_hook(responses, Box::new(move || plant_protected_file(&repo)))
    }

    #[tokio::test]
    async fn test_a_patch_touching_an_identity_file_fails_the_task_under_an_identity() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_git_repo(repo_dir.path());
        let hook = Arc::new(RecordingHook {
            seen: std::sync::Mutex::new(vec![]),
        });
        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS).with_audit_hook(hook.clone());
        let provider = planting_provider(repo_dir.path().to_path_buf());
        let task_id = manager
            .submit_with_identity(
                "Test".to_string(),
                repo_dir.path().to_path_buf(),
                "HEAD".to_string(),
                "mock".to_string(),
                20,
                provider,
                Some(scoped_identity()),
            )
            .await;

        let (error, diagnostics) = match wait_for_terminal(&manager, &task_id).await {
            TaskStatus::Failed {
                error, diagnostics, ..
            } => (error, diagnostics),
            other => panic!("expected a failure, got {other:?}"),
        };
        assert_eq!(diagnostics.error_type, "ProtectedPathViolation");
        assert!(
            error.contains("`.nanna/agents/x.toml` is protected by rule `.nanna/**`"),
            "{error}"
        );
        assert!(diagnostics.iterations_completed > 0);
        assert!(!diagnostics.tool_call_history.is_empty());
        assert!(diagnostics.last_tool_call.is_some());
        let denial = diagnostics.denials.last().unwrap();
        assert_eq!(denial.identity, "rust-implementer");
        assert_eq!(denial.tool, "extract_changes");
        assert!(
            matches!(&denial.reason, crate::scope::DenialReason::ProtectedPath { path, .. } if path == ".nanna/agents/x.toml")
        );
        let seen = hook.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, task_id.0);
        assert_eq!(seen[0].1.rule, ".nanna/**");
        assert!(manager.handles.read().await.get(&task_id).is_none());
        assert!(manager.progress.read().await.get(&task_id).is_none());
    }

    #[tokio::test]
    async fn test_a_patch_touching_an_identity_file_fails_the_task_without_an_identity() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_git_repo(repo_dir.path());
        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let provider = planting_provider(repo_dir.path().to_path_buf());
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

        match wait_for_terminal(&manager, &task_id).await {
            TaskStatus::Failed { diagnostics, .. } => {
                assert_eq!(diagnostics.error_type, "ProtectedPathViolation");
                let denial = diagnostics.denials.last().unwrap();
                assert_eq!(denial.identity, crate::scope::UNSCOPED_IDENTITY);
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn protected_failure_after_an_errored_run_keeps_its_iterations_and_calls() {
        let violation = crate::protected::ProtectedPathViolation {
            path: "windows.toml".to_string(),
            rule: "windows.toml".to_string(),
        };
        let call = record("write_file", Some(EffectClass::Workspace));
        let run: Result<AgentRunResult, AgentError> = Err(AgentError::MaxIterationsExceeded {
            iterations_completed: 7,
            tool_calls_made: vec![call.clone()],
            conversation_snapshot: vec![],
            last_agent_state: crate::agent::AgentState::PerformingEntityModification,
        });
        let (error, diagnostics) = protected_failure(violation, None, &run);
        assert!(error.starts_with("`windows.toml` is protected"));
        assert_eq!(diagnostics.iterations_completed, 7);
        assert_eq!(diagnostics.tool_call_history.len(), 1);
        assert_eq!(
            diagnostics.last_tool_call.map(|c| c.tool_name),
            Some("write_file".to_string())
        );
        assert_eq!(diagnostics.denials.len(), 1);
        assert_eq!(
            diagnostics.denials[0].identity,
            crate::scope::UNSCOPED_IDENTITY
        );
    }

    #[tokio::test]
    async fn test_a_run_that_exhausts_its_iterations_fails_with_no_denials() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_test_git_repo(repo_dir.path());
        let manager = TaskManager::new(DEFAULT_MAX_CONCURRENT_TASKS);
        let responses: Vec<ChatResponse> = (0..5).map(|_| stop_response("not done yet")).collect();
        let provider: Arc<dyn ModelProvider> = MockProvider::new(responses);
        let task_id = manager
            .submit(
                "Test".to_string(),
                repo_dir.path().to_path_buf(),
                "HEAD".to_string(),
                "mock".to_string(),
                0,
                provider,
            )
            .await;

        match wait_for_terminal(&manager, &task_id).await {
            TaskStatus::Failed { diagnostics, .. } => {
                assert_eq!(diagnostics.error_type, "MaxIterationsExceeded");
                assert!(diagnostics.denials.is_empty());
                assert_eq!(diagnostics.to_json()["denials"], serde_json::json!([]));
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }
}
