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
///     backlog_sync, BacklogConfig, BacklogError, BacklogSource, GithubClient, GithubIssue,
///     GithubPullRequest, StoreSink,
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

/// [`GithubClient`] over the GitHub REST API using `reqwest`.
///
/// Results are paginated `GITHUB_PAGE_SIZE` at a time until a short page is
/// returned. A token, when given, is sent as a bearer token.
#[derive(Debug, Clone)]
pub struct ReqwestGithubClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl ReqwestGithubClient {
    /// Client for the API rooted at `base_url` (no trailing slash).
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            token,
        }
    }

    /// Client for the public GitHub API.
    pub fn github(token: Option<String>) -> Self {
        Self::new(GITHUB_API_URL, token)
    }

    async fn get_json(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<serde_json::Value, BacklogError> {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self
            .http
            .get(&url)
            .query(query)
            .header(reqwest::header::USER_AGENT, "nanna-coder")
            .header(reqwest::header::ACCEPT, "application/vnd.github+json");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(BacklogError::Status {
                url,
                status: status.as_u16(),
            });
        }
        Ok(serde_json::from_str(&response.text().await?)?)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::InMemoryQueueStore;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct MockGithub {
        issues: HashMap<String, Vec<GithubIssue>>,
        pulls: HashMap<String, Vec<GithubPullRequest>>,
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
    }

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
        };
        let cfg = config(vec![source("o/n", "/repo")], None);
        assert!(backlog_sync(&github, &sink, &cfg).await.is_err());
    }

    async fn fake_github(
        routes: Vec<(&'static str, u16, String)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let n = socket.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let target = head.split_whitespace().nth(1).unwrap().to_string();
                let (status, body) = routes
                    .iter()
                    .find(|(needle, _, _)| target.contains(needle))
                    .map(|(_, status, body)| (*status, body.clone()))
                    .unwrap_or((404, "{}".to_string()));
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
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
}
