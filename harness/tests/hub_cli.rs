use chrono::Utc;
use harness::auditor::ReasonCode;
use harness::effects::EffectClass;
use harness::escalation::AGENT_SESSION_ENV;
use harness::hub::lineage::{Lineage, RootId, SessionId};
use harness::hub::message::{
    DeliveryState, MessageId, MessageKind, Seq, StoredMessage, VerdictRef,
};
use harness::hub::session::{ReadScope, SessionKind, SessionRecord, SessionStatus};
use harness::hub::store::{
    HubLock, JsonlMessageStore, JsonlSessionStore, MessageStore, SessionStore,
};
use harness::identity::DevLoop;
use harness::task::TaskId;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

const TOKEN: &str = "PLANTED-TOKEN-7c1e";
const LOCK_FILE: &str = "hub.lock";

fn uuid_of(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(n)
}

fn held(n: u128, identity: &str, subject: &str, body: &str) -> StoredMessage {
    StoredMessage {
        id: MessageId::from_uuid(uuid_of(n)),
        seq: Seq::new(0),
        from: SessionId::from_task(TaskId(format!("from-{n}"))),
        from_identity: identity.into(),
        to: SessionId::from_task(TaskId(format!("to-{n}"))),
        root: RootId::mcp(uuid_of(n)),
        kind: MessageKind::Question,
        subject: subject.into(),
        body: body.into(),
        in_reply_to: None,
        thread_depth: 0,
        outbound_verdict: VerdictRef {
            log_id: "v".into(),
            content_hash: 7,
        },
        state: DeliveryState::Quarantined,
        sent_at: Utc::now(),
        held: vec![ReasonCode::PromptInjection],
        released: None,
    }
}

fn session(task: &str, identity: &str, root: RootId) -> SessionRecord {
    let now = Utc::now();
    SessionRecord {
        id: SessionId::from_task(TaskId(task.into())),
        lineage: Lineage::root_child(root),
        kind: SessionKind::Agent,
        identity: identity.into(),
        role: "r".into(),
        dev_loop: DevLoop::Inner,
        max_effect: EffectClass::Workspace,
        tools: vec![],
        write_paths: vec![],
        read_paths: ReadScope::Unrestricted,
        subtask: "s".into(),
        note: None,
        result_summary: None,
        touched_paths: vec![],
        status: SessionStatus::Running,
        created_at: now,
        last_activity: now,
        ended_at: None,
    }
}

struct Hub {
    dir: tempfile::TempDir,
}

impl Hub {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn queue(&self) -> PathBuf {
        self.dir.path().join("queue.jsonl")
    }

    fn seed(&self, messages: Vec<StoredMessage>, sessions: Vec<SessionRecord>) {
        let lock = HubLock::acquire(self.dir.path()).unwrap();
        let store = JsonlMessageStore::open(&self.dir.path().join("messages.jsonl"), &lock);
        let store = store.unwrap();
        for message in messages {
            store.append(message).unwrap();
        }
        let table = JsonlSessionStore::open(&self.dir.path().join("sessions.jsonl"), &lock);
        let table = table.unwrap();
        for record in sessions {
            table.upsert(record).unwrap();
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        run_at(&self.queue(), args, None)
    }
}

fn run_at(queue: &Path, args: &[&str], agent: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanna"));
    command.arg("hub").args(args).arg("--queue-path").arg(queue);
    command.env_remove(AGENT_SESSION_ENV);
    for name in [
        "NANNA_SESSIONS_PATH",
        "NANNA_MESSAGES_PATH",
        "NANNA_QUEUE_PATH",
        "RUST_LOG",
    ] {
        command.env_remove(name);
    }
    if let Some(marker) = agent {
        command.env(AGENT_SESSION_ENV, marker);
    }
    command.stdin(std::process::Stdio::null());
    command.output().unwrap()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

type Tree = BTreeMap<String, (Vec<u8>, SystemTime)>;

fn age(dir: &Path) {
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    for entry in fs::read_dir(dir).unwrap() {
        let file = fs::File::options().write(true).open(entry.unwrap().path());
        file.unwrap().set_modified(old).unwrap();
    }
}

fn snapshot(dir: &Path) -> Tree {
    let mut tree = Tree::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name != LOCK_FILE {
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            tree.insert(name, (fs::read(&path).unwrap(), modified));
        }
    }
    tree
}

fn printable(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.chars().all(|c| c == '\n' || !c.is_control())
}

#[test]
fn list_prints_ids_codes_and_sizes_and_never_the_text() {
    let hub = Hub::new();
    hub.seed(vec![held(1, "coder", TOKEN, TOKEN)], vec![]);
    let output = hub.run(&["quarantine", "list"]);
    assert!(output.status.success(), "{}", err(&output));
    let text = out(&output);
    assert!(text.starts_with(&uuid_of(1).to_string()), "{text}");
    assert!(text.contains("held=prompt_injection"), "{text}");
    assert!(!text.contains(TOKEN) && !err(&output).contains(TOKEN));
}

#[test]
fn list_over_an_empty_hub_says_so_and_creates_no_file() {
    let hub = Hub::new();
    let output = hub.run(&["quarantine", "list"]);
    assert!(output.status.success());
    assert_eq!(out(&output), "no quarantined messages\n");
    assert!(fs::read_dir(hub.dir.path()).unwrap().next().is_none());
}

#[test]
fn a_missing_hub_directory_is_exit_one_with_a_clear_message_and_no_panic() {
    let dir = tempfile::tempdir().unwrap();
    let queue = dir.path().join("absent").join("queue.jsonl");
    let shown = dir.path().join("absent").display().to_string();
    for args in [
        vec!["quarantine", "list"],
        vec!["sessions", "--root", "plan-x"],
    ] {
        let output = run_at(&queue, &args, None);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            err(&output).contains(&format!("no hub state directory at {shown}")),
            "{}",
            err(&output)
        );
        assert!(!err(&output).contains("panicked"));
    }
    assert!(!dir.path().join("absent").exists());
}

#[test]
fn show_without_a_terminal_is_refused_and_prints_no_text() {
    let hub = Hub::new();
    hub.seed(vec![held(1, "coder", TOKEN, TOKEN)], vec![]);
    let output = hub.run(&["quarantine", "show", &uuid_of(1).to_string()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        err(&output).contains("operator commands need a terminal"),
        "{}",
        err(&output)
    );
    assert!(!out(&output).contains(TOKEN) && !err(&output).contains(TOKEN));
    assert!(out(&output).is_empty());
}

#[test]
fn show_under_an_agent_session_marker_is_refused_with_the_agent_message() {
    let hub = Hub::new();
    hub.seed(vec![held(1, "coder", TOKEN, TOKEN)], vec![]);
    let id = uuid_of(1).to_string();
    let output = run_at(&hub.queue(), &["quarantine", "show", &id], Some("task-1"));
    assert_eq!(output.status.code(), Some(1));
    assert!(
        err(&output).contains("refused inside an agent session"),
        "{}",
        err(&output)
    );
    assert!(!out(&output).contains(TOKEN) && !err(&output).contains(TOKEN));
}

#[test]
fn list_and_sessions_run_under_an_agent_marker_because_they_carry_no_message_text() {
    let hub = Hub::new();
    hub.seed(vec![held(1, "coder", TOKEN, TOKEN)], vec![]);
    let output = run_at(&hub.queue(), &["quarantine", "list"], Some("task-1"));
    assert!(output.status.success());
    assert!(!out(&output).contains(TOKEN));
}

#[test]
fn sessions_lists_one_root_in_text_and_json() {
    let hub = Hub::new();
    let one = RootId::mcp(uuid_of(1));
    let two = RootId::mcp(uuid_of(2));
    hub.seed(
        vec![],
        vec![session("m1", "coder", one), session("m2", "reviewer", two)],
    );
    let output = hub.run(&["sessions", "--root", &format!("mcp-{}", uuid_of(1))]);
    assert!(output.status.success(), "{}", err(&output));
    let text = out(&output);
    assert!(
        text.contains("coder") && !text.contains("reviewer"),
        "{text}"
    );
    let by_session = hub.run(&["sessions", "--root", "m2", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&out(&by_session)).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 1);
    assert_eq!(parsed[0]["status"], "running");
}

#[test]
fn sessions_exit_codes_empty_root_zero_unknown_one_missing_flag_two() {
    let hub = Hub::new();
    hub.seed(
        vec![],
        vec![session("m1", "coder", RootId::mcp(uuid_of(1)))],
    );
    let empty = hub.run(&["sessions", "--root", "plan-nothing"]);
    assert_eq!(empty.status.code(), Some(0));
    assert_eq!(out(&empty), "no sessions in this root\n");
    let unknown = hub.run(&["sessions", "--root", "no-such-task"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(err(&unknown).contains("no sessions are known for that root id"));
    let usage = hub.run(&["sessions"]);
    assert_eq!(usage.status.code(), Some(2));
}

#[test]
fn hostile_identities_never_put_a_raw_control_character_on_stdout() {
    let hub = Hub::new();
    let hostile = "\u{1b}[31mred\u{1b}]0;title\u{7}\u{9b}2J\u{202e}\r\u{8}x";
    let root = RootId::mcp(uuid_of(1));
    hub.seed(
        vec![held(1, hostile, "s", "b")],
        vec![session(hostile, hostile, root)],
    );
    let list = hub.run(&["quarantine", "list"]);
    let sessions = hub.run(&[
        "sessions",
        "--root",
        &format!("mcp-{}", uuid_of(1)),
        "--json",
    ]);
    assert!(list.status.success() && sessions.status.success());
    assert!(printable(&list.stdout), "{:?}", list.stdout);
    assert!(out(&list).contains("UNTRUSTED_AGENT_TEXT"));
    assert!(printable(&sessions.stdout), "{:?}", sessions.stdout);
}

#[test]
fn reads_change_no_byte_mtime_or_entry_while_another_handle_holds_the_lock_and_the_journal() {
    let hub = Hub::new();
    let lock = HubLock::acquire(hub.dir.path()).unwrap();
    let store = JsonlMessageStore::open(&hub.dir.path().join("messages.jsonl"), &lock).unwrap();
    let table = JsonlSessionStore::open(&hub.dir.path().join("sessions.jsonl"), &lock).unwrap();
    let gone = held(2, "coder", "s", "b");
    store.append(held(1, "coder", "s", "b")).unwrap();
    store.append(gone.clone()).unwrap();
    let (from, to) = (DeliveryState::Quarantined, DeliveryState::Expired);
    store.transition(gone.id, from, to, None).unwrap();
    table
        .upsert(session("m1", "coder", RootId::mcp(uuid_of(1))))
        .unwrap();
    age(hub.dir.path());
    let before = snapshot(hub.dir.path());
    assert_eq!(before.len(), 2);
    let list = hub.run(&["quarantine", "list"]);
    let sessions = hub.run(&["sessions", "--root", "m1"]);
    assert!(
        list.status.success() && sessions.status.success(),
        "{}{}",
        err(&list),
        err(&sessions)
    );
    assert!(out(&list).contains(&uuid_of(1).to_string()));
    assert!(!out(&list).contains(&uuid_of(2).to_string()));
    assert_eq!(snapshot(hub.dir.path()), before);
    drop((store, table, lock));
}

#[test]
fn a_half_written_last_line_from_a_live_writer_is_tolerated_and_left_alone() {
    let hub = Hub::new();
    hub.seed(vec![held(1, "coder", "s", "b")], vec![]);
    let path = hub.dir.path().join("messages.jsonl");
    let mut file = fs::File::options().append(true).open(&path).unwrap();
    std::io::Write::write_all(&mut file, b"{\"op\":\"append\",\"mess").unwrap();
    age(hub.dir.path());
    let before = snapshot(hub.dir.path());
    let output = hub.run(&["quarantine", "list"]);
    assert!(output.status.success(), "{}", err(&output));
    assert!(out(&output).contains(&uuid_of(1).to_string()));
    assert_eq!(snapshot(hub.dir.path()), before);
}

#[test]
fn a_corrupt_journal_is_exit_one_not_a_partial_listing() {
    let hub = Hub::new();
    hub.seed(vec![held(1, "coder", "s", "b")], vec![]);
    let path = hub.dir.path().join("messages.jsonl");
    let good = fs::read(&path).unwrap();
    let mut bytes = b"garbage\n".to_vec();
    bytes.extend_from_slice(&good);
    fs::write(&path, bytes).unwrap();
    let output = hub.run(&["quarantine", "list"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        err(&output).contains("corrupt record at line 1"),
        "{}",
        err(&output)
    );
    assert!(out(&output).is_empty());
}
