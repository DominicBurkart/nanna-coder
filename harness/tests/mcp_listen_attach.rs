#![cfg(unix)]

use chrono::Utc;
use harness::result_store::ResultStore;
use harness::task::{Task, TaskId, TaskStatus};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Daemon {
    _dir: Option<tempfile::TempDir>,
    root: PathBuf,
    socket: PathBuf,
    child: Reaper,
}

fn start_in(root: &Path, keep: Option<tempfile::TempDir>) -> Daemon {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/identities/global");
    let config = root.join("config");
    if !config.join("agents").exists() {
        copy_dir(&fixtures, &config.join("agents"));
    }
    let socket = root.join("run").join("mcp.sock");
    let state = root.join("state");
    let child = Command::new(env!("CARGO_BIN_EXE_nanna"))
        .arg("mcp-serve")
        .arg("--listen")
        .arg(&socket)
        .env("NANNA_QUEUE_PATH", state.join("queue.jsonl"))
        .env("NANNA_STATE_DIR", &state)
        .env("NANNA_MODEL", "qwen3:0.6b")
        .env("NANNA_CONFIG_DIR", &config)
        .env("HOME", root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let daemon = Daemon {
        _dir: keep,
        root: root.to_path_buf(),
        socket,
        child: Reaper(child),
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline
        && std::os::unix::net::UnixStream::connect(&daemon.socket).is_err()
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        std::os::unix::net::UnixStream::connect(&daemon.socket).is_ok(),
        "the daemon never opened its socket"
    );
    daemon
}

fn start() -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    start_in(&root, Some(dir))
}

fn attach(socket: &Path, tag: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_nanna"))
        .args(["mcp-attach", "--client", tag, "--socket"])
        .arg(socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn once(socket: &Path, request: &str) -> (std::process::ExitStatus, String, String) {
    let mut child = attach(socket, "test");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{request}\n").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status,
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn wait_exit(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline && child.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(50));
    }
    child.try_wait().unwrap()
}

const PING: &str = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
const LIST: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tasks/list"}"#;

fn record(id: &str, status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
        id: TaskId(id.to_string()),
        description: "from before".to_string(),
        repo_path: PathBuf::from("/repos/r"),
        branch: "HEAD".to_string(),
        model: "m".to_string(),
        status,
        created_at: now,
        last_updated_at: now,
        ttl_ms: None,
        not_before: None,
        identity_hint: None,
        origin: None,
        lineage: None,
    }
}

#[test]
fn attach_relays_a_reply_and_exits_when_stdin_closes() {
    let daemon = start();
    let (status, stdout, _) = once(&daemon.socket, PING);
    assert!(status.success());
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "stdout must hold protocol bytes only: {stdout:?}"
    );
    let reply: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(reply["id"], 1);
}

#[test]
fn two_attached_clients_share_the_daemon_and_one_leaving_does_not_stop_it() {
    let daemon = start();
    let mut staying = attach(&daemon.socket, "stays");
    let mut stdin = staying.stdin.take().unwrap();
    let mut replies = BufReader::new(staying.stdout.take().unwrap());
    writeln!(stdin, "{PING}").unwrap();
    let mut line = String::new();
    replies.read_line(&mut line).unwrap();
    assert!(line.contains("\"id\":1"));

    let (status, stdout, _) = once(&daemon.socket, LIST);
    assert!(status.success());
    assert!(stdout.contains("\"tasks\""));

    writeln!(stdin, "{LIST}").unwrap();
    line.clear();
    replies.read_line(&mut line).unwrap();
    assert!(line.contains("\"tasks\""));
    drop(stdin);
    drop(replies);
    assert!(wait_exit(&mut staying, Duration::from_secs(10))
        .unwrap()
        .success());

    let (status, stdout, _) = once(&daemon.socket, PING);
    assert!(status.success() && stdout.contains("\"id\":1"));
}

#[test]
fn attach_to_a_missing_daemon_fails_with_a_message_and_empty_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none.sock");
    let (status, stdout, stderr) = once(&missing, PING);
    assert!(!status.success());
    assert!(stdout.is_empty());
    assert!(stderr.contains("none.sock"), "{stderr}");
}

#[test]
fn attach_exits_non_zero_when_the_daemon_dies_mid_session() {
    let mut daemon = start();
    let mut session = attach(&daemon.socket, "doomed");
    let mut stdin = session.stdin.take().unwrap();
    let mut replies = BufReader::new(session.stdout.take().unwrap());
    writeln!(stdin, "{PING}").unwrap();
    let mut line = String::new();
    replies.read_line(&mut line).unwrap();
    assert_eq!(
        unsafe { libc::kill(daemon.child.0.id() as i32, libc::SIGKILL) },
        0
    );
    let _ = daemon.child.0.wait();
    let status = wait_exit(&mut session, Duration::from_secs(10)).expect("attach did not exit");
    assert!(!status.success());
    drop(stdin);
}

#[test]
fn sigterm_stops_the_daemon_and_removes_its_socket() {
    let mut daemon = start();
    assert!(daemon.socket.exists());
    assert_eq!(
        unsafe { libc::kill(daemon.child.0.id() as i32, libc::SIGTERM) },
        0
    );
    let status =
        wait_exit(&mut daemon.child.0, Duration::from_secs(15)).expect("daemon kept running");
    assert!(status.success(), "{status:?}");
    assert!(!daemon.socket.exists());
}

#[test]
fn results_written_before_a_restart_are_served_and_running_ones_fail_as_daemon_restarted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = ResultStore::open(&root.join("state"), Duration::from_secs(86400)).unwrap();
    store
        .persist(&record(
            "done-1",
            TaskStatus::Cancelled {
                finished_at: Utc::now(),
                iterations_completed: 2,
            },
        ))
        .unwrap();
    store
        .persist(&record(
            "mid-flight",
            TaskStatus::Running {
                started_at: Utc::now(),
                iterations: 3,
            },
        ))
        .unwrap();
    let daemon = start_in(&root, Some(dir));
    let (status, stdout, _) = once(&daemon.socket, LIST);
    assert!(status.success());
    let reply: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    let tasks = reply["result"]["tasks"].as_array().unwrap();
    let by_id = |id: &str| tasks.iter().find(|t| t["taskId"] == id).unwrap().clone();
    assert_eq!(by_id("done-1")["status"], "cancelled");
    assert_eq!(by_id("mid-flight")["status"], "failed");
    let on_disk = store.load(&TaskId("mid-flight".into())).unwrap();
    match on_disk.status {
        TaskStatus::Failed { diagnostics, .. } => {
            assert_eq!(diagnostics.error_type, "DaemonRestarted")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(daemon.root.join("state/results").is_dir());
}
