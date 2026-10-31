use harness::auditor::{AuditContext, AuditLog, Auditor, RuleAuditor, SpawnGate, TaskSummary};
use harness::escalation::{
    Escalation, EscalationLog, EscalationOutcome, EscalationPlane, EscalationSource,
    ProtectedPathEscalation, Severity, SinkSettings, ESCALATION_GITHUB_TOKEN_ENV,
    ESCALATION_REPO_ENV, ESCALATION_WEBHOOK_URL_ENV, RECORD_FILE_NAME,
};
use harness::identity::AUDITOR_IDENTITY;
use harness::leases::SystemClock;
use harness::protected::{AuditHook, ProtectedPathViolation};
use harness::IdentityCatalog;
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const TOKEN: &str = "ghp_E2Etoken0123456789abcdefghijklmnopqr";
const HOOK_SECRET: &str = "T0P-S3CR3T-HOOK-PATH";
const PINNED: &str = "ops/escalations";

#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedLog {
    type Writer = SharedLog;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn logs() -> &'static SharedLog {
    static LOGS: OnceLock<SharedLog> = OnceLock::new();
    LOGS.get_or_init(|| {
        let shared = SharedLog::default();
        tracing_subscriber::fmt()
            .with_writer(shared.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .try_init()
            .expect("this test binary installs the only global subscriber");
        shared
    })
}

fn logged() -> String {
    std::io::Write::flush(&mut logs().clone()).unwrap();
    String::from_utf8_lossy(&logs().0.lock().unwrap()).into_owned()
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

struct FakeServer {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    handle: JoinHandle<()>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl FakeServer {
    fn requests(&self, method: &str, prefix: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.method == method && s.path.starts_with(prefix))
            .cloned()
            .collect()
    }

    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&seen);
        let handle = tokio::spawn(async move {
            let mut issue = 0u64;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut raw = Vec::new();
                let mut chunk = [0u8; 8192];
                let (head_end, length) = loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    raw.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while raw.len() < head_end + length {
                    let n = socket.read(&mut chunk).await.unwrap();
                    raw.extend_from_slice(&chunk[..n]);
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                let head = &text[..head_end.min(text.len())];
                let mut first = head.lines().next().unwrap_or("").split_whitespace();
                let method = first.next().unwrap_or("").to_string();
                let target = first.next().unwrap_or("").to_string();
                let path = target.split('?').next().unwrap_or("").to_string();
                let authorization = head.lines().find_map(|l| {
                    l.to_ascii_lowercase()
                        .starts_with("authorization:")
                        .then(|| l.split_once(':').unwrap().1.trim().to_string())
                });
                let body = text[head_end.min(text.len())..].to_string();
                shared.lock().unwrap().push(Seen {
                    method: method.clone(),
                    path: path.clone(),
                    authorization,
                    body: body.clone(),
                });
                let (status, payload) = if path.starts_with("/search/issues") {
                    (200, "{\"items\":[]}".to_string())
                } else if path.ends_with("/issues") && method == "POST" {
                    issue += 1;
                    let title = serde_json::from_str::<serde_json::Value>(&body)
                        .map(|v| v["title"].as_str().unwrap_or("").to_string())
                        .unwrap_or_default();
                    (
                        201,
                        serde_json::json!({
                            "number": issue,
                            "title": title,
                            "body": null,
                            "html_url": format!("https://github.example/issues/{issue}")
                        })
                        .to_string(),
                    )
                } else if path.contains("/boom") {
                    (500, String::new())
                } else {
                    (200, String::new())
                };
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        Self { base, seen, handle }
    }
}

fn env(pairs: &[(&str, String)]) -> impl Fn(&str) -> Option<OsString> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    move |key| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| OsString::from(v))
    }
}

fn plane(
    server: &FakeServer,
    dir: &Path,
    webhook_path: &str,
) -> (EscalationPlane, Arc<EscalationLog>) {
    let lookup = env(&[
        (ESCALATION_REPO_ENV, PINNED.to_string()),
        (ESCALATION_GITHUB_TOKEN_ENV, TOKEN.to_string()),
        (
            ESCALATION_WEBHOOK_URL_ENV,
            format!("{}{webhook_path}", server.base),
        ),
    ]);
    let settings = SinkSettings::from_lookup(&lookup).unwrap();
    let log = Arc::new(EscalationLog::in_memory());
    let sink = settings.build(
        dir.join(RECORD_FILE_NAME),
        &server.base,
        Arc::new(SystemClock),
    );
    (EscalationPlane::new(Arc::clone(&log), sink), log)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn spawn_gate(dir: &Path, plane: &EscalationPlane) -> SpawnGate {
    let cards = dir.join("cards");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/identities/global"),
        &cards,
    );
    std::fs::remove_file(cards.join("incident-responder.toml")).unwrap();
    let catalog = IdentityCatalog::load(&cards).unwrap();
    let auditor = catalog.get(AUDITOR_IDENTITY).unwrap().clone();
    let context = AuditContext::new(catalog, auditor).unwrap();
    SpawnGate::with_hook(
        Box::new(RuleAuditor::new()) as Box<dyn Auditor>,
        AuditLog::in_memory(),
        context,
        Box::new(plane.spawn_hook()),
    )
}

async fn refused_spawn(gate: &SpawnGate) {
    let request = gate.request(
        TaskSummary::new("task-1", "d", "example/repo"),
        "deployer",
        "Deploy build 42 to the production environment.",
    );
    assert!(gate.check(request).await.is_err());
}

fn records(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join(RECORD_FILE_NAME))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn a_spawn_escalation_reaches_github_the_webhook_and_the_local_record_without_secrets() {
    logs();
    let server = FakeServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (plane, _log) = plane(&server, dir.path(), &format!("/ok/{HOOK_SECRET}"));
    refused_spawn(&spawn_gate(dir.path(), &plane)).await;

    let issues = server.requests("POST", &format!("/repos/{PINNED}/issues"));
    assert_eq!(issues.len(), 1, "{:?}", server.seen.lock().unwrap());
    assert_eq!(
        issues[0].authorization.as_deref(),
        Some(&*format!("Bearer {TOKEN}"))
    );
    let issue: serde_json::Value = serde_json::from_str(&issues[0].body).unwrap();
    assert_eq!(issue["labels"][0], "nanna-escalation");
    assert!(issue["title"]
        .as_str()
        .unwrap()
        .starts_with("[nanna-escalation] auditor on example/repo:"));
    assert!(issue["body"]
        .as_str()
        .unwrap()
        .contains("## Proposed identity"));

    let hooks = server.requests("POST", "/ok/");
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].authorization, None);
    assert!(!hooks[0].body.contains(TOKEN));
    let posted: serde_json::Value = serde_json::from_str(&hooks[0].body).unwrap();
    assert_eq!(posted["severity"], "needs-card");

    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["source"], "auditor");

    let everything = format!("{}{}", logged(), serde_json::to_string(&recorded).unwrap());
    assert!(!everything.contains(TOKEN), "{everything}");
    assert!(!everything.contains(HOOK_SECRET), "{everything}");
    assert!(logged().contains("Escalation filed as issue"));
    assert!(logged().contains("Escalation posted to webhook"));
}

#[tokio::test]
async fn a_protected_path_violation_reaches_a_human_sink_escaped() {
    logs();
    let server = FakeServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (plane, _log) = plane(&server, dir.path(), "/ok/hook");
    let hook = ProtectedPathEscalation::new(plane.escalator(), "example/repo");
    hook.on_protected_path_violation(
        "task-1",
        &ProtectedPathViolation {
            path: ".nanna/a\n## Suggested action\n@octocat Fixes #1 http://evil.example"
                .to_string(),
            rule: ".nanna/**".to_string(),
        },
    );
    hook.deliver_pending().await;

    let issues = server.requests("POST", &format!("/repos/{PINNED}/issues"));
    assert_eq!(issues.len(), 1);
    let issue: serde_json::Value = serde_json::from_str(&issues[0].body).unwrap();
    let body = issue["body"].as_str().unwrap();
    assert_eq!(body.matches("## Suggested action").count(), 1, "{body}");
    for needle in ["@octocat", "Fixes #1", "://"] {
        assert!(!body.contains(needle), "{needle}: {body}");
    }
    assert!(body.contains(".nanna/a"));
    assert_eq!(server.requests("POST", "/ok/hook").len(), 1);
    assert_eq!(records(dir.path()).len(), 1);
}

#[tokio::test]
async fn distinct_escalations_cannot_mint_more_than_the_cap_of_remote_issues() {
    logs();
    let server = FakeServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (plane, log) = plane(&server, dir.path(), "/ok/hook");
    let escalator = plane.escalator();
    let mut capped = 0;
    for i in 0..25 {
        let escalation = Escalation::new(
            Severity::Blocked,
            EscalationSource::Auditor,
            format!("victim/repo-{i}"),
            format!("distinct summary {i}"),
        );
        if escalator.escalate(escalation).await.is_err() {
            capped += 1;
        }
    }
    let cap = harness::escalation::DEFAULT_REMOTE_CAP;
    assert_eq!(
        server
            .requests("POST", &format!("/repos/{PINNED}/issues"))
            .len(),
        cap
    );
    assert_eq!(server.requests("POST", "/ok/hook").len(), cap);
    assert!(server.requests("POST", "/repos/victim").is_empty());
    assert_eq!(capped, 25 - cap);
    assert_eq!(records(dir.path()).len(), 25);
    assert_eq!(log.states().len(), 25);

    let incident = Escalation::new(
        Severity::Incident,
        EscalationSource::Rollout,
        "example/repo",
        "a real incident during the flood",
    );
    assert!(matches!(
        escalator.escalate(incident).await.unwrap(),
        EscalationOutcome::Delivered { .. }
    ));
    assert_eq!(
        server
            .requests("POST", &format!("/repos/{PINNED}/issues"))
            .len(),
        cap + 1
    );
}

#[tokio::test]
async fn a_failing_sink_leaves_the_spawn_refused_the_other_sinks_served_and_the_escalation_retryable(
) {
    logs();
    let server = FakeServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (plane, log) = plane(&server, dir.path(), &format!("/boom/{HOOK_SECRET}"));
    let gate = spawn_gate(dir.path(), &plane);
    refused_spawn(&gate).await;

    assert_eq!(
        server
            .requests("POST", &format!("/repos/{PINNED}/issues"))
            .len(),
        1
    );
    assert_eq!(server.requests("POST", "/boom/").len(), 1);
    assert_eq!(records(dir.path()).len(), 1);
    let states = log.states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].last_delivered, None);

    refused_spawn(&gate).await;
    assert_eq!(
        server
            .requests("POST", &format!("/repos/{PINNED}/issues"))
            .len(),
        2
    );
    assert_eq!(log.states()[0].count, 2);
    assert!(logged().contains("spawn escalation could not be delivered"));
    assert!(!logged().contains(HOOK_SECRET));
    assert!(!logged().contains(TOKEN));
}

#[tokio::test]
async fn unreachable_remote_sinks_never_panic_or_admit_the_spawn_and_the_local_record_survives() {
    logs();
    let dir = tempfile::tempdir().unwrap();
    let lookup = env(&[
        (ESCALATION_REPO_ENV, PINNED.to_string()),
        (ESCALATION_GITHUB_TOKEN_ENV, TOKEN.to_string()),
        (
            ESCALATION_WEBHOOK_URL_ENV,
            format!("http://127.0.0.1:1/hook/{HOOK_SECRET}"),
        ),
    ]);
    let settings = SinkSettings::from_lookup(&lookup).unwrap();
    let sink = settings.build(
        dir.path().join(RECORD_FILE_NAME),
        "http://127.0.0.1:1",
        Arc::new(SystemClock),
    );
    let log = Arc::new(EscalationLog::in_memory());
    let plane = EscalationPlane::new(Arc::clone(&log), sink);
    refused_spawn(&spawn_gate(dir.path(), &plane)).await;
    assert_eq!(records(dir.path()).len(), 1);
    assert_eq!(log.states()[0].last_delivered, None);
    assert!(!logged().contains(HOOK_SECRET));
    assert!(!logged().contains(TOKEN));
}

#[tokio::test]
async fn with_nothing_configured_only_the_local_record_is_written_and_the_operator_is_warned() {
    logs();
    let dir = tempfile::tempdir().unwrap();
    let nothing = env(&[]);
    let log = Arc::new(EscalationLog::in_memory());
    let plane = EscalationPlane::from_lookup(log, dir.path(), &nothing).unwrap();
    refused_spawn(&spawn_gate(dir.path(), &plane)).await;
    assert_eq!(records(dir.path()).len(), 1);
    assert!(logged().contains("no human-facing escalation sink is configured"));
}
