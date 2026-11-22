use super::{MessageStore, Retention, SessionStore, StoreError};
use crate::hub::lineage::{Lineage, RootId, SessionId};
use crate::hub::message::{
    DeliveryState, MessageId, MessageKind, Recipient, Seq, StoredMessage, VerdictRef,
};
use crate::hub::session::{SessionKind, SessionRecord, SessionStatus};
use crate::identity::DevLoop;
use crate::task::TaskId;
use chrono::{DateTime, TimeZone, Utc};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub(super) const TOKEN: &str = "TOKEN-9f3c1b7a-secret-body";

pub(super) fn sid(name: &str) -> SessionId {
    SessionId::from_task(TaskId(name.to_string()))
}

pub(super) fn root(n: u128) -> RootId {
    RootId::mcp(Uuid::from_u128(n))
}

pub(super) fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
}

pub(super) fn session(
    id: &str,
    root: &RootId,
    status: SessionStatus,
    created: i64,
    last: i64,
) -> SessionRecord {
    SessionRecord {
        id: sid(id),
        lineage: Lineage::root_child(root.clone()),
        kind: SessionKind::Agent,
        identity: format!("identity-{id}"),
        role: "role".into(),
        dev_loop: DevLoop::Inner,
        max_effect: crate::effects::EffectClass::Workspace,
        tools: vec!["read_file".into()],
        write_paths: vec!["src".into()],
        subtask: "subtask".into(),
        note: None,
        result_summary: None,
        touched_paths: vec![],
        status,
        created_at: at(created),
        last_activity: at(last),
        ended_at: None,
    }
}

pub(super) fn message(
    id: u128,
    from: &str,
    to: &str,
    state: DeliveryState,
    sent: i64,
    body: &str,
) -> StoredMessage {
    StoredMessage {
        id: MessageId::from_uuid(Uuid::from_u128(id)),
        seq: Seq::new(999),
        from: sid(from),
        from_identity: format!("identity-{from}"),
        to: sid(to),
        root: root(1),
        kind: MessageKind::Info,
        subject: "s".into(),
        body: body.into(),
        in_reply_to: None,
        thread_depth: 0,
        outbound_verdict: VerdictRef {
            log_id: "v".into(),
            content_hash: 7,
        },
        state,
        sent_at: at(sent),
    }
}

pub(super) fn mid(n: u128) -> MessageId {
    MessageId::from_uuid(Uuid::from_u128(n))
}

pub(super) fn policy() -> Retention {
    Retention {
        completed_session_ttl: chrono::Duration::seconds(1000),
        acked_message_ttl: chrono::Duration::seconds(100),
        unacked_message_ttl: chrono::Duration::seconds(1000),
    }
}

pub(super) struct Flaky {
    pub(super) fail: Arc<AtomicBool>,
    pub(super) written: Arc<Mutex<Vec<u8>>>,
}

impl super::journal::Journal for Flaky {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::other("injected disk failure"));
        }
        self.written.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }
}

pub(super) fn flaky() -> (Flaky, Arc<AtomicBool>, Arc<Mutex<Vec<u8>>>) {
    let fail = Arc::new(AtomicBool::new(false));
    let written = Arc::new(Mutex::new(Vec::new()));
    (
        Flaky {
            fail: fail.clone(),
            written: written.clone(),
        },
        fail,
        written,
    )
}

pub(super) fn upsert_then_get<S: SessionStore>(s: &S) {
    let r = root(1);
    let rec = session("a", &r, SessionStatus::Running, 0, 0);
    assert_eq!(s.get(sid("a")).unwrap(), None);
    s.upsert(rec.clone()).unwrap();
    assert_eq!(s.get(sid("a")).unwrap(), Some(rec));
    assert_eq!(s.get(sid("b")).unwrap(), None);
}

pub(super) fn upsert_replaces_by_id<S: SessionStore>(s: &S) {
    let r = root(1);
    s.upsert(session("a", &r, SessionStatus::Pending, 0, 0))
        .unwrap();
    let mut second = session("a", &r, SessionStatus::Running, 0, 5);
    second.subtask = "changed".into();
    s.upsert(second.clone()).unwrap();
    assert_eq!(s.get(sid("a")).unwrap(), Some(second));
    assert_eq!(s.in_root(&r).unwrap().len(), 1);
}

pub(super) fn in_root_filters_and_orders<S: SessionStore>(s: &S) {
    let (r1, r2) = (root(1), root(2));
    s.upsert(session("c", &r1, SessionStatus::Running, 30, 30))
        .unwrap();
    s.upsert(session("b", &r1, SessionStatus::Running, 10, 10))
        .unwrap();
    s.upsert(session("z", &r2, SessionStatus::Running, 5, 5))
        .unwrap();
    s.upsert(session("a", &r1, SessionStatus::Running, 10, 10))
        .unwrap();
    let ids: Vec<String> = s
        .in_root(&r1)
        .unwrap()
        .iter()
        .map(|r| r.id.to_string())
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
    assert_eq!(s.in_root(&r2).unwrap().len(), 1);
    assert!(s.in_root(&root(3)).unwrap().is_empty());
}

pub(super) fn set_status_tracks_activity_and_end<S: SessionStore>(s: &S) {
    let r = root(1);
    s.upsert(session("a", &r, SessionStatus::Running, 0, 0))
        .unwrap();
    s.set_status(sid("a"), SessionStatus::Completed, at(50))
        .unwrap();
    let done = s.get(sid("a")).unwrap().unwrap();
    assert_eq!(done.status, SessionStatus::Completed);
    assert_eq!(done.last_activity, at(50));
    assert_eq!(done.ended_at, Some(at(50)));
    s.set_status(sid("a"), SessionStatus::WaitingOnMessages, at(60))
        .unwrap();
    let resumed = s.get(sid("a")).unwrap().unwrap();
    assert_eq!(resumed.status, SessionStatus::WaitingOnMessages);
    assert_eq!(resumed.last_activity, at(60));
    assert_eq!(resumed.ended_at, None);
    assert!(matches!(
        s.set_status(sid("missing"), SessionStatus::Failed, at(1)),
        Err(StoreError::NotFound)
    ));
}

fn note(text: &str) -> crate::hub::summary::Summary {
    crate::hub::summary::Summary::compose("a", "running", text)
}

pub(super) fn set_note_replaces<S: SessionStore>(s: &S) {
    let r = root(1);
    s.upsert(session("a", &r, SessionStatus::Running, 0, 0))
        .unwrap();
    s.set_note(sid("a"), note("first")).unwrap();
    assert_eq!(s.get(sid("a")).unwrap().unwrap().note, Some(note("first")));
    s.set_note(sid("a"), note("second")).unwrap();
    assert_eq!(s.get(sid("a")).unwrap().unwrap().note, Some(note("second")));
    assert!(matches!(
        s.set_note(sid("missing"), note("n")),
        Err(StoreError::NotFound)
    ));
}

pub(super) fn update_replaces_declines_and_reports_a_missing_id<S: SessionStore>(s: &S) {
    let r = root(1);
    s.upsert(session("a", &r, SessionStatus::Running, 0, 0))
        .unwrap();
    s.update(
        sid("a"),
        Box::new(|mut record| {
            record.subtask = "changed".into();
            Some(record)
        }),
    )
    .unwrap();
    let changed = s.get(sid("a")).unwrap().unwrap();
    assert_eq!(changed.subtask, "changed");
    s.update(sid("a"), Box::new(|_| None)).unwrap();
    assert_eq!(s.get(sid("a")).unwrap().unwrap(), changed);
    let missing = s.update(sid("missing"), Box::new(|_| None));
    assert!(matches!(missing, Err(StoreError::NotFound)));
}

pub(super) fn prune_removes_only_finished_idle_roots<S: SessionStore>(s: &S) {
    let (old, busy, fresh) = (root(1), root(2), root(3));
    s.upsert(session("old1", &old, SessionStatus::Completed, 0, 10))
        .unwrap();
    s.upsert(session("old2", &old, SessionStatus::Failed, 0, 20))
        .unwrap();
    s.upsert(session("busy-done", &busy, SessionStatus::Completed, 0, 10))
        .unwrap();
    s.upsert(session(
        "busy-live",
        &busy,
        SessionStatus::WaitingOnMessages,
        0,
        10,
    ))
    .unwrap();
    s.upsert(session("fresh", &fresh, SessionStatus::Cancelled, 0, 5000))
        .unwrap();
    let now = at(1021);
    assert_eq!(s.prune(now, &policy()).unwrap(), 2);
    assert!(s.in_root(&old).unwrap().is_empty());
    assert_eq!(s.in_root(&busy).unwrap().len(), 2);
    assert_eq!(s.in_root(&fresh).unwrap().len(), 1);
    assert_eq!(s.prune(now, &policy()).unwrap(), 0);
}

pub(super) fn prune_keeps_a_root_exactly_at_the_ttl<S: SessionStore>(s: &S) {
    let r = root(1);
    s.upsert(session("a", &r, SessionStatus::Completed, 0, 0))
        .unwrap();
    s.upsert(session("b", &r, SessionStatus::Completed, 0, 500))
        .unwrap();
    assert_eq!(s.prune(at(1500), &policy()).unwrap(), 0);
    assert_eq!(s.prune(at(1501), &policy()).unwrap(), 2);
}

pub(super) fn append_assigns_increasing_seq<S: MessageStore>(s: &S) {
    let a = s
        .append(message(1, "a", "b", DeliveryState::Pending, 0, "x"))
        .unwrap();
    let b = s
        .append(message(2, "a", "b", DeliveryState::Pending, 0, "y"))
        .unwrap();
    assert_eq!((a, b), (Seq::new(1), Seq::new(2)));
    assert_eq!(s.get(mid(1)).unwrap().unwrap().seq, Seq::new(1));
    assert_eq!(s.get(mid(2)).unwrap().unwrap().body, "y");
    assert_eq!(s.get(mid(3)).unwrap(), None);
}

pub(super) fn duplicate_id_is_refused_and_changes_nothing<S: MessageStore>(s: &S) {
    s.append(message(1, "a", "b", DeliveryState::Pending, 0, "first"))
        .unwrap();
    assert!(matches!(
        s.append(message(1, "a", "b", DeliveryState::Pending, 0, "second")),
        Err(StoreError::Duplicate)
    ));
    assert_eq!(s.unread(sid("b")).unwrap(), 1);
    assert_eq!(s.get(mid(1)).unwrap().unwrap().body, "first");
    assert_eq!(
        s.append(message(2, "a", "b", DeliveryState::Pending, 0, "third"))
            .unwrap(),
        Seq::new(2)
    );
}

pub(super) fn inbox_is_ordered_scoped_and_limited<S: MessageStore>(s: &S) {
    for n in 1..=5u128 {
        s.append(message(n, "a", "b", DeliveryState::Pending, 0, "m"))
            .unwrap();
    }
    s.append(message(6, "a", "other", DeliveryState::Pending, 0, "m"))
        .unwrap();
    let seqs = |after: Option<u64>, limit: usize| -> Vec<u64> {
        s.inbox(sid("b"), after.map(Seq::new), limit)
            .unwrap()
            .iter()
            .map(|m| m.seq.get())
            .collect()
    };
    assert_eq!(seqs(None, 100), [1, 2, 3, 4, 5]);
    assert_eq!(seqs(Some(2), 100), [3, 4, 5]);
    assert_eq!(seqs(Some(5), 100), Vec::<u64>::new());
    assert_eq!(seqs(None, 2), [1, 2]);
    assert_eq!(seqs(Some(1), 0), Vec::<u64>::new());
    assert_eq!(s.inbox(sid("other"), None, 100).unwrap().len(), 1);
}

pub(super) fn inbox_and_unread_only_see_deliverable_states<S: MessageStore>(s: &S) {
    let states = [
        DeliveryState::Pending,
        DeliveryState::Delivered,
        DeliveryState::Acked,
        DeliveryState::Withheld,
        DeliveryState::Quarantined,
        DeliveryState::Expired,
    ];
    for (n, state) in states.into_iter().enumerate() {
        s.append(message(n as u128 + 1, "a", "b", state, 0, "m"))
            .unwrap();
    }
    let seen: Vec<u64> = s
        .inbox(sid("b"), None, 100)
        .unwrap()
        .iter()
        .map(|m| m.seq.get())
        .collect();
    assert_eq!(seen, [1, 2]);
    assert_eq!(s.unread(sid("b")).unwrap(), 2);
    assert_eq!(s.unread(sid("nobody")).unwrap(), 0);
}

pub(super) fn ack_acknowledges_only_delivered_up_to_the_cursor<S: MessageStore>(s: &S) {
    s.append(message(1, "a", "b", DeliveryState::Delivered, 0, "m"))
        .unwrap();
    s.append(message(2, "a", "b", DeliveryState::Delivered, 0, "m"))
        .unwrap();
    s.append(message(3, "a", "b", DeliveryState::Pending, 0, "m"))
        .unwrap();
    s.append(message(4, "a", "b", DeliveryState::Quarantined, 0, "m"))
        .unwrap();
    s.append(message(5, "a", "c", DeliveryState::Delivered, 0, "m"))
        .unwrap();
    s.append(message(6, "a", "b", DeliveryState::Delivered, 0, "m"))
        .unwrap();
    let state = |n: u128| s.get(mid(n)).unwrap().unwrap().state;
    s.ack(sid("b"), Seq::new(4)).unwrap();
    assert_eq!(state(1), DeliveryState::Acked);
    assert_eq!(state(2), DeliveryState::Acked);
    assert_eq!(state(3), DeliveryState::Pending);
    assert_eq!(state(4), DeliveryState::Quarantined);
    assert_eq!(state(5), DeliveryState::Delivered);
    assert_eq!(state(6), DeliveryState::Delivered);
    assert_eq!(s.unread(sid("b")).unwrap(), 2);
    s.ack(sid("b"), Seq::new(4)).unwrap();
    assert_eq!(s.unread(sid("b")).unwrap(), 2);
    s.ack(sid("b"), Seq::new(100)).unwrap();
    assert_eq!(state(6), DeliveryState::Acked);
    assert_eq!(s.unread(sid("b")).unwrap(), 1);
}

pub(super) fn set_state_updates_or_reports_not_found<S: MessageStore>(s: &S) {
    s.append(message(1, "a", "b", DeliveryState::Pending, 0, "m"))
        .unwrap();
    s.set_state(mid(1), DeliveryState::Quarantined).unwrap();
    assert_eq!(
        s.get(mid(1)).unwrap().unwrap().state,
        DeliveryState::Quarantined
    );
    assert!(s.inbox(sid("b"), None, 10).unwrap().is_empty());
    assert!(matches!(
        s.set_state(mid(9), DeliveryState::Acked),
        Err(StoreError::NotFound)
    ));
}

pub(super) fn outbound_window_is_the_newest_messages_within_the_budget<S: MessageStore>(s: &S) {
    for (n, body) in ["aaaa", "bbbbbb", "cc", "dddd"].into_iter().enumerate() {
        let mut m = message(n as u128 + 1, "a", "b", DeliveryState::Pending, 0, body);
        m.subject = "s".into();
        s.append(m).unwrap();
    }
    s.append(message(10, "other", "b", DeliveryState::Pending, 0, "zzzz"))
        .unwrap();
    let bodies = |bytes: usize| -> Vec<String> {
        s.outbound_window(sid("a"), Recipient::Parent, bytes)
            .unwrap()
            .into_iter()
            .map(|m| m.body)
            .collect()
    };
    assert_eq!(bodies(0), Vec::<String>::new());
    assert_eq!(bodies(4), Vec::<String>::new());
    assert_eq!(bodies(5), ["dddd"]);
    assert_eq!(bodies(8), ["cc", "dddd"]);
    assert_eq!(bodies(14), ["cc", "dddd"]);
    assert_eq!(bodies(15), ["bbbbbb", "cc", "dddd"]);
    assert_eq!(bodies(19), ["bbbbbb", "cc", "dddd"]);
    assert_eq!(bodies(20), ["aaaa", "bbbbbb", "cc", "dddd"]);
    assert_eq!(bodies(10_000).len(), 4);
}

pub(super) fn outbound_window_narrows_to_a_session_recipient<S: MessageStore>(s: &S) {
    s.append(message(1, "a", "b", DeliveryState::Pending, 0, "to-b"))
        .unwrap();
    s.append(message(2, "a", "c", DeliveryState::Quarantined, 0, "to-c"))
        .unwrap();
    let to = |recipient: Recipient| -> Vec<String> {
        s.outbound_window(sid("a"), recipient, 1000)
            .unwrap()
            .into_iter()
            .map(|m| m.body)
            .collect()
    };
    assert_eq!(to(Recipient::Session { id: sid("c") }), ["to-c"]);
    assert_eq!(to(Recipient::Session { id: sid("b") }), ["to-b"]);
    assert_eq!(to(Recipient::Root), ["to-b", "to-c"]);
    assert_eq!(to(Recipient::Children), ["to-b", "to-c"]);
    assert_eq!(to(Recipient::Siblings), ["to-b", "to-c"]);
    assert!(to(Recipient::Session { id: sid("d") }).is_empty());
}

pub(super) fn prune_applies_the_retention_per_state<S: MessageStore>(s: &S) {
    let ancient = 0;
    let states = [
        (DeliveryState::Acked, 1),
        (DeliveryState::Pending, 2),
        (DeliveryState::Delivered, 3),
        (DeliveryState::Withheld, 4),
        (DeliveryState::Expired, 5),
        (DeliveryState::Quarantined, 6),
    ];
    for (state, n) in states {
        s.append(message(n, "a", "b", state, ancient, "m")).unwrap();
    }
    s.append(message(
        7,
        "a",
        "b",
        DeliveryState::Acked,
        960,
        "recent-acked",
    ))
    .unwrap();
    s.append(message(
        8,
        "a",
        "b",
        DeliveryState::Pending,
        1050,
        "recent-pending",
    ))
    .unwrap();
    let now = at(1051);
    assert_eq!(s.prune(now, &policy()).unwrap(), 5);
    let alive: Vec<u128> = (1..=8)
        .filter(|n| s.get(mid(*n)).unwrap().is_some())
        .collect();
    assert_eq!(alive, [6, 7, 8]);
    assert_eq!(s.prune(now, &policy()).unwrap(), 0);
}

pub(super) fn prune_boundary_keeps_a_message_exactly_at_the_ttl<S: MessageStore>(s: &S) {
    s.append(message(1, "a", "b", DeliveryState::Acked, 0, "m"))
        .unwrap();
    s.append(message(2, "a", "b", DeliveryState::Pending, 0, "m"))
        .unwrap();
    assert_eq!(s.prune(at(100), &policy()).unwrap(), 0);
    assert_eq!(s.prune(at(101), &policy()).unwrap(), 1);
    assert!(s.get(mid(2)).unwrap().is_some());
    assert_eq!(s.prune(at(1000), &policy()).unwrap(), 0);
    assert_eq!(s.prune(at(1001), &policy()).unwrap(), 1);
}

pub(super) fn quarantined_messages_survive_any_age<S: MessageStore>(s: &S) {
    s.append(message(1, "a", "b", DeliveryState::Quarantined, 0, "m"))
        .unwrap();
    let far_future = at(10_000_000_000);
    assert_eq!(s.prune(far_future, &policy()).unwrap(), 0);
    assert_eq!(
        s.get(mid(1)).unwrap().unwrap().state,
        DeliveryState::Quarantined
    );
}

pub(super) fn seq_is_never_reused_after_everything_is_pruned<S: MessageStore>(s: &S) {
    s.append(message(1, "a", "b", DeliveryState::Pending, 0, "m"))
        .unwrap();
    s.append(message(2, "a", "b", DeliveryState::Pending, 0, "m"))
        .unwrap();
    assert_eq!(s.prune(at(5000), &policy()).unwrap(), 2);
    let next = s
        .append(message(3, "a", "b", DeliveryState::Pending, 0, "m"))
        .unwrap();
    assert_eq!(next, Seq::new(3));
}

macro_rules! session_contract_tests {
    ($make:expr) => {
        session_contract_tests!(@each $make,
            upsert_then_get,
            upsert_replaces_by_id,
            in_root_filters_and_orders,
            set_status_tracks_activity_and_end,
            set_note_replaces,
            update_replaces_declines_and_reports_a_missing_id,
            prune_removes_only_finished_idle_roots,
            prune_keeps_a_root_exactly_at_the_ttl
        );
    };
    (@each $make:expr, $($name:ident),+) => {
        $(
            #[test]
            fn $name() {
                let (_keep, store) = $make;
                $crate::hub::store::contract::$name(&store);
            }
        )+
    };
}

macro_rules! message_contract_tests {
    ($make:expr) => {
        message_contract_tests!(@each $make,
            append_assigns_increasing_seq,
            duplicate_id_is_refused_and_changes_nothing,
            inbox_is_ordered_scoped_and_limited,
            inbox_and_unread_only_see_deliverable_states,
            ack_acknowledges_only_delivered_up_to_the_cursor,
            set_state_updates_or_reports_not_found,
            outbound_window_is_the_newest_messages_within_the_budget,
            outbound_window_narrows_to_a_session_recipient,
            prune_applies_the_retention_per_state,
            prune_boundary_keeps_a_message_exactly_at_the_ttl,
            quarantined_messages_survive_any_age,
            seq_is_never_reused_after_everything_is_pruned
        );
    };
    (@each $make:expr, $($name:ident),+) => {
        $(
            #[test]
            fn $name() {
                let (_keep, store) = $make;
                $crate::hub::store::contract::$name(&store);
            }
        )+
    };
}

pub(super) struct Fixture {
    pub(super) dir: tempfile::TempDir,
    pub(super) lock: Arc<super::HubLock>,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let lock = super::HubLock::acquire(dir.path()).unwrap();
        Self { dir, lock }
    }

    pub(super) fn path(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }
}

struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn logged<R>(run: impl FnOnce() -> R) -> (R, String) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let sink = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || Capture(sink.clone()))
        .with_ansi(false)
        .finish();
    let result = tracing::subscriber::with_default(subscriber, run);
    let text = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    (result, text)
}
