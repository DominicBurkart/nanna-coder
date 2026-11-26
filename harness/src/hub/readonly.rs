use std::path::Path;
use std::str::FromStr;

use super::lineage::{RootId, SessionId};
use super::message::{MessageId, StoredMessage};
use super::operator::{inline, OperatorError};
use super::runtime::HubPaths;
use super::session::SessionRecord;
use super::store::{ReadOnlyMessageStore, ReadOnlySessionStore};
use crate::task::TaskId;

const EMPTY_SESSIONS: &str = "no sessions in this root";

pub(super) struct ReadOnlyHub {
    paths: HubPaths,
}

fn journal_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn present(path: &Path) -> Result<(), OperatorError> {
    let dir = journal_dir(path);
    if dir.is_dir() {
        return Ok(());
    }
    Err(OperatorError::HubMissing(dir.display().to_string()))
}

impl ReadOnlyHub {
    pub(super) fn open(paths: &HubPaths) -> Self {
        Self {
            paths: paths.clone(),
        }
    }

    fn messages(&self) -> Result<ReadOnlyMessageStore, OperatorError> {
        present(&self.paths.messages)?;
        Ok(ReadOnlyMessageStore::open(&self.paths.messages)?)
    }

    fn sessions_store(&self) -> Result<ReadOnlySessionStore, OperatorError> {
        present(&self.paths.sessions)?;
        Ok(ReadOnlySessionStore::open(&self.paths.sessions)?)
    }

    pub(super) fn quarantined(&self) -> Result<Vec<StoredMessage>, OperatorError> {
        Ok(self.messages()?.quarantined())
    }

    pub(super) fn message(&self, id: MessageId) -> Result<Option<StoredMessage>, OperatorError> {
        Ok(self.messages()?.get(id))
    }

    pub(super) fn sessions(&self, root: &str) -> Result<Vec<SessionLine>, OperatorError> {
        let store = self.sessions_store()?;
        let resolved = resolve_root(root, &store)?;
        let records = store.in_root(&resolved);
        Ok(records.iter().map(line_of).collect())
    }
}

fn backlog_root(rest: &str) -> Option<RootId> {
    let (repo, issue) = rest.split_once('#')?;
    let number = issue.parse::<u64>().ok()?;
    RootId::backlog(repo, number).ok()
}

fn prefixed_root(text: &str) -> Option<RootId> {
    if let Some(rest) = text.strip_prefix("mcp-") {
        return uuid::Uuid::from_str(rest).ok().map(RootId::mcp);
    }
    if let Some(rest) = text.strip_prefix("backlog-") {
        return backlog_root(rest);
    }
    if text.starts_with("plan-") {
        return RootId::plan(text).ok();
    }
    None
}

fn resolve_root(text: &str, store: &ReadOnlySessionStore) -> Result<RootId, OperatorError> {
    if let Some(root) = prefixed_root(text) {
        return Ok(root);
    }
    let id = SessionId::from_task(TaskId(text.to_string()));
    let record = store.get(&id).ok_or(OperatorError::UnknownRoot)?;
    Ok(record.lineage.root().clone())
}

pub(super) struct SessionLine {
    fields: [(&'static str, String); 5],
}

fn line_of(record: &SessionRecord) -> SessionLine {
    let note = record.note.as_ref().map(|note| note.as_str());
    SessionLine {
        fields: [
            ("id", inline(&record.id.to_string())),
            ("identity", inline(&record.identity)),
            ("status", record.status.label().to_string()),
            ("depth", record.lineage.depth().to_string()),
            ("summary", inline(note.unwrap_or_default())),
        ],
    }
}

pub(super) fn render_sessions(lines: &[SessionLine]) -> String {
    let rendered: Vec<String> = lines.iter().map(render_line).collect();
    match rendered.is_empty() {
        true => EMPTY_SESSIONS.to_string(),
        false => rendered.join("\n"),
    }
}

fn render_line(line: &SessionLine) -> String {
    let pair = |(name, value): &(&str, String)| format!("{name}={value}");
    let parts: Vec<String> = line.fields.iter().map(pair).collect();
    parts.join(" ")
}

pub(super) fn sessions_json(lines: &[SessionLine]) -> String {
    let objects: Vec<serde_json::Value> = lines.iter().map(json_line).collect();
    serde_json::Value::from(objects).to_string()
}

fn json_line(line: &SessionLine) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    for (name, value) in &line.fields {
        object.insert((*name).to_string(), value.clone().into());
    }
    object.into()
}

#[cfg(test)]
pub(super) mod seed {
    use super::*;
    use crate::auditor::ReasonCode;
    use crate::effects::EffectClass;
    use crate::hub::lineage::Lineage;
    use crate::hub::message::{DeliveryState, MessageKind, Seq, VerdictRef};
    use crate::hub::session::{ReadScope, SessionKind, SessionStatus};
    use crate::hub::store::{
        HubLock, JsonlMessageStore, JsonlSessionStore, MessageStore, SessionStore,
    };
    use crate::identity::DevLoop;

    pub(in crate::hub) struct Seeded {
        pub dir: tempfile::TempDir,
        pub paths: HubPaths,
    }

    pub(in crate::hub) fn message(n: u128, state: DeliveryState, text: &str) -> StoredMessage {
        StoredMessage {
            id: MessageId::from_uuid(uuid::Uuid::from_u128(n)),
            seq: Seq::new(0),
            from: SessionId::from_task(TaskId(format!("from-{n}"))),
            from_identity: "coder".into(),
            to: SessionId::from_task(TaskId(format!("to-{n}"))),
            root: RootId::mcp(uuid::Uuid::from_u128(n)),
            kind: MessageKind::Question,
            subject: format!("subject {text}"),
            body: format!("body {text}"),
            in_reply_to: None,
            thread_depth: 0,
            outbound_verdict: VerdictRef {
                log_id: "v".into(),
                content_hash: 0xabc,
            },
            state,
            sent_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_default(),
            held: vec![ReasonCode::PromptInjection],
            released: None,
        }
    }

    pub(in crate::hub) fn record(task: &str, identity: &str, root: RootId) -> SessionRecord {
        let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_default();
        SessionRecord {
            id: SessionId::from_task(TaskId(task.to_string())),
            lineage: Lineage::root_child(root),
            kind: SessionKind::Agent,
            identity: identity.to_string(),
            role: "ROLE-TEXT".into(),
            dev_loop: DevLoop::Inner,
            max_effect: EffectClass::Workspace,
            tools: vec![],
            write_paths: vec![],
            read_paths: ReadScope::Unrestricted,
            subtask: "SUBTASK-TEXT".into(),
            note: None,
            result_summary: None,
            touched_paths: vec![],
            status: SessionStatus::Running,
            created_at: now,
            last_activity: now,
            ended_at: None,
        }
    }

    pub(in crate::hub) fn seeded(
        messages: Vec<StoredMessage>,
        sessions: Vec<SessionRecord>,
    ) -> Seeded {
        let dir = tempfile::tempdir().unwrap();
        let paths = HubPaths::beside(&dir.path().join("queue.jsonl"), None, None);
        let lock = HubLock::acquire(dir.path()).unwrap();
        let store = JsonlMessageStore::open(&paths.messages, &lock).unwrap();
        for message in messages {
            store.append(message).unwrap();
        }
        let table = JsonlSessionStore::open(&paths.sessions, &lock).unwrap();
        for session in sessions {
            table.upsert(session).unwrap();
        }
        Seeded { dir, paths }
    }
}

#[cfg(test)]
mod tests {
    use super::seed::{message, record, seeded};
    use super::*;
    use crate::hub::message::DeliveryState;
    use crate::hub::store::{HubLock, JsonlMessageStore, JsonlSessionStore, MessageStore};
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::{Duration, SystemTime};

    type Tree = BTreeMap<String, (Vec<u8>, SystemTime)>;

    fn age(dir: &Path) {
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let file = fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(old).unwrap();
        }
    }

    fn snapshot(dir: &Path) -> Tree {
        let mut tree = Tree::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name == "hub.lock" {
                continue;
            }
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            tree.insert(name, (fs::read(&path).unwrap(), modified));
        }
        tree
    }

    fn names(dir: &Path) -> Vec<String> {
        let entries = fs::read_dir(dir).unwrap();
        let mut found: Vec<String> = entries
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        found.sort();
        found
    }

    fn mcp(n: u128) -> RootId {
        RootId::mcp(uuid::Uuid::from_u128(n))
    }

    fn fenced(text: &str) -> String {
        format!("<<<UNTRUSTED_AGENT_TEXT {text} UNTRUSTED_AGENT_TEXT>>>")
    }

    fn ids(lines: &[SessionLine]) -> Vec<String> {
        lines.iter().map(|l| l.fields[0].1.clone()).collect()
    }

    #[test]
    fn a_read_changes_no_byte_mtime_or_directory_entry_while_a_writer_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HubPaths::beside(&dir.path().join("queue.jsonl"), None, None);
        let lock = HubLock::acquire(dir.path()).unwrap();
        let writer = JsonlMessageStore::open(&paths.messages, &lock).unwrap();
        let sessions = JsonlSessionStore::open(&paths.sessions, &lock).unwrap();
        let kept = message(1, DeliveryState::Quarantined, "kept");
        let gone = message(2, DeliveryState::Quarantined, "gone");
        writer.append(kept).unwrap();
        writer.append(gone.clone()).unwrap();
        let (held, expired) = (DeliveryState::Quarantined, DeliveryState::Expired);
        writer.transition(gone.id, held, expired, None).unwrap();
        crate::hub::store::SessionStore::upsert(&sessions, record("a", "coder", mcp(1))).unwrap();
        age(dir.path());
        let before = snapshot(dir.path());
        let listing = names(dir.path());
        assert!(before.contains_key("messages.jsonl"));
        assert!(before.contains_key("sessions.jsonl"));
        assert_eq!(before.len(), 2);
        let hub = ReadOnlyHub::open(&paths);
        assert_eq!(hub.quarantined().unwrap().len(), 1);
        assert_eq!(
            hub.sessions("mcp-00000000-0000-0000-0000-000000000001")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(snapshot(dir.path()), before);
        assert_eq!(names(dir.path()), listing);
        drop((writer, sessions));
    }

    #[test]
    fn a_missing_journal_in_an_existing_directory_is_an_empty_view_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HubPaths::beside(&dir.path().join("queue.jsonl"), None, None);
        let hub = ReadOnlyHub::open(&paths);
        assert!(hub.quarantined().unwrap().is_empty());
        assert!(hub.message(MessageId::new()).unwrap().is_none());
        assert!(hub.sessions("plan-abc").unwrap().is_empty());
        assert!(names(dir.path()).is_empty());
    }

    #[test]
    fn a_missing_hub_directory_is_a_clear_error_for_every_read() {
        let dir = tempfile::tempdir().unwrap();
        let queue = dir.path().join("absent").join("queue.jsonl");
        let hub = ReadOnlyHub::open(&HubPaths::beside(&queue, None, None));
        let shown = dir.path().join("absent").display().to_string();
        let expected = format!("no hub state directory at {shown}");
        assert_eq!(hub.quarantined().unwrap_err().to_string(), expected);
        assert_eq!(
            hub.message(MessageId::new()).unwrap_err().to_string(),
            expected
        );
        assert_eq!(
            hub.sessions("plan-abc").err().unwrap().to_string(),
            expected
        );
        assert!(!dir.path().join("absent").exists());
    }

    #[test]
    fn a_journal_path_without_a_parent_reads_the_current_directory() {
        assert_eq!(journal_dir(Path::new("messages.jsonl")), Path::new("."));
        assert_eq!(journal_dir(Path::new("/")), Path::new("."));
        assert_eq!(journal_dir(Path::new("a/messages.jsonl")), Path::new("a"));
    }

    #[test]
    fn a_half_written_last_line_is_tolerated_and_left_in_place() {
        let seed = seeded(vec![message(1, DeliveryState::Quarantined, "x")], vec![]);
        let path = seed.paths.messages.clone();
        let mut file = fs::File::options().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, b"{\"op\":\"append\",\"mess").unwrap();
        drop(file);
        age(seed.dir.path());
        let before = snapshot(seed.dir.path());
        let held = ReadOnlyHub::open(&seed.paths).quarantined().unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(snapshot(seed.dir.path()), before);
    }

    #[test]
    fn a_corrupt_line_that_is_not_the_last_is_an_error_not_a_partial_view() {
        let seed = seeded(vec![message(1, DeliveryState::Quarantined, "x")], vec![]);
        let good = fs::read(&seed.paths.messages).unwrap();
        let mut bytes = b"not json\n".to_vec();
        bytes.extend_from_slice(&good);
        fs::write(&seed.paths.messages, bytes).unwrap();
        let result = ReadOnlyHub::open(&seed.paths).quarantined();
        assert_eq!(result.unwrap_err().to_string(), "corrupt record at line 1");
    }

    #[test]
    fn a_journal_that_cannot_be_read_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let paths = HubPaths::beside(&dir.path().join("queue.jsonl"), None, None);
        fs::create_dir(&paths.messages).unwrap();
        let result = ReadOnlyHub::open(&paths).quarantined();
        assert!(matches!(result, Err(OperatorError::Store(_))));
    }

    #[test]
    fn only_quarantined_messages_are_listed_and_get_finds_any_state() {
        let pending = message(1, DeliveryState::Pending, "p");
        let held = message(2, DeliveryState::Quarantined, "q");
        let seed = seeded(vec![pending.clone(), held.clone()], vec![]);
        let hub = ReadOnlyHub::open(&seed.paths);
        let listed: Vec<MessageId> = hub.quarantined().unwrap().iter().map(|m| m.id).collect();
        assert_eq!(listed, vec![held.id]);
        assert_eq!(hub.message(pending.id).unwrap().unwrap().id, pending.id);
    }

    #[test]
    fn the_root_forms_resolve_and_a_session_lists_only_its_own_root() {
        let plan = RootId::plan("plan-0a1b").unwrap();
        let backlog = RootId::backlog("example/repo", 7).unwrap();
        let sessions = vec![
            record("m1", "coder", mcp(1)),
            record("m2", "reviewer", mcp(2)),
            record("p1", "planner", plan),
            record("b1", "triage", backlog),
        ];
        let seed = seeded(vec![], sessions);
        let hub = ReadOnlyHub::open(&seed.paths);
        let one = hub
            .sessions("mcp-00000000-0000-0000-0000-000000000001")
            .unwrap();
        assert_eq!(ids(&one), vec![fenced("m1")]);
        let two = hub.sessions("m2").unwrap();
        assert_eq!(ids(&two), vec![fenced("m2")]);
        assert_eq!(ids(&hub.sessions("plan-0a1b").unwrap()), vec![fenced("p1")]);
        let backlog_lines = hub.sessions("backlog-example/repo#7").unwrap();
        assert_eq!(ids(&backlog_lines), vec![fenced("b1")]);
    }

    #[test]
    fn a_parsable_root_with_no_sessions_is_empty_and_unparsable_or_unknown_ids_are_refused() {
        let seed = seeded(vec![], vec![record("m1", "coder", mcp(1))]);
        let hub = ReadOnlyHub::open(&seed.paths);
        let empty = hub
            .sessions("mcp-00000000-0000-0000-0000-0000000000ff")
            .unwrap();
        assert!(empty.is_empty());
        for bad in [
            "mcp-not-a-uuid",
            "plan-UPPER",
            "backlog-no-issue",
            "backlog-x/y#z",
            "backlog-bad repo/x#1",
            "unknown-task",
            "",
        ] {
            let refused = hub.sessions(bad).err().map(|e| e.to_string());
            assert_eq!(
                refused.as_deref(),
                Some("no sessions are known for that root id"),
                "{bad}"
            );
        }
    }

    #[test]
    fn session_lines_show_identity_status_depth_and_summary_and_never_subtask_or_role() {
        let mut one = record("m1", "coder", mcp(1));
        one.note = Some(crate::hub::summary::Summary::compose(
            "coder", "running", "fixing",
        ));
        let seed = seeded(vec![], vec![one]);
        let lines = ReadOnlyHub::open(&seed.paths).sessions("m1").unwrap();
        let text = render_sessions(&lines);
        assert!(
            text.starts_with(&format!("id={} identity=", fenced("m1"))),
            "{text}"
        );
        assert!(text.contains("status=running depth=1 summary="), "{text}");
        assert!(text.contains("fixing"), "{text}");
        assert!(!text.contains("SUBTASK-TEXT") && !text.contains("ROLE-TEXT"));
        let json: serde_json::Value = serde_json::from_str(&sessions_json(&lines)).unwrap();
        assert_eq!(json[0]["id"], fenced("m1").as_str());
        assert_eq!(json[0]["status"], "running");
        assert_eq!(json[0]["depth"], "1");
        assert_eq!(render_sessions(&[]), "no sessions in this root");
        assert_eq!(sessions_json(&[]), "[]");
    }
}
