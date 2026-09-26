//! Backlog ingestion: pull open GitHub issues into the scheduler queue.
//!
//! [`backlog_sync`] queries each configured repository for open issues
//! matching a search query, skips issues that are already queued (by issue
//! number) or already claimed by an open pull request carrying the
//! [`IDENTITY_MARKER`], honours an optional per-repository cap, and submits
//! the rest as [`QueuedTask`]s tagged with the source's identity hint.
//!
//! GitHub access goes through the [`GithubClient`] trait so ingestion can be
//! tested against a mock; [`ReqwestGithubClient`] is the REST implementation.
//! Where tasks go is the [`BacklogSink`]: a running [`TaskManager`] via
//! [`ManagerSink`], or a bare [`QueueStore`] via [`StoreSink`] for the CLI.

use crate::scheduler::{QueueStore, QueueStoreError, QueuedTask, TaskOrigin, TaskQueue};
use crate::task::TaskManager;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use model::provider::ModelProvider;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;

/// Marker an agent-authored pull request carries in its body. Issues
/// referenced by an open pull request with this marker are not ingested.
pub const IDENTITY_MARKER: &str = "Nanna-Identity:";

/// Page size used for GitHub list and search requests.
pub const GITHUB_PAGE_SIZE: usize = 100;

/// Default GitHub REST endpoint.
pub const GITHUB_API_URL: &str = "https://api.github.com";

/// The subset of a GitHub issue that ingestion needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIssue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub html_url: String,
}

/// The subset of a GitHub pull request that ingestion needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubPullRequest {
    pub number: u64,
    #[serde(default)]
    pub body: Option<String>,
}

/// The pull request GitHub hands back right after creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubPullRequestCreated {
    pub number: u64,
    pub html_url: String,
    pub node_id: String,
}

/// The subset of a pull request's detail view the PR lifecycle tools need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubPullRequestDetail {
    pub number: u64,
    pub node_id: String,
    pub draft: bool,
    pub state: String,
    #[serde(default)]
    pub body: Option<String>,
    pub html_url: String,
}

/// A single review or issue comment on a pull request or issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubComment {
    pub id: u64,
    pub author: String,
    pub body: String,
    pub html_url: String,
}

/// The subset of an issue's detail view the issue-read tool needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubIssueDetail {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub html_url: String,
}

/// Failure during ingestion.
#[derive(Debug, Error)]
pub enum BacklogError {
    #[error("GitHub request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("GitHub returned HTTP {status} for {url}")]
    Status { url: String, status: u16 },
    #[error("GitHub response could not be parsed: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("queue store failed: {0}")]
    Store(#[from] QueueStoreError),
    #[error("GitHub rate limit exceeded for {url} after {attempts} attempt(s), retry after {retry_after_secs:?}s")]
    RateLimited {
        url: String,
        attempts: u32,
        retry_after_secs: Option<u64>,
    },
    #[error("GitHub GraphQL error for {url}: {message}")]
    GraphQl { url: String, message: String },
}

/// Read access to the GitHub data ingestion needs.
#[async_trait]
pub trait GithubClient: Send + Sync {
    /// Open issues of `repo` (`owner/name`) matching the GitHub search
    /// `query` (for example `label:nanna`).
    async fn search_open_issues(
        &self,
        repo: &str,
        query: &str,
    ) -> Result<Vec<GithubIssue>, BacklogError>;

    /// Open pull requests of `repo`.
    async fn open_pull_requests(&self, repo: &str) -> Result<Vec<GithubPullRequest>, BacklogError>;

    /// Create an issue in `repo` with `labels` and return it.
    async fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<GithubIssue, BacklogError>;

    /// Add a comment to issue `number` of `repo`.
    async fn comment_on_issue(
        &self,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<(), BacklogError>;

    /// Open a pull request from `head` into `base` of `repo`. Always created
    /// as a draft: the method takes no `draft` parameter, so there is no
    /// argument that could make it create a ready-for-review pull request.
    async fn create_draft_pull_request(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<GithubPullRequestCreated, BacklogError>;

    /// Fetch pull request `number` of `repo`.
    async fn get_pull_request(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<GithubPullRequestDetail, BacklogError>;

    /// Convert draft pull request `number` of `repo` to ready for review.
    async fn mark_pull_request_ready(&self, repo: &str, number: u64) -> Result<(), BacklogError>;

    /// Close pull request `number` of `repo`. Never touches an issue: the
    /// implementation always addresses the `pulls` endpoint, so this method
    /// cannot be used to close an issue.
    async fn close_pull_request(&self, repo: &str, number: u64) -> Result<(), BacklogError>;

    /// Review comments (inline code comments) on pull request `number` of
    /// `repo`.
    async fn list_review_comments(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Vec<GithubComment>, BacklogError>;

    /// Issue-style (conversation) comments on issue or pull request `number`
    /// of `repo`. GitHub treats a pull request as an issue for this
    /// endpoint, so the same method serves both.
    async fn list_issue_comments(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Vec<GithubComment>, BacklogError>;

    /// Fetch issue `number` of `repo`.
    async fn get_issue(&self, repo: &str, number: u64) -> Result<GithubIssueDetail, BacklogError>;
}

/// One GitHub Actions workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    /// `queued`, `in_progress` or `completed`.
    pub status: String,
    /// `success`, `failure`, `cancelled`, ... only meaningful once `status`
    /// is `completed`.
    #[serde(default)]
    pub conclusion: Option<String>,
    pub html_url: String,
    /// Branch the run's workflow file ran from; absent from some older
    /// mocked payloads, hence the default rather than a hard parse failure.
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default)]
    pub run_started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl WorkflowRun {
    /// Minutes between `run_started_at` and `updated_at`, when both are
    /// known and the run has finished; the closest thing the REST API
    /// offers to billed run duration without a separate timing call.
    pub fn duration_minutes(&self) -> Option<f64> {
        let started = self.run_started_at?;
        let ended = self.updated_at?;
        if self.status != "completed" || ended < started {
            return None;
        }
        Some((ended - started).num_seconds() as f64 / 60.0)
    }

    /// Whether re-running this run's failed jobs makes sense: it belongs to
    /// `branch` and it actually finished with a failure-like conclusion,
    /// rather than a run from an unrelated branch (a different PR, or
    /// `main`) or one still in progress.
    pub fn is_rerunnable_failure_on(&self, branch: &str) -> bool {
        self.head_branch.as_deref() == Some(branch)
            && matches!(
                self.conclusion.as_deref(),
                Some("failure") | Some("timed_out") | Some("cancelled")
            )
    }
}

/// One job of a [`WorkflowRun`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowJob {
    pub id: u64,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub steps: Vec<WorkflowStep>,
}

impl WorkflowJob {
    /// Whether this job's conclusion is a failure worth fetching logs for.
    pub fn failed(&self) -> bool {
        matches!(
            self.conclusion.as_deref(),
            Some("failure") | Some("timed_out") | Some("cancelled")
        )
    }
}

/// One step of a [`WorkflowJob`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    pub number: u64,
}

/// GitHub Actions access the CI tools need: dispatching or re-running a
/// workflow and reading back its status, jobs and logs. Kept as a trait
/// distinct from [`GithubClient`] (rather than added to it) so the existing
/// issue/PR trait, its doctest and `test_support::MockGithub` are
/// untouched; [`ReqwestGithubClient`] implements both over the same
/// retry/rate-limit plumbing.
#[async_trait]
pub trait GithubActionsClient: Send + Sync {
    /// `POST /repos/{repo}/actions/workflows/{workflow}/dispatches`.
    /// `workflow` is the file name (`ci.yml`) or the numeric workflow id as
    /// a string. GitHub answers `204 No Content` and does not hand back a
    /// run id; callers resolve it with [`Self::list_workflow_runs`].
    async fn dispatch_workflow(
        &self,
        repo: &str,
        workflow: &str,
        git_ref: &str,
        inputs: serde_json::Value,
    ) -> Result<(), BacklogError>;

    /// `POST /repos/{repo}/actions/runs/{run_id}/rerun-failed-jobs`. Reuses
    /// `run_id`: GitHub bumps the run's attempt count rather than minting a
    /// new run.
    async fn rerun_workflow(&self, repo: &str, run_id: u64) -> Result<(), BacklogError>;

    /// `GET /repos/{repo}/actions/workflows/{workflow}/runs`, newest first,
    /// filtered to `branch` and `event` (for example `workflow_dispatch`).
    /// One page (GitHub's default ordering and page size), which is enough
    /// to find the run a dispatch just created.
    async fn list_workflow_runs(
        &self,
        repo: &str,
        workflow: &str,
        branch: &str,
        event: &str,
    ) -> Result<Vec<WorkflowRun>, BacklogError>;

    /// `GET /repos/{repo}/actions/runs/{run_id}`.
    async fn get_workflow_run(&self, repo: &str, run_id: u64) -> Result<WorkflowRun, BacklogError>;

    /// `GET /repos/{repo}/actions/runs/{run_id}/jobs`.
    async fn list_workflow_jobs(
        &self,
        repo: &str,
        run_id: u64,
    ) -> Result<Vec<WorkflowJob>, BacklogError>;

    /// `GET /repos/{repo}/actions/jobs/{job_id}/logs`: the plain-text log of
    /// one job, after GitHub's redirect to blob storage.
    async fn get_job_logs(&self, repo: &str, job_id: u64) -> Result<String, BacklogError>;
}

/// A repository whose backlog is ingested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogSource {
    /// GitHub repository in `owner/name` form.
    pub repo: String,
    /// Local checkout the tasks run against.
    pub repo_path: PathBuf,
    /// Branch the task worktrees are based on.
    pub branch: String,
    /// GitHub search query fragment selecting the issues.
    pub query: String,
    /// Identity hint attached to every ingested task.
    pub identity: String,
    /// Model the tasks run with.
    pub model: String,
    /// Iteration budget per task.
    pub max_iterations: usize,
}

/// Ingestion configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BacklogConfig {
    pub sources: Vec<BacklogSource>,
    /// Maximum number of pending tasks per `repo_path`, counting tasks
    /// already known to the sink. `None` means unlimited.
    pub max_per_repo: Option<usize>,
}

/// A pending task as far as ingestion cares: where it runs and where it
/// came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownTask {
    pub repo_path: PathBuf,
    pub origin: Option<TaskOrigin>,
}

/// Destination of ingested tasks.
#[async_trait]
pub trait BacklogSink: Send + Sync {
    /// Tasks that are queued or running, used for deduplication and the
    /// per-repository cap.
    async fn known(&self) -> Result<Vec<KnownTask>, BacklogError>;
    /// Accept a new task.
    async fn enqueue(&self, task: QueuedTask) -> Result<(), BacklogError>;
}

/// Outcome of one [`backlog_sync`] run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Origins enqueued by this run, in ingestion order.
    pub enqueued: Vec<TaskOrigin>,
    /// Issues skipped because a task with the same origin was already known.
    pub duplicates: usize,
    /// Issues skipped because an open marked pull request references them.
    pub claimed: usize,
    /// Issues skipped because the repository reached `max_per_repo`.
    pub capped: usize,
}

/// Issue numbers referenced as `#123` in `text`.
///
/// ```
/// use harness::backlog::referenced_issues;
///
/// let refs = referenced_issues("Closes #12, relates to #7 and #12; not #x");
/// assert_eq!(refs.into_iter().collect::<Vec<_>>(), vec![7, 12]);
/// ```
pub fn referenced_issues(text: &str) -> BTreeSet<u64> {
    text.split('#')
        .skip(1)
        .filter_map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect()
}

/// Task description generated for an issue.
pub fn describe_issue(repo: &str, issue: &GithubIssue) -> String {
    let body = issue.body.as_deref().unwrap_or("").trim();
    format!(
        "Resolve GitHub issue {repo}#{}: {}\n\n{}\n\n{body}",
        issue.number, issue.title, issue.html_url
    )
}

/// Pull matching open issues from every source into `sink`.
///
/// ```
/// use harness::backlog::{
///     backlog_sync, BacklogConfig, BacklogError, BacklogSource, GithubClient, GithubComment,
///     GithubIssue, GithubIssueDetail, GithubPullRequest, GithubPullRequestCreated,
///     GithubPullRequestDetail, StoreSink,
/// };
/// use harness::scheduler::InMemoryQueueStore;
/// use std::path::PathBuf;
///
/// struct OneIssue;
/// #[async_trait::async_trait]
/// impl GithubClient for OneIssue {
///     async fn search_open_issues(&self, _: &str, _: &str) -> Result<Vec<GithubIssue>, BacklogError> {
///         Ok(vec![GithubIssue { number: 1, title: "t".into(), body: None, html_url: "u".into() }])
///     }
///     async fn open_pull_requests(&self, _: &str) -> Result<Vec<GithubPullRequest>, BacklogError> {
///         Ok(vec![])
///     }
///     async fn create_issue(&self, _: &str, _: &str, _: &str, _: &[String]) -> Result<GithubIssue, BacklogError> {
///         unreachable!("ingestion never creates issues")
///     }
///     async fn comment_on_issue(&self, _: &str, _: u64, _: &str) -> Result<(), BacklogError> {
///         unreachable!("ingestion never comments")
///     }
///     async fn create_draft_pull_request(&self, _: &str, _: &str, _: &str, _: &str, _: &str) -> Result<GithubPullRequestCreated, BacklogError> {
///         unreachable!("ingestion never opens pull requests")
///     }
///     async fn get_pull_request(&self, _: &str, _: u64) -> Result<GithubPullRequestDetail, BacklogError> {
///         unreachable!("ingestion never reads pull request detail")
///     }
///     async fn mark_pull_request_ready(&self, _: &str, _: u64) -> Result<(), BacklogError> {
///         unreachable!("ingestion never promotes pull requests")
///     }
///     async fn close_pull_request(&self, _: &str, _: u64) -> Result<(), BacklogError> {
///         unreachable!("ingestion never closes pull requests")
///     }
///     async fn list_review_comments(&self, _: &str, _: u64) -> Result<Vec<GithubComment>, BacklogError> {
///         unreachable!("ingestion never reads review comments")
///     }
///     async fn list_issue_comments(&self, _: &str, _: u64) -> Result<Vec<GithubComment>, BacklogError> {
///         unreachable!("ingestion never reads issue comments")
///     }
///     async fn get_issue(&self, _: &str, _: u64) -> Result<GithubIssueDetail, BacklogError> {
///         unreachable!("ingestion never reads issue detail")
///     }
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let sink = StoreSink::open(Box::new(InMemoryQueueStore::default())).unwrap();
/// let config = BacklogConfig {
///     sources: vec![BacklogSource {
///         repo: "owner/name".into(),
///         repo_path: PathBuf::from("/repo"),
///         branch: "main".into(),
///         query: "label:nanna".into(),
///         identity: "sdlc-dev".into(),
///         model: "m".into(),
///         max_iterations: 10,
///     }],
///     max_per_repo: None,
/// };
/// let report = backlog_sync(&OneIssue, &sink, &config).await.unwrap();
/// assert_eq!(report.enqueued.len(), 1);
/// let again = backlog_sync(&OneIssue, &sink, &config).await.unwrap();
/// assert_eq!(again.duplicates, 1);
/// # });
/// ```
pub async fn backlog_sync(
    client: &dyn GithubClient,
    sink: &dyn BacklogSink,
    config: &BacklogConfig,
) -> Result<SyncReport, BacklogError> {
    let known = sink.known().await?;
    let mut known_origins: HashSet<TaskOrigin> =
        known.iter().filter_map(|t| t.origin.clone()).collect();
    let mut per_repo: HashMap<PathBuf, usize> = HashMap::new();
    for task in &known {
        *per_repo.entry(task.repo_path.clone()).or_insert(0) += 1;
    }
    let mut report = SyncReport::default();
    for source in &config.sources {
        let issues = client
            .search_open_issues(&source.repo, &source.query)
            .await?;
        let claimed: BTreeSet<u64> = client
            .open_pull_requests(&source.repo)
            .await?
            .iter()
            .filter_map(|pr| pr.body.as_deref())
            .filter(|body| body.contains(IDENTITY_MARKER))
            .flat_map(referenced_issues)
            .collect();
        for issue in issues {
            let origin = TaskOrigin {
                repo: source.repo.clone(),
                issue: issue.number,
            };
            if known_origins.contains(&origin) {
                report.duplicates += 1;
                continue;
            }
            if claimed.contains(&issue.number) {
                report.claimed += 1;
                continue;
            }
            let count = per_repo.entry(source.repo_path.clone()).or_insert(0);
            if config.max_per_repo.is_some_and(|cap| *count >= cap) {
                report.capped += 1;
                continue;
            }
            let task = QueuedTask::new(
                describe_issue(&source.repo, &issue),
                source.repo_path.clone(),
                source.branch.clone(),
                source.model.clone(),
                source.max_iterations,
            )
            .with_identity_hint(Some(source.identity.clone()))
            .with_origin(Some(origin.clone()));
            sink.enqueue(task).await?;
            *count += 1;
            known_origins.insert(origin.clone());
            report.enqueued.push(origin);
        }
    }
    Ok(report)
}

/// Sink that writes straight into a [`QueueStore`], for a process that does
/// not run a [`TaskManager`] (the `backlog-sync` CLI). The running manager
/// picks the entries up when it is next restored from the same store.
pub struct StoreSink {
    store: Box<dyn QueueStore>,
    queue: Mutex<TaskQueue>,
}

impl StoreSink {
    /// Load the current queue from `store`.
    pub fn open(store: Box<dyn QueueStore>) -> Result<Self, QueueStoreError> {
        let queue = Mutex::new(TaskQueue::from_entries(store.load()?));
        Ok(Self { store, queue })
    }
}

#[async_trait]
impl BacklogSink for StoreSink {
    async fn known(&self) -> Result<Vec<KnownTask>, BacklogError> {
        Ok(self
            .queue
            .lock()
            .await
            .entries()
            .iter()
            .map(|t| KnownTask {
                repo_path: t.repo_path.clone(),
                origin: t.origin.clone(),
            })
            .collect())
    }

    async fn enqueue(&self, task: QueuedTask) -> Result<(), BacklogError> {
        let mut queue = self.queue.lock().await;
        let stored = queue.push(task);
        self.store.insert(&stored)?;
        Ok(())
    }
}

/// Sink that submits into a live [`TaskManager`].
pub struct ManagerSink {
    manager: Arc<TaskManager>,
    provider: Arc<dyn ModelProvider>,
}

impl ManagerSink {
    pub fn new(manager: Arc<TaskManager>, provider: Arc<dyn ModelProvider>) -> Self {
        Self { manager, provider }
    }
}

#[async_trait]
impl BacklogSink for ManagerSink {
    async fn known(&self) -> Result<Vec<KnownTask>, BacklogError> {
        Ok(self
            .manager
            .list()
            .await
            .into_iter()
            .filter(|t| !t.status.is_terminal())
            .map(|t| KnownTask {
                repo_path: t.repo_path,
                origin: t.origin,
            })
            .collect())
    }

    async fn enqueue(&self, task: QueuedTask) -> Result<(), BacklogError> {
        self.manager
            .submit_task(task, Arc::clone(&self.provider))
            .await;
        Ok(())
    }
}

/// Number of attempts [`ReqwestGithubClient`] makes for a call that keeps
/// meeting a rate-limit response, including the first.
const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// Base delay [`ReqwestGithubClient`] waits before the second attempt,
/// doubled for each attempt after that. The actual wait is at least the
/// response's `Retry-After` (or rate-limit reset) delay, when it gave one.
const DEFAULT_BASE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(200);

/// A `Retry-After`/rate-limit-reset delay longer than this fails the call
/// immediately with [`BacklogError::RateLimited`] rather than blocking the
/// caller for that long, and rather than resending a request GitHub has
/// asked to be held back that long (repeatedly ignoring `Retry-After` risks
/// the integration being blocked).
const MAX_RETRY_AFTER_SECS: u64 = 60;

/// Ceiling [`ReqwestGithubClient::with_retry_policy`] clamps `max_attempts`
/// to: the exponential backoff shift in [`ReqwestGithubClient::request_json`]
/// is only defined for shift amounts below `u32::BITS`, so an unbounded
/// caller-supplied `max_attempts` would panic (debug) or silently wrap
/// (release) on the last attempt before the cap kicked in anyway.
const MAX_RETRY_ATTEMPTS: u32 = 32;

/// One of the HTTP methods [`ReqwestGithubClient`] issues.
#[derive(Debug, Clone, Copy)]
enum HttpMethod {
    Get,
    Post,
    Patch,
}

/// The `Retry-After` delay, in seconds, a rate-limited response asked for,
/// derived from the `Retry-After` header or a `X-RateLimit-Remaining: 0` /
/// `X-RateLimit-Reset` pair. `None` means the response carried neither.
fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    if let Some(secs) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
    {
        return Some(secs);
    }
    let remaining = headers
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok());
    if remaining != Some("0") {
        return None;
    }
    let reset = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok());
    match reset {
        Some(reset) => Some((reset - Utc::now().timestamp()).max(0) as u64),
        None => Some(0),
    }
}

#[derive(Deserialize)]
struct RawUser {
    login: String,
}

#[derive(Deserialize)]
struct RawComment {
    id: u64,
    user: RawUser,
    body: String,
    html_url: String,
}

impl From<RawComment> for GithubComment {
    fn from(raw: RawComment) -> Self {
        Self {
            id: raw.id,
            author: raw.user.login,
            body: raw.body,
            html_url: raw.html_url,
        }
    }
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

#[derive(Deserialize)]
struct RawIssueDetail {
    number: u64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    labels: Vec<RawLabel>,
    html_url: String,
}

impl From<RawIssueDetail> for GithubIssueDetail {
    fn from(raw: RawIssueDetail) -> Self {
        Self {
            number: raw.number,
            title: raw.title,
            body: raw.body,
            labels: raw.labels.into_iter().map(|label| label.name).collect(),
            html_url: raw.html_url,
        }
    }
}

#[derive(Deserialize)]
struct RawPrDetail {
    number: u64,
    node_id: String,
    draft: bool,
    state: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
}

impl From<RawPrDetail> for GithubPullRequestDetail {
    fn from(raw: RawPrDetail) -> Self {
        Self {
            number: raw.number,
            node_id: raw.node_id,
            draft: raw.draft,
            state: raw.state,
            body: raw.body,
            html_url: raw.html_url,
        }
    }
}

#[derive(Deserialize)]
struct RawPrCreated {
    number: u64,
    node_id: String,
    html_url: String,
}

impl From<RawPrCreated> for GithubPullRequestCreated {
    fn from(raw: RawPrCreated) -> Self {
        Self {
            number: raw.number,
            html_url: raw.html_url,
            node_id: raw.node_id,
        }
    }
}

/// [`GithubClient`] over the GitHub REST API using `reqwest`.
///
/// Results are paginated `GITHUB_PAGE_SIZE` at a time until a short page is
/// returned. A token, when given, is sent as a bearer token. A response that
/// carries a `429`, or a `403` with a `Retry-After` header or an exhausted
/// `X-RateLimit-Remaining`, is retried with exponential backoff up to
/// [`ReqwestGithubClient::with_retry_policy`]'s configured attempts before
/// surfacing [`BacklogError::RateLimited`]. Any other non-success status
/// fails immediately.
#[derive(Debug, Clone)]
pub struct ReqwestGithubClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
    max_attempts: u32,
    base_backoff: std::time::Duration,
}

impl ReqwestGithubClient {
    /// Client for the API rooted at `base_url` (no trailing slash).
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            token,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_backoff: DEFAULT_BASE_BACKOFF,
        }
    }

    /// Client for the public GitHub API.
    pub fn github(token: Option<String>) -> Self {
        Self::new(GITHUB_API_URL, token)
    }

    /// Override the rate-limit retry policy: `max_attempts` total tries
    /// (including the first) and `base_backoff` doubled for each attempt
    /// after the first. Tests use a small `base_backoff` to avoid sleeping.
    pub fn with_retry_policy(
        mut self,
        max_attempts: u32,
        base_backoff: std::time::Duration,
    ) -> Self {
        self.max_attempts = max_attempts.clamp(1, MAX_RETRY_ATTEMPTS);
        self.base_backoff = base_backoff;
        self
    }

    async fn get_json(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value, BacklogError> {
        self.request_json(HttpMethod::Get, path, query, None).await
    }

    async fn post_json(
        &self,
        path: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, BacklogError> {
        self.request_json(HttpMethod::Post, path, &[], Some(payload))
            .await
    }

    async fn patch_json(
        &self,
        path: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, BacklogError> {
        self.request_json(HttpMethod::Patch, path, &[], Some(payload))
            .await
    }

    /// Issue `method path`, rebuilding and resending the request on every
    /// retry attempt (a [`reqwest::RequestBuilder`] is consumed by `send`,
    /// so it cannot be reused across attempts), and hand back the first
    /// successful response unconsumed so callers can read it as JSON, plain
    /// text, or not at all (`204 No Content`).
    async fn send_with_retry(
        &self,
        method: HttpMethod,
        path: &str,
        query: &[(&str, String)],
        payload: Option<&serde_json::Value>,
    ) -> Result<reqwest::Response, BacklogError> {
        let url = format!("{}{}", self.base_url, path);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut request = match method {
                HttpMethod::Get => self.http.get(&url).query(query),
                HttpMethod::Post => self.http.post(&url),
                HttpMethod::Patch => self.http.patch(&url),
            };
            if let Some(payload) = payload {
                request = request.json(payload);
            }
            request = request
                .header(reqwest::header::USER_AGENT, "nanna-coder")
                .header(reqwest::header::ACCEPT, "application/vnd.github+json");
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            let response = request.send().await?;
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            let retry_after = retry_after_secs(response.headers());
            let rate_limited =
                status.as_u16() == 429 || (status.as_u16() == 403 && retry_after.is_some());
            if !rate_limited {
                return Err(BacklogError::Status {
                    url,
                    status: status.as_u16(),
                });
            }
            let exceeds_cap = retry_after.is_some_and(|secs| secs > MAX_RETRY_AFTER_SECS);
            if !exceeds_cap && attempt < self.max_attempts {
                let factor = 1u32 << (attempt - 1);
                let backoff = self.base_backoff * factor;
                let wait = match retry_after {
                    Some(secs) => backoff.max(std::time::Duration::from_secs(secs)),
                    None => backoff,
                };
                tokio::time::sleep(wait).await;
                continue;
            }
            return Err(BacklogError::RateLimited {
                url,
                attempts: attempt,
                retry_after_secs: retry_after,
            });
        }
    }

    async fn request_json(
        &self,
        method: HttpMethod,
        path: &str,
        query: &[(&str, String)],
        payload: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, BacklogError> {
        let response = self.send_with_retry(method, path, query, payload).await?;
        Ok(serde_json::from_str(&response.text().await?)?)
    }

    /// Like [`Self::request_json`], but the response body is plain text
    /// rather than JSON (a workflow job's log).
    async fn request_text(
        &self,
        method: HttpMethod,
        path: &str,
        query: &[(&str, String)],
        payload: Option<&serde_json::Value>,
    ) -> Result<String, BacklogError> {
        let response = self.send_with_retry(method, path, query, payload).await?;
        Ok(response.text().await?)
    }

    /// Like [`Self::request_json`], for endpoints that answer success with
    /// no body (`workflow_dispatch` and `rerun-failed-jobs` both do).
    async fn request_no_content(
        &self,
        method: HttpMethod,
        path: &str,
        payload: Option<&serde_json::Value>,
    ) -> Result<(), BacklogError> {
        self.send_with_retry(method, path, &[], payload).await?;
        Ok(())
    }

    async fn paginate<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        query: &[(&str, String)],
        items: fn(serde_json::Value) -> serde_json::Value,
    ) -> Result<Vec<T>, BacklogError> {
        let mut all = Vec::new();
        let mut page = 1;
        let mut last_page = false;
        while !last_page {
            let mut params = query.to_vec();
            params.push(("per_page", GITHUB_PAGE_SIZE.to_string()));
            params.push(("page", page.to_string()));
            let value = items(self.get_json(path, &params).await?);
            let page_items: Vec<T> = serde_json::from_value(value)?;
            last_page = page_items.len() < GITHUB_PAGE_SIZE;
            all.extend(page_items);
            page += 1;
        }
        Ok(all)
    }
}

#[async_trait]
impl GithubClient for ReqwestGithubClient {
    async fn search_open_issues(
        &self,
        repo: &str,
        query: &str,
    ) -> Result<Vec<GithubIssue>, BacklogError> {
        let q = format!("repo:{repo} is:issue is:open {query}");
        self.paginate("/search/issues", &[("q", q)], |v| v["items"].clone())
            .await
    }

    async fn open_pull_requests(&self, repo: &str) -> Result<Vec<GithubPullRequest>, BacklogError> {
        self.paginate(
            &format!("/repos/{repo}/pulls"),
            &[("state", "open".to_string())],
            |v| v,
        )
        .await
    }

    async fn create_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<GithubIssue, BacklogError> {
        let payload = serde_json::json!({ "title": title, "body": body, "labels": labels });
        let value = self
            .post_json(&format!("/repos/{repo}/issues"), &payload)
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    async fn comment_on_issue(
        &self,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<(), BacklogError> {
        let payload = serde_json::json!({ "body": body });
        self.post_json(&format!("/repos/{repo}/issues/{number}/comments"), &payload)
            .await?;
        Ok(())
    }

    async fn create_draft_pull_request(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<GithubPullRequestCreated, BacklogError> {
        let payload = serde_json::json!({
            "title": title,
            "body": body,
            "head": head,
            "base": base,
            "draft": true,
        });
        let value = self
            .post_json(&format!("/repos/{repo}/pulls"), &payload)
            .await?;
        let raw: RawPrCreated = serde_json::from_value(value)?;
        Ok(raw.into())
    }

    async fn get_pull_request(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<GithubPullRequestDetail, BacklogError> {
        let value = self
            .get_json(&format!("/repos/{repo}/pulls/{number}"), &[])
            .await?;
        let raw: RawPrDetail = serde_json::from_value(value)?;
        Ok(raw.into())
    }

    async fn mark_pull_request_ready(&self, repo: &str, number: u64) -> Result<(), BacklogError> {
        let detail = self.get_pull_request(repo, number).await?;
        let payload = serde_json::json!({
            "query": "mutation($id: ID!) { markPullRequestReadyForReview(input: { pullRequestId: $id }) { pullRequest { isDraft } } }",
            "variables": { "id": detail.node_id },
        });
        let url = format!("{}/graphql", self.base_url);
        let value = self.post_json("/graphql", &payload).await?;
        if let Some(errors) = value.get("errors").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                let message = errors
                    .iter()
                    .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(BacklogError::GraphQl { url, message });
            }
        }
        let is_draft = value
            .pointer("/data/markPullRequestReadyForReview/pullRequest/isDraft")
            .and_then(|v| v.as_bool());
        if is_draft != Some(false) {
            return Err(BacklogError::GraphQl {
                url,
                message: "markPullRequestReadyForReview reported no errors but did not confirm pullRequest.isDraft is false".to_string(),
            });
        }
        Ok(())
    }

    async fn close_pull_request(&self, repo: &str, number: u64) -> Result<(), BacklogError> {
        let payload = serde_json::json!({ "state": "closed" });
        self.patch_json(&format!("/repos/{repo}/pulls/{number}"), &payload)
            .await?;
        Ok(())
    }

    async fn list_review_comments(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Vec<GithubComment>, BacklogError> {
        let raw: Vec<RawComment> = self
            .paginate(
                &format!("/repos/{repo}/pulls/{number}/comments"),
                &[],
                |v| v,
            )
            .await?;
        Ok(raw.into_iter().map(GithubComment::from).collect())
    }

    async fn list_issue_comments(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Vec<GithubComment>, BacklogError> {
        let raw: Vec<RawComment> = self
            .paginate(
                &format!("/repos/{repo}/issues/{number}/comments"),
                &[],
                |v| v,
            )
            .await?;
        Ok(raw.into_iter().map(GithubComment::from).collect())
    }

    async fn get_issue(&self, repo: &str, number: u64) -> Result<GithubIssueDetail, BacklogError> {
        let value = self
            .get_json(&format!("/repos/{repo}/issues/{number}"), &[])
            .await?;
        let raw: RawIssueDetail = serde_json::from_value(value)?;
        Ok(raw.into())
    }
}

#[async_trait]
impl GithubActionsClient for ReqwestGithubClient {
    async fn dispatch_workflow(
        &self,
        repo: &str,
        workflow: &str,
        git_ref: &str,
        inputs: serde_json::Value,
    ) -> Result<(), BacklogError> {
        let payload = serde_json::json!({ "ref": git_ref, "inputs": inputs });
        self.request_no_content(
            HttpMethod::Post,
            &format!("/repos/{repo}/actions/workflows/{workflow}/dispatches"),
            Some(&payload),
        )
        .await
    }

    async fn rerun_workflow(&self, repo: &str, run_id: u64) -> Result<(), BacklogError> {
        self.request_no_content(
            HttpMethod::Post,
            &format!("/repos/{repo}/actions/runs/{run_id}/rerun-failed-jobs"),
            None,
        )
        .await
    }

    async fn list_workflow_runs(
        &self,
        repo: &str,
        workflow: &str,
        branch: &str,
        event: &str,
    ) -> Result<Vec<WorkflowRun>, BacklogError> {
        let value = self
            .get_json(
                &format!("/repos/{repo}/actions/workflows/{workflow}/runs"),
                &[("branch", branch.to_string()), ("event", event.to_string())],
            )
            .await?;
        let runs = value.get("workflow_runs").cloned().unwrap_or(value);
        Ok(serde_json::from_value(runs)?)
    }

    async fn get_workflow_run(&self, repo: &str, run_id: u64) -> Result<WorkflowRun, BacklogError> {
        let value = self
            .get_json(&format!("/repos/{repo}/actions/runs/{run_id}"), &[])
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    async fn list_workflow_jobs(
        &self,
        repo: &str,
        run_id: u64,
    ) -> Result<Vec<WorkflowJob>, BacklogError> {
        let value = self
            .get_json(&format!("/repos/{repo}/actions/runs/{run_id}/jobs"), &[])
            .await?;
        let jobs = value.get("jobs").cloned().unwrap_or(value);
        Ok(serde_json::from_value(jobs)?)
    }

    async fn get_job_logs(&self, repo: &str, job_id: u64) -> Result<String, BacklogError> {
        self.request_text(
            HttpMethod::Get,
            &format!("/repos/{repo}/actions/jobs/{job_id}/logs"),
            &[],
            None,
        )
        .await
    }
}

/// Test doubles shared by `backlog`'s own tests and by other modules'
/// (`pr_tools`) tests that need a mocked [`GithubClient`], so there is one
/// mocked-client pattern for the whole crate rather than a parallel one per
/// consumer.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// A [`GithubClient`] double configured with canned responses. Every
    /// call is appended to [`MockGithub::calls`] so a test can assert on
    /// exactly what was sent (for example, that a created pull request's
    /// payload carried `draft: true`).
    #[derive(Default)]
    pub(crate) struct MockGithub {
        pub issues: HashMap<String, Vec<GithubIssue>>,
        pub pulls: HashMap<String, Vec<GithubPullRequest>>,
        pub pr_details: HashMap<(String, u64), GithubPullRequestDetail>,
        pub issue_details: HashMap<(String, u64), GithubIssueDetail>,
        pub review_comments: HashMap<(String, u64), Vec<GithubComment>>,
        pub issue_comments: HashMap<(String, u64), Vec<GithubComment>>,
        pub created_pr: Option<GithubPullRequestCreated>,
        /// When set, every call below except `get_pull_request`/`get_issue`
        /// (which fail on a missing map entry instead) returns this status
        /// as a [`BacklogError::Status`].
        pub fail_status: Option<u16>,
        pub(crate) calls: StdMutex<Vec<String>>,
    }

    impl MockGithub {
        /// Every call made so far, in order, as a human-readable summary.
        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("mock call log poisoned").clone()
        }

        fn record(&self, call: impl Into<String>) {
            self.calls
                .lock()
                .expect("mock call log poisoned")
                .push(call.into());
        }

        fn maybe_fail(&self, url: &str) -> Result<(), BacklogError> {
            match self.fail_status {
                Some(status) => Err(BacklogError::Status {
                    url: url.to_string(),
                    status,
                }),
                None => Ok(()),
            }
        }
    }

    #[async_trait]
    impl GithubClient for MockGithub {
        async fn search_open_issues(
            &self,
            repo: &str,
            _query: &str,
        ) -> Result<Vec<GithubIssue>, BacklogError> {
            Ok(self.issues.get(repo).cloned().unwrap_or_default())
        }

        async fn open_pull_requests(
            &self,
            repo: &str,
        ) -> Result<Vec<GithubPullRequest>, BacklogError> {
            Ok(self.pulls.get(repo).cloned().unwrap_or_default())
        }

        async fn create_issue(
            &self,
            _repo: &str,
            _title: &str,
            _body: &str,
            _labels: &[String],
        ) -> Result<GithubIssue, BacklogError> {
            unreachable!("ingestion never creates issues")
        }

        async fn comment_on_issue(
            &self,
            repo: &str,
            number: u64,
            body: &str,
        ) -> Result<(), BacklogError> {
            self.record(format!("comment_on_issue {repo}#{number}: {body}"));
            self.maybe_fail("mock:comment_on_issue")
        }

        async fn create_draft_pull_request(
            &self,
            repo: &str,
            title: &str,
            body: &str,
            head: &str,
            base: &str,
        ) -> Result<GithubPullRequestCreated, BacklogError> {
            self.record(format!(
                "create_draft_pull_request {repo} {head}->{base} title={title}\nbody={body}"
            ));
            self.maybe_fail("mock:create_draft_pull_request")?;
            self.created_pr.clone().ok_or_else(|| BacklogError::Status {
                url: "mock:create_draft_pull_request".to_string(),
                status: 500,
            })
        }

        async fn get_pull_request(
            &self,
            repo: &str,
            number: u64,
        ) -> Result<GithubPullRequestDetail, BacklogError> {
            self.pr_details
                .get(&(repo.to_string(), number))
                .cloned()
                .ok_or_else(|| BacklogError::Status {
                    url: format!("mock:pulls/{number}"),
                    status: 404,
                })
        }

        async fn mark_pull_request_ready(
            &self,
            repo: &str,
            number: u64,
        ) -> Result<(), BacklogError> {
            self.record(format!("mark_pull_request_ready {repo}#{number}"));
            self.maybe_fail("mock:mark_pull_request_ready")
        }

        async fn close_pull_request(&self, repo: &str, number: u64) -> Result<(), BacklogError> {
            self.record(format!("close_pull_request {repo}#{number}"));
            self.maybe_fail("mock:close_pull_request")
        }

        async fn list_review_comments(
            &self,
            repo: &str,
            number: u64,
        ) -> Result<Vec<GithubComment>, BacklogError> {
            self.maybe_fail("mock:list_review_comments")?;
            Ok(self
                .review_comments
                .get(&(repo.to_string(), number))
                .cloned()
                .unwrap_or_default())
        }

        async fn list_issue_comments(
            &self,
            repo: &str,
            number: u64,
        ) -> Result<Vec<GithubComment>, BacklogError> {
            self.maybe_fail("mock:list_issue_comments")?;
            Ok(self
                .issue_comments
                .get(&(repo.to_string(), number))
                .cloned()
                .unwrap_or_default())
        }

        async fn get_issue(
            &self,
            repo: &str,
            number: u64,
        ) -> Result<GithubIssueDetail, BacklogError> {
            self.issue_details
                .get(&(repo.to_string(), number))
                .cloned()
                .ok_or_else(|| BacklogError::Status {
                    url: format!("mock:issues/{number}"),
                    status: 404,
                })
        }
    }

    /// A [`GithubActionsClient`] double alongside [`MockGithub`]: `jobs`,
    /// `logs` and `list_result` are pre-seeded lookups; `runs` is mutable
    /// through [`Self::set_run`] so a test can script a run moving from
    /// `queued` to `completed` across successive `ci_status` polls.
    #[derive(Default)]
    pub(crate) struct MockGithubActions {
        pub runs: StdMutex<HashMap<u64, WorkflowRun>>,
        pub jobs: HashMap<u64, Vec<WorkflowJob>>,
        pub logs: HashMap<u64, String>,
        pub list_result: Vec<WorkflowRun>,
        /// When set, every call returns this status as a
        /// [`BacklogError::Status`].
        pub fail_status: Option<u16>,
        pub(crate) calls: StdMutex<Vec<String>>,
    }

    impl MockGithubActions {
        /// Every call made so far, in order, as a human-readable summary.
        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("mock call log poisoned").clone()
        }

        fn record(&self, call: impl Into<String>) {
            self.calls
                .lock()
                .expect("mock call log poisoned")
                .push(call.into());
        }

        fn maybe_fail(&self, url: &str) -> Result<(), BacklogError> {
            match self.fail_status {
                Some(status) => Err(BacklogError::Status {
                    url: url.to_string(),
                    status,
                }),
                None => Ok(()),
            }
        }

        /// Replace the run seen by
        /// [`GithubActionsClient::get_workflow_run`].
        pub(crate) fn set_run(&self, run: WorkflowRun) {
            self.runs
                .lock()
                .expect("mock run table poisoned")
                .insert(run.id, run);
        }
    }

    #[async_trait]
    impl GithubActionsClient for MockGithubActions {
        async fn dispatch_workflow(
            &self,
            repo: &str,
            workflow: &str,
            git_ref: &str,
            inputs: serde_json::Value,
        ) -> Result<(), BacklogError> {
            self.record(format!(
                "dispatch_workflow {repo} {workflow}@{git_ref} {inputs}"
            ));
            self.maybe_fail("mock:dispatch_workflow")
        }

        async fn rerun_workflow(&self, repo: &str, run_id: u64) -> Result<(), BacklogError> {
            self.record(format!("rerun_workflow {repo}#{run_id}"));
            self.maybe_fail("mock:rerun_workflow")
        }

        async fn list_workflow_runs(
            &self,
            repo: &str,
            workflow: &str,
            branch: &str,
            event: &str,
        ) -> Result<Vec<WorkflowRun>, BacklogError> {
            self.record(format!(
                "list_workflow_runs {repo} {workflow} {branch} {event}"
            ));
            self.maybe_fail("mock:list_workflow_runs")?;
            Ok(self.list_result.clone())
        }

        async fn get_workflow_run(
            &self,
            repo: &str,
            run_id: u64,
        ) -> Result<WorkflowRun, BacklogError> {
            self.record(format!("get_workflow_run {repo}#{run_id}"));
            self.maybe_fail("mock:get_workflow_run")?;
            self.runs
                .lock()
                .expect("mock run table poisoned")
                .get(&run_id)
                .cloned()
                .ok_or_else(|| BacklogError::Status {
                    url: format!("mock:runs/{run_id}"),
                    status: 404,
                })
        }

        async fn list_workflow_jobs(
            &self,
            repo: &str,
            run_id: u64,
        ) -> Result<Vec<WorkflowJob>, BacklogError> {
            self.record(format!("list_workflow_jobs {repo}#{run_id}"));
            self.maybe_fail("mock:list_workflow_jobs")?;
            Ok(self.jobs.get(&run_id).cloned().unwrap_or_default())
        }

        async fn get_job_logs(&self, repo: &str, job_id: u64) -> Result<String, BacklogError> {
            self.record(format!("get_job_logs {repo}#{job_id}"));
            self.maybe_fail("mock:get_job_logs")?;
            Ok(self.logs.get(&job_id).cloned().unwrap_or_default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::MockGithub;
    use super::*;
    use crate::scheduler::InMemoryQueueStore;
    use chrono::TimeZone;
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn issue(number: u64) -> GithubIssue {
        GithubIssue {
            number,
            title: format!("issue {number}"),
            body: Some(format!("body {number}")),
            html_url: format!("https://example.invalid/i/{number}"),
        }
    }

    fn source(repo: &str, path: &str) -> BacklogSource {
        BacklogSource {
            repo: repo.to_string(),
            repo_path: PathBuf::from(path),
            branch: "main".to_string(),
            query: "label:nanna".to_string(),
            identity: "sdlc-dev".to_string(),
            model: "m".to_string(),
            max_iterations: 5,
        }
    }

    fn config(sources: Vec<BacklogSource>, max_per_repo: Option<usize>) -> BacklogConfig {
        BacklogConfig {
            sources,
            max_per_repo,
        }
    }

    #[test]
    fn referenced_issues_parses_hash_numbers() {
        assert!(referenced_issues("no refs").is_empty());
        assert_eq!(
            referenced_issues("#1 and #22x #abc ##3")
                .into_iter()
                .collect::<Vec<_>>(),
            vec![1, 3, 22]
        );
    }

    #[test]
    fn describe_issue_includes_url_and_body() {
        let text = describe_issue("o/n", &issue(4));
        assert!(text.starts_with("Resolve GitHub issue o/n#4: issue 4"));
        assert!(text.contains("https://example.invalid/i/4"));
        assert!(text.ends_with("body 4"));
        let bare = GithubIssue {
            body: None,
            ..issue(5)
        };
        assert!(describe_issue("o/n", &bare).ends_with("\n\n"));
    }

    #[tokio::test]
    async fn sync_enqueues_and_dedupes_by_issue_number() {
        let github = MockGithub {
            issues: HashMap::from([("o/n".to_string(), vec![issue(1), issue(2)])]),
            pulls: HashMap::new(),
            ..Default::default()
        };
        let store = InMemoryQueueStore::default();
        let sink = StoreSink::open(Box::new(store.clone())).unwrap();
        let cfg = config(vec![source("o/n", "/repo")], None);

        let report = backlog_sync(&github, &sink, &cfg).await.unwrap();
        assert_eq!(report.enqueued.len(), 2);
        assert_eq!(report.duplicates, 0);
        let stored = store.load().unwrap();
        assert_eq!(stored.len(), 2);
        let first = stored
            .iter()
            .find(|t| t.origin.as_ref().unwrap().issue == 1)
            .unwrap();
        assert_eq!(first.identity_hint.as_deref(), Some("sdlc-dev"));
        assert_eq!(first.branch, "main");
        assert_eq!(first.max_iterations, 5);
        assert!(first.description.contains("issue 1"));

        let again = backlog_sync(&github, &sink, &cfg).await.unwrap();
        assert!(again.enqueued.is_empty());
        assert_eq!(again.duplicates, 2);
        assert_eq!(store.load().unwrap().len(), 2);

        let reopened = StoreSink::open(Box::new(store.clone())).unwrap();
        let third = backlog_sync(&github, &reopened, &cfg).await.unwrap();
        assert_eq!(third.duplicates, 2);
    }

    #[tokio::test]
    async fn sync_skips_issues_claimed_by_marked_open_prs() {
        let github = MockGithub {
            issues: HashMap::from([("o/n".to_string(), vec![issue(1), issue(2), issue(3)])]),
            pulls: HashMap::from([(
                "o/n".to_string(),
                vec![
                    GithubPullRequest {
                        number: 10,
                        body: Some("Closes #1\n\nNanna-Identity: sdlc-dev".to_string()),
                    },
                    GithubPullRequest {
                        number: 11,
                        body: Some("Closes #2 (human PR)".to_string()),
                    },
                    GithubPullRequest {
                        number: 12,
                        body: None,
                    },
                ],
            )]),
            ..Default::default()
        };
        let sink = StoreSink::open(Box::new(InMemoryQueueStore::default())).unwrap();
        let report = backlog_sync(&github, &sink, &config(vec![source("o/n", "/repo")], None))
            .await
            .unwrap();
        assert_eq!(report.claimed, 1);
        let enqueued: Vec<u64> = report.enqueued.iter().map(|o| o.issue).collect();
        assert_eq!(enqueued, vec![2, 3]);
    }

    #[tokio::test]
    async fn sync_honours_per_repo_cap_across_sources() {
        let github = MockGithub {
            issues: HashMap::from([
                ("o/a".to_string(), vec![issue(1), issue(2), issue(3)]),
                ("o/b".to_string(), vec![issue(1), issue(2)]),
            ]),
            pulls: HashMap::new(),
            ..Default::default()
        };
        let sink = StoreSink::open(Box::new(InMemoryQueueStore::default())).unwrap();
        sink.enqueue(QueuedTask::new(
            "manual",
            PathBuf::from("/a"),
            "main",
            "m",
            1,
        ))
        .await
        .unwrap();
        let cfg = config(vec![source("o/a", "/a"), source("o/b", "/b")], Some(2));
        let report = backlog_sync(&github, &sink, &cfg).await.unwrap();
        assert_eq!(report.capped, 2);
        assert_eq!(
            report.enqueued,
            vec![
                TaskOrigin {
                    repo: "o/a".to_string(),
                    issue: 1
                },
                TaskOrigin {
                    repo: "o/b".to_string(),
                    issue: 1
                },
                TaskOrigin {
                    repo: "o/b".to_string(),
                    issue: 2
                },
            ]
        );
        assert_eq!(sink.known().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn mock_github_records_pr_lifecycle_calls() {
        let mock = MockGithub {
            created_pr: Some(GithubPullRequestCreated {
                number: 1,
                html_url: "u".to_string(),
                node_id: "n".to_string(),
            }),
            pr_details: HashMap::from([(
                ("o/n".to_string(), 1),
                GithubPullRequestDetail {
                    number: 1,
                    node_id: "n".to_string(),
                    draft: true,
                    state: "open".to_string(),
                    body: None,
                    html_url: "u".to_string(),
                },
            )]),
            ..Default::default()
        };
        mock.create_draft_pull_request("o/n", "t", "b", "feature", "main")
            .await
            .unwrap();
        mock.mark_pull_request_ready("o/n", 1).await.unwrap();
        mock.close_pull_request("o/n", 1).await.unwrap();
        mock.comment_on_issue("o/n", 1, "reason").await.unwrap();
        let calls = mock.calls();
        assert_eq!(calls.len(), 4);
        assert!(calls[0].starts_with("create_draft_pull_request"));
        assert!(calls[1].starts_with("mark_pull_request_ready"));
        assert!(calls[2].starts_with("close_pull_request"));
        assert!(calls[3].starts_with("comment_on_issue"));
    }

    #[tokio::test]
    async fn manager_sink_submits_and_reports_pending_tasks() {
        use crate::task::TaskStatus;
        use model::provider::{ModelError, ModelResult};
        use model::types::{ChatRequest, ChatResponse, ModelInfo};

        struct NoProvider;
        #[async_trait]
        impl ModelProvider for NoProvider {
            async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
                Err(ModelError::Unknown {
                    message: "unused".to_string(),
                })
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

        let manager = Arc::new(TaskManager::new(0));
        let sink = ManagerSink::new(Arc::clone(&manager), Arc::new(NoProvider));
        let github = MockGithub {
            issues: HashMap::from([("o/n".to_string(), vec![issue(7)])]),
            pulls: HashMap::new(),
            ..Default::default()
        };
        let cfg = config(vec![source("o/n", "/repo")], None);
        let report = backlog_sync(&github, &sink, &cfg).await.unwrap();
        assert_eq!(report.enqueued.len(), 1);
        let tasks = manager.list().await;
        assert_eq!(tasks.len(), 1);
        assert!(matches!(tasks[0].status, TaskStatus::Pending));
        assert_eq!(
            tasks[0].origin,
            Some(TaskOrigin {
                repo: "o/n".to_string(),
                issue: 7
            })
        );
        assert_eq!(
            backlog_sync(&github, &sink, &cfg).await.unwrap().duplicates,
            1
        );
        manager.cancel(&tasks[0].id).await.unwrap();
        assert!(sink.known().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn store_sink_surfaces_store_errors() {
        struct Broken;
        impl QueueStore for Broken {
            fn load(&self) -> Result<Vec<QueuedTask>, QueueStoreError> {
                Err(QueueStoreError::Rejected("load".to_string()))
            }
            fn insert(&self, _task: &QueuedTask) -> Result<(), QueueStoreError> {
                Err(QueueStoreError::Rejected("insert".to_string()))
            }
            fn remove(&self, _id: &crate::task::TaskId) -> Result<(), QueueStoreError> {
                Ok(())
            }
        }
        assert!(StoreSink::open(Box::new(Broken)).is_err());
        let sink = StoreSink {
            store: Box::new(Broken),
            queue: Mutex::new(TaskQueue::new()),
        };
        let err = sink
            .enqueue(QueuedTask::new("t", PathBuf::from("/r"), "main", "m", 1))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("queue store failed"));
        let github = MockGithub {
            issues: HashMap::from([("o/n".to_string(), vec![issue(1)])]),
            pulls: HashMap::new(),
            ..Default::default()
        };
        let cfg = config(vec![source("o/n", "/repo")], None);
        assert!(backlog_sync(&github, &sink, &cfg).await.is_err());
    }

    /// One scripted response for [`fake_github_ext`]: `method` is an HTTP
    /// verb or `"*"` for any, `path_contains` a substring of the request
    /// target. Routes are consumed in the order they match, so the same
    /// target can be scripted to answer differently across attempts (for
    /// example a `429` followed by a `200`, to exercise retry).
    struct MockRoute {
        method: &'static str,
        path_contains: &'static str,
        status: u16,
        body: String,
        headers: Vec<(&'static str, &'static str)>,
    }

    impl MockRoute {
        fn new(method: &'static str, path_contains: &'static str, status: u16, body: &str) -> Self {
            Self {
                method,
                path_contains,
                status,
                body: body.to_string(),
                headers: Vec::new(),
            }
        }

        fn with_headers(mut self, headers: Vec<(&'static str, &'static str)>) -> Self {
            self.headers = headers;
            self
        }
    }

    /// A mock GitHub server. Returns the base URL, the raw request lines
    /// received so far (`"METHOD target body"`), and the server task handle.
    async fn fake_github_ext(
        routes: Vec<MockRoute>,
    ) -> (
        String,
        Arc<StdMutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = Arc::new(StdMutex::new(routes));
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let requests_for_server = Arc::clone(&requests);
        let handle = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 65536];
                let n = socket.read(&mut buf).await.unwrap();
                let raw = String::from_utf8_lossy(&buf[..n]).to_string();
                let request_line = raw.split("\r\n").next().unwrap_or("").to_string();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let target = parts.next().unwrap_or("").to_string();
                let body = raw
                    .split("\r\n\r\n")
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                requests_for_server
                    .lock()
                    .unwrap()
                    .push(format!("{method} {target} {body}"));
                let (status, body, headers) = {
                    let mut guard = routes.lock().unwrap();
                    let idx = guard.iter().position(|route| {
                        (route.method == "*" || route.method == method)
                            && target.contains(route.path_contains)
                    });
                    match idx {
                        Some(i) => {
                            let route = guard.remove(i);
                            (route.status, route.body, route.headers)
                        }
                        None => (404, "{}".to_string(), Vec::new()),
                    }
                };
                let mut extra_headers = String::new();
                for (key, value) in &headers {
                    extra_headers.push_str(&format!("{key}: {value}\r\n"));
                }
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        (base, requests, handle)
    }

    async fn fake_github(
        routes: Vec<(&'static str, u16, String)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let routes = routes
            .into_iter()
            .map(|(path, status, body)| MockRoute::new("*", path, status, &body))
            .collect();
        let (base, _requests, handle) = fake_github_ext(routes).await;
        (base, handle)
    }

    fn issues_json(numbers: std::ops::Range<u64>) -> String {
        let items: Vec<serde_json::Value> = numbers
            .map(|n| serde_json::json!({"number": n, "title": format!("t{n}"), "html_url": "u"}))
            .collect();
        serde_json::json!({ "items": items }).to_string()
    }

    #[tokio::test]
    async fn reqwest_client_paginates_search_and_pulls() {
        let (base, server) = fake_github(vec![
            ("/search/issues?q=repo%3Ao%2Fn+is%3Aissue+is%3Aopen+label%3Ananna&per_page=100&page=1", 200, issues_json(1..101)),
            ("/search/issues?q=repo%3Ao%2Fn+is%3Aissue+is%3Aopen+label%3Ananna&per_page=100&page=2", 200, issues_json(101..103)),
            ("/repos/o/n/pulls?state=open&per_page=100&page=1", 200, r#"[{"number": 9, "body": "Nanna-Identity: x"}]"#.to_string()),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, Some("token".to_string()));
        let issues = client
            .search_open_issues("o/n", "label:nanna")
            .await
            .unwrap();
        assert_eq!(issues.len(), 102);
        assert_eq!(issues[101].number, 102);
        let pulls = client.open_pull_requests("o/n").await.unwrap();
        assert_eq!(
            pulls,
            vec![GithubPullRequest {
                number: 9,
                body: Some("Nanna-Identity: x".to_string())
            }]
        );
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_creates_issues_and_comments() {
        let (base, server) = fake_github(vec![
            ("/repos/o/n/issues/7/comments", 201, "{}".to_string()),
            ("/repos/o/n/issues", 201, r#"{"number": 7, "title": "t", "body": "b", "html_url": "https://example.invalid/i/7"}"#.to_string()),
            ("/repos/o/bad/issues", 422, "{}".to_string()),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, Some("token".to_string()));
        let issue = client
            .create_issue("o/n", "t", "b", &["nanna-escalation".to_string()])
            .await
            .unwrap();
        assert_eq!(issue.number, 7);
        assert_eq!(issue.html_url, "https://example.invalid/i/7");
        client.comment_on_issue("o/n", 7, "again").await.unwrap();
        let err = client
            .create_issue("o/bad", "t", "b", &[])
            .await
            .unwrap_err();
        assert!(matches!(err, BacklogError::Status { status: 422, .. }));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_reports_status_parse_and_transport_errors() {
        let (base, server) = fake_github(vec![
            ("/repos/o/forbidden/pulls", 403, "{}".to_string()),
            ("/repos/o/garbage/pulls", 200, "not json".to_string()),
            ("/repos/o/shape/pulls", 200, r#"[{"body": 1}]"#.to_string()),
        ])
        .await;
        let client = ReqwestGithubClient::new(base.clone(), None);
        let err = client.open_pull_requests("o/forbidden").await.unwrap_err();
        assert!(matches!(err, BacklogError::Status { status: 403, .. }));
        assert!(err.to_string().contains("HTTP 403"));
        assert!(matches!(
            client.open_pull_requests("o/garbage").await.unwrap_err(),
            BacklogError::Parse(_)
        ));
        assert!(matches!(
            client.open_pull_requests("o/shape").await.unwrap_err(),
            BacklogError::Parse(_)
        ));
        server.abort();
        let unreachable = ReqwestGithubClient::new("http://127.0.0.1:1", None);
        assert!(matches!(
            unreachable.open_pull_requests("o/n").await.unwrap_err(),
            BacklogError::Http(_)
        ));
        assert_eq!(ReqwestGithubClient::github(None).base_url, GITHUB_API_URL);
    }

    #[tokio::test]
    async fn reqwest_client_creates_draft_pull_request_always_sends_draft_true() {
        let (base, requests, server) = fake_github_ext(vec![MockRoute::new(
            "POST",
            "/repos/o/n/pulls",
            201,
            r#"{"number": 42, "node_id": "PR_kwid", "html_url": "https://example.invalid/pr/42"}"#,
        )])
        .await;
        let client = ReqwestGithubClient::new(base, Some("token".to_string()));
        let created = client
            .create_draft_pull_request("o/n", "title", "body", "feature", "main")
            .await
            .unwrap();
        assert_eq!(created.number, 42);
        assert_eq!(created.node_id, "PR_kwid");
        assert_eq!(created.html_url, "https://example.invalid/pr/42");
        let sent = requests.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("\"draft\":true"));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_gets_pull_request_detail() {
        let (base, server) = fake_github(vec![(
            "/repos/o/n/pulls/7",
            200,
            r#"{"number": 7, "node_id": "PR_x", "draft": true, "state": "open", "body": "desc", "html_url": "u"}"#.to_string(),
        )])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        let detail = client.get_pull_request("o/n", 7).await.unwrap();
        assert_eq!(detail.number, 7);
        assert_eq!(detail.node_id, "PR_x");
        assert!(detail.draft);
        assert_eq!(detail.state, "open");
        assert_eq!(detail.body.as_deref(), Some("desc"));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_marks_pull_request_ready_via_graphql() {
        let (base, requests, server) = fake_github_ext(vec![
            MockRoute::new(
                "GET",
                "/repos/o/n/pulls/7",
                200,
                r#"{"number": 7, "node_id": "PR_x", "draft": true, "state": "open", "body": null, "html_url": "u"}"#,
            ),
            MockRoute::new(
                "POST",
                "/graphql",
                200,
                r#"{"data": {"markPullRequestReadyForReview": {"pullRequest": {"isDraft": false}}}}"#,
            ),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, Some("token".to_string()));
        client.mark_pull_request_ready("o/n", 7).await.unwrap();
        let sent = requests.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert!(sent[1].starts_with("POST /graphql"));
        assert!(sent[1].contains("markPullRequestReadyForReview"));
        assert!(sent[1].contains("PR_x"));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_mark_pull_request_ready_surfaces_graphql_errors() {
        let (base, _requests, server) = fake_github_ext(vec![
            MockRoute::new(
                "GET",
                "/repos/o/n/pulls/7",
                200,
                r#"{"number": 7, "node_id": "PR_x", "draft": true, "state": "open", "body": null, "html_url": "u"}"#,
            ),
            MockRoute::new(
                "POST",
                "/graphql",
                200,
                r#"{"errors": [{"message": "Could not resolve to a node"}]}"#,
            ),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        let err = client.mark_pull_request_ready("o/n", 7).await.unwrap_err();
        assert!(matches!(err, BacklogError::GraphQl { .. }));
        assert!(err.to_string().contains("Could not resolve to a node"));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_mark_pull_request_ready_rejects_a_silent_no_op() {
        let (base, _requests, server) = fake_github_ext(vec![
            MockRoute::new(
                "GET",
                "/repos/o/n/pulls/7",
                200,
                r#"{"number": 7, "node_id": "PR_x", "draft": true, "state": "open", "body": null, "html_url": "u"}"#,
            ),
            MockRoute::new(
                "POST",
                "/graphql",
                200,
                r#"{"data": {"markPullRequestReadyForReview": {"pullRequest": {"isDraft": true}}}}"#,
            ),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        let err = client.mark_pull_request_ready("o/n", 7).await.unwrap_err();
        assert!(matches!(err, BacklogError::GraphQl { .. }));
        assert!(err.to_string().contains("did not confirm"));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_mark_pull_request_ready_rejects_a_missing_is_draft_field() {
        let (base, _requests, server) = fake_github_ext(vec![
            MockRoute::new(
                "GET",
                "/repos/o/n/pulls/7",
                200,
                r#"{"number": 7, "node_id": "PR_x", "draft": true, "state": "open", "body": null, "html_url": "u"}"#,
            ),
            MockRoute::new("POST", "/graphql", 200, r#"{"data": {}}"#),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        let err = client.mark_pull_request_ready("o/n", 7).await.unwrap_err();
        assert!(matches!(err, BacklogError::GraphQl { .. }));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_closes_pull_request_patches_state_closed() {
        let (base, requests, server) = fake_github_ext(vec![MockRoute::new(
            "PATCH",
            "/repos/o/n/pulls/9",
            200,
            r#"{"number": 9, "state": "closed"}"#,
        )])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        client.close_pull_request("o/n", 9).await.unwrap();
        let sent = requests.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].starts_with("PATCH /repos/o/n/pulls/9"));
        assert!(sent[0].contains("\"state\":\"closed\""));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_lists_review_and_issue_comments() {
        let (base, server) = fake_github(vec![
            (
                "/repos/o/n/pulls/3/comments?per_page=100&page=1",
                200,
                r#"[{"id": 1, "user": {"login": "alice"}, "body": "inline", "html_url": "u1"}]"#.to_string(),
            ),
            (
                "/repos/o/n/issues/3/comments?per_page=100&page=1",
                200,
                r#"[{"id": 2, "user": {"login": "bob"}, "body": "conversation", "html_url": "u2"}]"#.to_string(),
            ),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        let review = client.list_review_comments("o/n", 3).await.unwrap();
        assert_eq!(
            review,
            vec![GithubComment {
                id: 1,
                author: "alice".to_string(),
                body: "inline".to_string(),
                html_url: "u1".to_string()
            }]
        );
        let issue = client.list_issue_comments("o/n", 3).await.unwrap();
        assert_eq!(
            issue,
            vec![GithubComment {
                id: 2,
                author: "bob".to_string(),
                body: "conversation".to_string(),
                html_url: "u2".to_string()
            }]
        );
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_gets_issue_detail_with_labels() {
        let (base, server) = fake_github(vec![(
            "/repos/o/n/issues/5",
            200,
            r#"{"number": 5, "title": "t", "body": "b", "labels": [{"name": "bug"}, {"name": "sdlc"}], "html_url": "u"}"#.to_string(),
        )])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        let detail = client.get_issue("o/n", 5).await.unwrap();
        assert_eq!(detail.number, 5);
        assert_eq!(detail.title, "t");
        assert_eq!(detail.body.as_deref(), Some("b"));
        assert_eq!(detail.labels, vec!["bug".to_string(), "sdlc".to_string()]);
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_retries_429_then_succeeds() {
        let (base, requests, server) = fake_github_ext(vec![
            MockRoute::new("GET", "/repos/o/n/issues/1", 429, "{}")
                .with_headers(vec![("Retry-After", "0")]),
            MockRoute::new(
                "GET",
                "/repos/o/n/issues/1",
                200,
                r#"{"number": 1, "title": "t", "body": null, "labels": [], "html_url": "u"}"#,
            ),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, None)
            .with_retry_policy(3, std::time::Duration::from_millis(1));
        let detail = client.get_issue("o/n", 1).await.unwrap();
        assert_eq!(detail.number, 1);
        assert_eq!(requests.lock().unwrap().len(), 2);
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_retries_a_bare_429_with_no_rate_limit_headers() {
        let (base, requests, server) = fake_github_ext(vec![
            MockRoute::new("GET", "/repos/o/n/issues/1", 429, "{}"),
            MockRoute::new(
                "GET",
                "/repos/o/n/issues/1",
                200,
                r#"{"number": 1, "title": "t", "body": null, "labels": [], "html_url": "u"}"#,
            ),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, None)
            .with_retry_policy(3, std::time::Duration::from_millis(1));
        let detail = client.get_issue("o/n", 1).await.unwrap();
        assert_eq!(detail.number, 1);
        assert_eq!(requests.lock().unwrap().len(), 2);
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_retry_waits_at_least_as_long_as_retry_after() {
        let (base, requests, server) = fake_github_ext(vec![
            MockRoute::new("GET", "/repos/o/n/issues/1", 429, "{}")
                .with_headers(vec![("Retry-After", "1")]),
            MockRoute::new(
                "GET",
                "/repos/o/n/issues/1",
                200,
                r#"{"number": 1, "title": "t", "body": null, "labels": [], "html_url": "u"}"#,
            ),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, None)
            .with_retry_policy(3, std::time::Duration::from_millis(1));
        let start = std::time::Instant::now();
        client.get_issue("o/n", 1).await.unwrap();
        assert!(
            start.elapsed() >= std::time::Duration::from_secs(1),
            "must wait at least the server's Retry-After, not just the base backoff"
        );
        assert_eq!(requests.lock().unwrap().len(), 2);
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_retry_after_beyond_the_cap_fails_immediately() {
        let (base, requests, server) = fake_github_ext(vec![MockRoute::new(
            "GET",
            "/repos/o/n/issues/1",
            429,
            "{}",
        )
        .with_headers(vec![("Retry-After", "120")])])
        .await;
        let client = ReqwestGithubClient::new(base, None)
            .with_retry_policy(5, std::time::Duration::from_millis(1));
        let start = std::time::Instant::now();
        let err = client.get_issue("o/n", 1).await.unwrap_err();
        match err {
            BacklogError::RateLimited {
                attempts,
                retry_after_secs,
                ..
            } => {
                assert_eq!(attempts, 1);
                assert_eq!(retry_after_secs, Some(120));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "must not sleep when Retry-After exceeds the cap"
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_403_with_remaining_zero_retries_then_exhausts() {
        let route = || {
            MockRoute::new("GET", "/repos/o/n/issues/1", 403, "{}").with_headers(vec![
                ("X-RateLimit-Remaining", "0"),
                ("X-RateLimit-Reset", "0"),
            ])
        };
        let (base, requests, server) = fake_github_ext(vec![route(), route()]).await;
        let client = ReqwestGithubClient::new(base, None)
            .with_retry_policy(2, std::time::Duration::from_millis(1));
        let err = client.get_issue("o/n", 1).await.unwrap_err();
        match err {
            BacklogError::RateLimited {
                attempts,
                retry_after_secs,
                ..
            } => {
                assert_eq!(attempts, 2);
                assert_eq!(retry_after_secs, Some(0));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
        assert_eq!(requests.lock().unwrap().len(), 2);
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_403_with_remaining_zero_and_no_reset_header_defaults_to_zero() {
        let (base, _requests, server) = fake_github_ext(vec![MockRoute::new(
            "GET",
            "/repos/o/n/issues/1",
            403,
            "{}",
        )
        .with_headers(vec![("X-RateLimit-Remaining", "0")])])
        .await;
        let client = ReqwestGithubClient::new(base, None)
            .with_retry_policy(1, std::time::Duration::from_millis(1));
        let err = client.get_issue("o/n", 1).await.unwrap_err();
        match err {
            BacklogError::RateLimited {
                retry_after_secs, ..
            } => assert_eq!(retry_after_secs, Some(0)),
            other => panic!("expected RateLimited, got {other:?}"),
        }
        server.abort();
    }

    #[test]
    fn with_retry_policy_clamps_an_oversized_max_attempts() {
        // The exponential backoff shift (`1u32 << (attempt - 1)`) is only
        // defined for shift amounts below 32; an unclamped caller-supplied
        // max_attempts would panic (debug) or wrap (release) once enough
        // retries were exhausted to reach it.
        let client = ReqwestGithubClient::new("http://example.invalid", None)
            .with_retry_policy(u32::MAX, std::time::Duration::from_millis(1));
        assert_eq!(client.max_attempts, MAX_RETRY_ATTEMPTS);
    }

    #[test]
    fn with_retry_policy_still_floors_at_one() {
        let client = ReqwestGithubClient::new("http://example.invalid", None)
            .with_retry_policy(0, std::time::Duration::from_millis(1));
        assert_eq!(client.max_attempts, 1);
    }

    #[tokio::test]
    async fn reqwest_client_plain_403_is_not_retried() {
        let (base, requests, server) = fake_github_ext(vec![MockRoute::new(
            "GET",
            "/repos/o/n/issues/1",
            403,
            "{}",
        )])
        .await;
        let client = ReqwestGithubClient::new(base, None)
            .with_retry_policy(3, std::time::Duration::from_millis(1));
        let err = client.get_issue("o/n", 1).await.unwrap_err();
        assert!(matches!(err, BacklogError::Status { status: 403, .. }));
        assert_eq!(requests.lock().unwrap().len(), 1);
        server.abort();
    }

    fn run(id: u64, status: &str, conclusion: Option<&str>) -> WorkflowRun {
        WorkflowRun {
            id,
            name: Some("ci".to_string()),
            status: status.to_string(),
            conclusion: conclusion.map(str::to_string),
            html_url: format!("https://example.invalid/runs/{id}"),
            head_branch: None,
            run_started_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn is_rerunnable_failure_on_requires_a_matching_branch_and_a_failure_conclusion() {
        let mut failed = run(1, "completed", Some("failure"));
        failed.head_branch = Some("feat/x".to_string());
        assert!(failed.is_rerunnable_failure_on("feat/x"));
        assert!(!failed.is_rerunnable_failure_on("main"));

        let mut succeeded = run(2, "completed", Some("success"));
        succeeded.head_branch = Some("feat/x".to_string());
        assert!(!succeeded.is_rerunnable_failure_on("feat/x"));

        let mut timed_out = run(3, "completed", Some("timed_out"));
        timed_out.head_branch = Some("feat/x".to_string());
        assert!(timed_out.is_rerunnable_failure_on("feat/x"));

        let no_branch = run(4, "completed", Some("failure"));
        assert!(!no_branch.is_rerunnable_failure_on("feat/x"));
    }

    #[test]
    fn duration_minutes_is_none_unless_completed_with_both_timestamps_in_order() {
        let started = Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap();
        let ended = started + chrono::Duration::minutes(9);
        let mut completed = run(1, "completed", Some("success"));
        completed.run_started_at = Some(started);
        completed.updated_at = Some(ended);
        assert_eq!(completed.duration_minutes(), Some(9.0));

        let in_progress = WorkflowRun {
            run_started_at: Some(started),
            updated_at: Some(ended),
            ..run(2, "in_progress", None)
        };
        assert_eq!(in_progress.duration_minutes(), None);

        let missing_start = WorkflowRun {
            updated_at: Some(ended),
            ..run(3, "completed", Some("success"))
        };
        assert_eq!(missing_start.duration_minutes(), None);

        let missing_end = WorkflowRun {
            run_started_at: Some(started),
            ..run(4, "completed", Some("success"))
        };
        assert_eq!(missing_end.duration_minutes(), None);

        let backwards = WorkflowRun {
            run_started_at: Some(ended),
            updated_at: Some(started),
            ..run(5, "completed", Some("success"))
        };
        assert_eq!(backwards.duration_minutes(), None);
    }

    #[test]
    fn workflow_job_failed_covers_every_terminal_conclusion() {
        let job = |conclusion: Option<&str>| WorkflowJob {
            id: 1,
            name: "build".to_string(),
            status: "completed".to_string(),
            conclusion: conclusion.map(str::to_string),
            steps: vec![],
        };
        assert!(job(Some("failure")).failed());
        assert!(job(Some("timed_out")).failed());
        assert!(job(Some("cancelled")).failed());
        assert!(!job(Some("success")).failed());
        assert!(!job(None).failed());
    }

    #[tokio::test]
    async fn reqwest_client_dispatches_a_workflow_with_204_no_content() {
        let (base, requests, server) = fake_github_ext(vec![MockRoute::new(
            "POST",
            "/actions/workflows/ci.yml/dispatches",
            204,
            "",
        )])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        client
            .dispatch_workflow("o/n", "ci.yml", "feat/x", serde_json::json!({"k": "v"}))
            .await
            .unwrap();
        let sent = requests.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("POST /repos/o/n/actions/workflows/ci.yml/dispatches"));
        assert!(sent[0].contains("\"ref\":\"feat/x\""));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_dispatch_workflow_surfaces_a_status_error() {
        let (base, _requests, server) =
            fake_github_ext(vec![MockRoute::new("POST", "/dispatches", 404, "{}")]).await;
        let client = ReqwestGithubClient::new(base, None);
        let err = client
            .dispatch_workflow("o/n", "ci.yml", "feat/x", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, BacklogError::Status { status: 404, .. }));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_reruns_a_workflow_with_201_no_content() {
        let (base, requests, server) = fake_github_ext(vec![MockRoute::new(
            "POST",
            "/actions/runs/9/rerun-failed-jobs",
            201,
            "",
        )])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        client.rerun_workflow("o/n", 9).await.unwrap();
        assert!(requests.lock().unwrap()[0].contains("runs/9/rerun-failed-jobs"));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_lists_workflow_runs_unwraps_the_envelope() {
        let body = serde_json::json!({
            "total_count": 1,
            "workflow_runs": [
                {"id": 42, "status": "queued", "html_url": "https://example.invalid/runs/42"},
            ],
        })
        .to_string();
        let (base, requests, server) = fake_github_ext(vec![MockRoute::new(
            "GET",
            "/actions/workflows/ci.yml/runs",
            200,
            &body,
        )])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        let runs = client
            .list_workflow_runs("o/n", "ci.yml", "feat/x", "workflow_dispatch")
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, 42);
        assert_eq!(runs[0].status, "queued");
        assert!(requests.lock().unwrap()[0].contains("branch=feat%2Fx"));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_gets_a_workflow_run() {
        let body = serde_json::json!({
            "id": 42,
            "status": "completed",
            "conclusion": "success",
            "html_url": "https://example.invalid/runs/42",
            "run_started_at": "2026-09-26T10:00:00Z",
            "updated_at": "2026-09-26T10:09:00Z",
        })
        .to_string();
        let (base, _requests, server) =
            fake_github_ext(vec![MockRoute::new("GET", "/actions/runs/42", 200, &body)]).await;
        let client = ReqwestGithubClient::new(base, None);
        let run = client.get_workflow_run("o/n", 42).await.unwrap();
        assert_eq!(run.status, "completed");
        assert_eq!(run.conclusion.as_deref(), Some("success"));
        assert_eq!(run.duration_minutes(), Some(9.0));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_lists_workflow_jobs_unwraps_the_envelope() {
        let body = serde_json::json!({
            "jobs": [
                {
                    "id": 7,
                    "name": "test",
                    "status": "completed",
                    "conclusion": "failure",
                    "steps": [
                        {"name": "cargo test", "status": "completed", "conclusion": "failure", "number": 3},
                    ],
                },
            ],
        })
        .to_string();
        let (base, _requests, server) = fake_github_ext(vec![MockRoute::new(
            "GET",
            "/actions/runs/42/jobs",
            200,
            &body,
        )])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        let jobs = client.list_workflow_jobs("o/n", 42).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].failed());
        assert_eq!(jobs[0].steps[0].name, "cargo test");
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_fetches_job_logs_across_a_redirect() {
        let (base, _requests, server) = fake_github_ext(vec![
            MockRoute::new("GET", "/actions/jobs/7/logs", 302, "")
                .with_headers(vec![("Location", "/blob/7")]),
            MockRoute::new("GET", "/blob/7", 200, "cargo test failed\nsee above\n"),
        ])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        let logs = client.get_job_logs("o/n", 7).await.unwrap();
        assert!(logs.contains("cargo test failed"));
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_client_job_logs_surfaces_a_status_error() {
        let (base, _requests, server) = fake_github_ext(vec![MockRoute::new(
            "GET",
            "/actions/jobs/7/logs",
            410,
            "{}",
        )])
        .await;
        let client = ReqwestGithubClient::new(base, None);
        let err = client.get_job_logs("o/n", 7).await.unwrap_err();
        assert!(matches!(err, BacklogError::Status { status: 410, .. }));
        server.abort();
    }
}
