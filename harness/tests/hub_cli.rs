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
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

static FORKS: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    FORKS.lock().unwrap_or_else(PoisonError::into_inner)
}

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
        let _forks = serial();
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
    let _forks = serial();
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

const UNIFORM: &str = "no quarantined message with that id";

fn recipient(n: u128) -> SessionRecord {
    session(&format!("to-{n}"), "coder", RootId::mcp(uuid_of(n)))
}

fn message_of(hub: &Hub, n: u128) -> StoredMessage {
    let _forks = serial();
    let lock = HubLock::acquire(hub.dir.path()).unwrap();
    let store = JsonlMessageStore::open(&hub.dir.path().join("messages.jsonl"), &lock).unwrap();
    store
        .get(MessageId::from_uuid(uuid_of(n)))
        .unwrap()
        .unwrap()
}

fn stays_quarantined(hub: &Hub, n: u128) {
    let found = message_of(hub, n);
    assert_eq!(found.state, DeliveryState::Quarantined);
    assert!(found.released.is_none());
}

#[test]
fn release_and_drop_without_a_terminal_are_refused_and_the_message_stays_quarantined() {
    let hub = Hub::new();
    hub.seed(vec![held(1, "coder", TOKEN, TOKEN)], vec![recipient(1)]);
    let id = uuid_of(1).to_string();
    for verb in ["release", "drop"] {
        let output = hub.run(&["quarantine", verb, &id]);
        assert_eq!(output.status.code(), Some(1), "{verb}");
        assert!(
            err(&output).contains("operator commands need a terminal"),
            "{}",
            err(&output)
        );
        assert!(out(&output).is_empty());
        assert!(!err(&output).contains(TOKEN));
    }
    stays_quarantined(&hub, 1);
}

#[test]
fn release_and_drop_under_an_agent_session_marker_are_refused_and_the_message_stays_quarantined() {
    let hub = Hub::new();
    hub.seed(vec![held(1, "coder", TOKEN, TOKEN)], vec![recipient(1)]);
    let id = uuid_of(1).to_string();
    for verb in ["release", "drop"] {
        let output = run_at(&hub.queue(), &["quarantine", verb, &id], Some("task-1"));
        assert_eq!(output.status.code(), Some(1), "{verb}");
        assert!(
            err(&output).contains("refused inside an agent session"),
            "{}",
            err(&output)
        );
        assert!(out(&output).is_empty());
    }
    stays_quarantined(&hub, 1);
}

#[test]
fn release_and_drop_need_an_id_argument() {
    let hub = Hub::new();
    for verb in ["release", "drop"] {
        let output = hub.run(&["quarantine", verb]);
        assert_eq!(output.status.code(), Some(2), "{verb}");
        assert!(err(&output).contains("<ID>"), "{}", err(&output));
    }
}

#[cfg(unix)]
mod terminal {
    use super::*;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::Stdio;

    fn pty() -> (OwnedFd, OwnedFd) {
        let mut master = 0;
        let mut slave = 0;
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(rc, 0, "openpty failed");
        unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
    }

    struct Ran {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    }

    fn on_a_terminal(queue: &Path, args: &[&str]) -> Ran {
        let (master, slave) = pty();
        let stdin = File::from(slave.try_clone().unwrap());
        let stdout = File::from(slave);
        let mut command = Command::new(env!("CARGO_BIN_EXE_nanna"));
        command.arg("hub").args(args).arg("--queue-path").arg(queue);
        for name in [
            AGENT_SESSION_ENV,
            "NANNA_SESSIONS_PATH",
            "NANNA_MESSAGES_PATH",
            "NANNA_QUEUE_PATH",
            "RUST_LOG",
        ] {
            command.env_remove(name);
        }
        command.stdin(Stdio::from(stdin));
        command.stdout(Stdio::from(stdout));
        command.stderr(Stdio::piped());
        let forks = serial();
        let output = command.output().unwrap();
        drop(forks);
        drop(command);
        let mut bytes = Vec::new();
        let _ = File::from(master).read_to_end(&mut bytes);
        Ran {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&bytes).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn resolve(hub: &Hub, verb: &str, id: &str) -> Ran {
        on_a_terminal(&hub.queue(), &["quarantine", verb, id])
    }

    #[test]
    fn release_on_a_terminal_moves_the_message_to_pending_with_the_marker_and_prints_ids_only() {
        let hub = Hub::new();
        hub.seed(
            vec![held(1, "coder", "subject-x", TOKEN)],
            vec![recipient(1)],
        );
        let id = uuid_of(1).to_string();
        let ran = resolve(&hub, "release", &id);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(ran.stdout.trim(), format!("released {id}: pending"));
        assert!(!ran.stdout.contains(TOKEN) && !ran.stderr.contains(TOKEN));
        let found = message_of(&hub, 1);
        assert_eq!(found.state, DeliveryState::Pending);
        assert!(found.released.is_some());
        assert_eq!(found.body, TOKEN);
    }

    #[test]
    fn drop_on_a_terminal_expires_the_message_without_a_marker() {
        let hub = Hub::new();
        hub.seed(vec![held(1, "coder", "s", TOKEN)], vec![]);
        let id = uuid_of(1).to_string();
        let ran = resolve(&hub, "drop", &id);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(ran.stdout.trim(), format!("dropped {id}: expired"));
        let found = message_of(&hub, 1);
        assert_eq!(found.state, DeliveryState::Expired);
        assert!(found.released.is_none());
    }

    #[test]
    fn json_prints_the_id_the_action_and_the_state() {
        let hub = Hub::new();
        hub.seed(vec![held(1, "coder", "s", "b")], vec![recipient(1)]);
        let id = uuid_of(1).to_string();
        let ran = on_a_terminal(&hub.queue(), &["quarantine", "release", &id, "--json"]);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        let parsed: serde_json::Value = serde_json::from_str(ran.stdout.trim()).unwrap();
        let expected = serde_json::json!({"id": id, "action": "released", "state": "pending"});
        assert_eq!(parsed, expected);
    }

    #[test]
    fn an_unparsable_unknown_and_non_quarantined_id_give_the_same_error_and_change_nothing() {
        let hub = Hub::new();
        let mut done = held(2, "coder", "s", "b");
        done.state = DeliveryState::Expired;
        hub.seed(
            vec![held(1, "coder", "s", "b"), done],
            vec![recipient(1), recipient(2)],
        );
        for verb in ["release", "drop"] {
            for id in [
                "not-an-id".to_string(),
                uuid_of(404).to_string(),
                uuid_of(2).to_string(),
            ] {
                let ran = resolve(&hub, verb, &id);
                assert_eq!(ran.code, Some(1), "{verb} {id}");
                assert_eq!(ran.stderr.trim(), format!("Error: {UNIFORM}"));
                assert!(ran.stdout.is_empty());
            }
        }
        stays_quarantined(&hub, 1);
        assert_eq!(message_of(&hub, 2).state, DeliveryState::Expired);
    }

    #[test]
    fn a_hub_held_by_a_running_process_is_refused_with_the_in_use_message_and_nothing_changes() {
        let hub = Hub::new();
        hub.seed(vec![held(1, "coder", "s", "b")], vec![recipient(1)]);
        let forks = serial();
        let holder = HubLock::acquire(hub.dir.path()).unwrap();
        drop(forks);
        let id = uuid_of(1).to_string();
        for verb in ["release", "drop"] {
            let ran = resolve(&hub, verb, &id);
            assert_eq!(ran.code, Some(1), "{verb}");
            assert!(
                ran.stderr
                    .contains("the hub is in use by a running nanna process"),
                "{}",
                ran.stderr
            );
            assert!(ran.stdout.is_empty());
        }
        drop(holder);
        stays_quarantined(&hub, 1);
    }

    #[test]
    fn a_release_to_an_inactive_recipient_is_refused_and_the_message_stays_quarantined() {
        let hub = Hub::new();
        hub.seed(vec![held(1, "coder", "s", "b")], vec![]);
        let ran = resolve(&hub, "release", &uuid_of(1).to_string());
        assert_eq!(ran.code, Some(1));
        assert!(
            ran.stderr.contains("recipient session is not active"),
            "{}",
            ran.stderr
        );
        stays_quarantined(&hub, 1);
    }

    #[test]
    fn a_missing_hub_directory_is_exit_one_and_is_not_created() {
        let dir = tempfile::tempdir().unwrap();
        let queue = dir.path().join("absent").join("queue.jsonl");
        let ran = on_a_terminal(&queue, &["quarantine", "drop", &uuid_of(1).to_string()]);
        assert_eq!(ran.code, Some(1));
        assert!(
            ran.stderr.contains("no hub state directory at"),
            "{}",
            ran.stderr
        );
        assert!(!dir.path().join("absent").exists());
    }

    #[test]
    fn a_dropped_message_is_never_listed_again_and_a_second_drop_is_the_uniform_error() {
        let hub = Hub::new();
        hub.seed(vec![held(1, "coder", "s", "b")], vec![]);
        let id = uuid_of(1).to_string();
        assert_eq!(resolve(&hub, "drop", &id).code, Some(0));
        let listed = hub.run(&["quarantine", "list"]);
        assert_eq!(out(&listed), "no quarantined messages\n");
        let again = resolve(&hub, "drop", &id);
        assert_eq!(again.code, Some(1));
        assert_eq!(again.stderr.trim(), format!("Error: {UNIFORM}"));
    }
}
