use super::engine::{invalid, Engine, Table};
use super::journal::HubLock;
use super::{MessageStore, Retention, StoreError};
use crate::auditor::ReasonCode;
use crate::hub::lineage::SessionId;
use crate::hub::message::{DeliveryState, MessageId, Recipient, Release, Seq, StoredMessage};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::path::Path;
use std::sync::Arc;

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum Record {
    Counter {
        next: u64,
    },
    Append {
        message: Box<StoredMessage>,
    },
    State {
        id: MessageId,
        state: DeliveryState,
    },
    Transition {
        id: MessageId,
        from: DeliveryState,
        to: DeliveryState,
        release: Option<Release>,
    },
    Drop {
        id: MessageId,
    },
}

#[derive(Clone)]
pub(super) struct MessageTable {
    next: u64,
    by_seq: BTreeMap<Seq, StoredMessage>,
    seq_of: HashMap<MessageId, Seq>,
}

impl Default for MessageTable {
    fn default() -> Self {
        Self {
            next: 1,
            by_seq: BTreeMap::new(),
            seq_of: HashMap::new(),
        }
    }
}

fn ensure_held(message: &mut StoredMessage) {
    if message.state == DeliveryState::Quarantined && message.held.is_empty() {
        message.held.push(ReasonCode::Other);
    }
}

fn deliverable(state: DeliveryState) -> bool {
    matches!(state, DeliveryState::Pending | DeliveryState::Delivered)
}

impl Table for MessageTable {
    type Record = Record;

    fn apply(&mut self, record: &Record) -> Result<(), StoreError> {
        match record {
            Record::Counter { next } => {
                self.next = self.next.max(*next);
            }
            Record::Append { message } => {
                if self.seq_of.contains_key(&message.id) {
                    return Err(StoreError::Duplicate);
                }
                let after = message
                    .seq
                    .get()
                    .checked_add(1)
                    .filter(|_| message.seq.get() >= self.next)
                    .ok_or_else(|| invalid("message sequence is exhausted or out of order"))?;
                self.next = after;
                self.seq_of.insert(message.id, message.seq);
                let mut stored = (**message).clone();
                ensure_held(&mut stored);
                self.by_seq.insert(message.seq, stored);
            }
            Record::State { id, state } => {
                let seq = self.seq_of.get(id).ok_or(StoreError::NotFound)?;
                if let Some(found) = self.by_seq.get_mut(seq) {
                    found.state = *state;
                    ensure_held(found);
                }
            }
            Record::Transition {
                id,
                from,
                to,
                release,
            } => {
                let updated = self.resolve(*id, *from, *to, *release)?;
                self.by_seq.insert(updated.seq, updated);
            }
            Record::Drop { id } => {
                let seq = self.seq_of.remove(id).ok_or(StoreError::NotFound)?;
                self.by_seq.remove(&seq);
            }
        }
        Ok(())
    }

    fn records(&self) -> Vec<Record> {
        let mut records: Vec<Record> = self
            .by_seq
            .values()
            .map(|message| Record::Append {
                message: Box::new(message.clone()),
            })
            .collect();
        records.push(Record::Counter { next: self.next });
        records
    }
}

impl MessageTable {
    fn admit(&self, message: StoredMessage) -> StoredMessage {
        StoredMessage {
            seq: Seq::new(self.next),
            released: None,
            ..message
        }
    }

    fn quarantined(&self) -> Vec<StoredMessage> {
        let all = self.by_seq.values();
        let held = all.filter(|message| message.state == DeliveryState::Quarantined);
        held.cloned().collect()
    }

    fn resolve(
        &self,
        id: MessageId,
        from: DeliveryState,
        to: DeliveryState,
        release: Option<Release>,
    ) -> Result<StoredMessage, StoreError> {
        let seq = self.seq_of.get(&id).ok_or(StoreError::NotFound)?;
        let found = self.by_seq.get(seq).ok_or(StoreError::NotFound)?;
        let release_pair = from == DeliveryState::Quarantined && to == DeliveryState::Pending;
        let marker_allowed = release.is_none() || release_pair;
        if found.state != from || !marker_allowed {
            return Err(StoreError::Conflict);
        }
        let mut updated = found.clone();
        let kept = updated.released.take();
        updated.released = release.or(kept);
        updated.state = to;
        ensure_held(&mut updated);
        Ok(updated)
    }

    fn plan_transition(
        &self,
        id: MessageId,
        from: DeliveryState,
        to: DeliveryState,
        release: Option<Release>,
    ) -> Result<(StoredMessage, Vec<Record>), StoreError> {
        let updated = self.resolve(id, from, to, release)?;
        let record = Record::Transition {
            id,
            from,
            to,
            release,
        };
        Ok((updated, vec![record]))
    }

    fn expired_ids(&self, now: DateTime<Utc>, policy: &Retention) -> Vec<MessageId> {
        self.by_seq
            .values()
            .filter(|message| {
                let ttl = match message.state {
                    DeliveryState::Quarantined => return false,
                    DeliveryState::Acked => policy.acked_message_ttl,
                    _ => policy.unacked_message_ttl,
                };
                now.signed_duration_since(message.sent_at) > ttl
            })
            .map(|message| message.id)
            .collect()
    }
}

macro_rules! message_store_impl {
    ($ty:ty) => {
        impl MessageStore for $ty {
            fn append(&self, m: StoredMessage) -> Result<Seq, StoreError> {
                self.engine.commit(|table| {
                    let message = table.admit(m);
                    let seq = message.seq;
                    Ok((
                        seq,
                        vec![Record::Append {
                            message: Box::new(message),
                        }],
                    ))
                })
            }

            fn get(&self, id: MessageId) -> Result<Option<StoredMessage>, StoreError> {
                Ok(self.engine.read(|table| {
                    table
                        .seq_of
                        .get(&id)
                        .and_then(|seq| table.by_seq.get(seq))
                        .cloned()
                }))
            }

            fn inbox(
                &self,
                to: SessionId,
                after: Option<Seq>,
                limit: usize,
            ) -> Result<Vec<StoredMessage>, StoreError> {
                let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
                Ok(self.engine.read(|table| {
                    table
                        .by_seq
                        .range((lower, Bound::Unbounded))
                        .map(|(_, message)| message)
                        .filter(|message| message.to == to && deliverable(message.state))
                        .take(limit)
                        .cloned()
                        .collect()
                }))
            }

            fn ack(&self, to: SessionId, upto: Seq) -> Result<(), StoreError> {
                self.engine.commit(|table| {
                    let records = table
                        .by_seq
                        .range(..=upto)
                        .map(|(_, message)| message)
                        .filter(|message| {
                            message.to == to && message.state == DeliveryState::Delivered
                        })
                        .map(|message| Record::State {
                            id: message.id,
                            state: DeliveryState::Acked,
                        })
                        .collect();
                    Ok(((), records))
                })
            }

            fn unread(&self, to: SessionId) -> Result<usize, StoreError> {
                Ok(self.engine.read(|table| {
                    table
                        .by_seq
                        .values()
                        .filter(|message| message.to == to && deliverable(message.state))
                        .count()
                }))
            }

            fn outbound_window(
                &self,
                from: SessionId,
                to: Recipient,
                bytes: usize,
            ) -> Result<Vec<StoredMessage>, StoreError> {
                Ok(self.engine.read(|table| {
                    let mut used = 0usize;
                    let mut window: Vec<StoredMessage> = table
                        .by_seq
                        .values()
                        .rev()
                        .filter(|message| {
                            message.from == from
                                && match &to {
                                    Recipient::Session { id } => message.to == *id,
                                    _ => true,
                                }
                        })
                        .take_while(|message| {
                            used = used.saturating_add(message.subject.len() + message.body.len());
                            used <= bytes
                        })
                        .cloned()
                        .collect();
                    window.reverse();
                    window
                }))
            }

            fn set_state(&self, id: MessageId, s: DeliveryState) -> Result<(), StoreError> {
                self.engine
                    .commit(|_| Ok(((), vec![Record::State { id, state: s }])))
            }

            fn quarantined(&self) -> Result<Vec<StoredMessage>, StoreError> {
                Ok(self.engine.read(|table| table.quarantined()))
            }

            fn transition(
                &self,
                id: MessageId,
                from: DeliveryState,
                to: DeliveryState,
                release: Option<Release>,
            ) -> Result<StoredMessage, StoreError> {
                let plan = |table: &MessageTable| table.plan_transition(id, from, to, release);
                self.engine.commit(plan)
            }

            fn prune(&self, now: DateTime<Utc>, policy: &Retention) -> Result<usize, StoreError> {
                self.engine.commit(|table| {
                    let ids = table.expired_ids(now, policy);
                    let removed = ids.len();
                    Ok((
                        removed,
                        ids.into_iter().map(|id| Record::Drop { id }).collect(),
                    ))
                })
            }
        }
    };
}

/// Volatile [`MessageStore`] for tests and for hubs that do not persist.
///
/// ```
/// use harness::hub::message::MessageId;
/// use harness::hub::store::{InMemoryMessageStore, MessageStore};
///
/// let store = InMemoryMessageStore::new();
/// assert!(store.get(MessageId::new()).unwrap().is_none());
/// ```
pub struct InMemoryMessageStore {
    engine: Engine<MessageTable>,
}

impl InMemoryMessageStore {
    /// An empty store.
    ///
    /// ```
    /// let _store = harness::hub::store::InMemoryMessageStore::new();
    /// let _same = harness::hub::store::InMemoryMessageStore::default();
    /// ```
    pub fn new() -> Self {
        Self {
            engine: Engine::memory(),
        }
    }
}

impl Default for InMemoryMessageStore {
    fn default() -> Self {
        Self::new()
    }
}

message_store_impl!(InMemoryMessageStore);

/// Durable [`MessageStore`]: one JSON record per line in a file created with
/// mode `0600`, replayed and compacted on [`open`](Self::open). This file is
/// the one sanctioned place message bodies are stored. The sequence counter
/// is persisted, so a [`Seq`] is never reused even after retention removes
/// every message.
///
/// ```
/// use chrono::Utc;
/// use harness::hub::lineage::{RootId, SessionId};
/// use harness::hub::message::*;
/// use harness::hub::store::{HubLock, JsonlMessageStore, MessageStore};
/// use harness::task::TaskId;
///
/// let dir = tempfile::tempdir().unwrap();
/// let lock = HubLock::acquire(dir.path()).unwrap();
/// let path = dir.path().join("messages.jsonl");
/// let to = SessionId::from_task(TaskId("b".into()));
/// let message = StoredMessage {
///     id: MessageId::new(),
///     seq: Seq::new(0),
///     from: SessionId::from_task(TaskId("a".into())),
///     from_identity: "coder".into(),
///     to: to.clone(),
///     root: RootId::mcp(uuid::Uuid::nil()),
///     kind: MessageKind::Info,
///     subject: "hi".into(),
///     body: "hello".into(),
///     in_reply_to: None,
///     thread_depth: 0,
///     outbound_verdict: VerdictRef { log_id: "v".into(), content_hash: 1 },
///     state: DeliveryState::Pending,
///     sent_at: Utc::now(),
///     held: vec![],
///     released: None,
/// };
/// let seq = JsonlMessageStore::open(&path, &lock).unwrap().append(message).unwrap();
/// assert_eq!(seq, Seq::new(1));
/// let reopened = JsonlMessageStore::open(&path, &lock).unwrap();
/// assert_eq!(reopened.unread(to).unwrap(), 1);
/// ```
pub struct JsonlMessageStore {
    engine: Engine<MessageTable>,
}

impl JsonlMessageStore {
    /// Replay `path` (creating it, mode `0600`, when absent), ignore a
    /// truncated final line, fail with [`StoreError::Corrupt`] on any other
    /// unreadable line, and rewrite the file with only the live records.
    /// `lock` must protect the directory that holds `path`.
    ///
    /// ```
    /// use harness::hub::store::{HubLock, JsonlMessageStore};
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let lock = HubLock::acquire(dir.path()).unwrap();
    /// assert!(JsonlMessageStore::open(&dir.path().join("messages.jsonl"), &lock).is_ok());
    /// assert!(JsonlMessageStore::open(&dir.path().join("sub").join("m.jsonl"), &lock).is_err());
    /// ```
    pub fn open(path: &Path, lock: &Arc<HubLock>) -> Result<Self, StoreError> {
        Ok(Self {
            engine: Engine::open(path, lock)?,
        })
    }
}

message_store_impl!(JsonlMessageStore);

#[cfg(test)]
mod tests {
    use super::super::contract::*;
    use super::super::journal::Journal;
    use super::*;
    use crate::auditor::ReasonCode;
    use proptest::prelude::*;
    use std::sync::atomic::Ordering;

    mod memory {
        use super::*;
        message_contract_tests!(((), InMemoryMessageStore::new()));
    }

    mod jsonl {
        use super::*;
        message_contract_tests!({
            let fixture = Fixture::new();
            let store =
                JsonlMessageStore::open(&fixture.path("messages.jsonl"), &fixture.lock).unwrap();
            (fixture, store)
        });
    }

    fn open(fixture: &Fixture) -> Result<JsonlMessageStore, StoreError> {
        JsonlMessageStore::open(&fixture.path("messages.jsonl"), &fixture.lock)
    }

    fn file_text(fixture: &Fixture) -> String {
        std::fs::read_to_string(fixture.path("messages.jsonl")).unwrap()
    }

    #[test]
    fn state_survives_a_reopen_including_state_changes_and_acks() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        store
            .append(message(1, "a", "b", DeliveryState::Delivered, 0, "one"))
            .unwrap();
        store
            .append(message(2, "a", "b", DeliveryState::Pending, 0, "two"))
            .unwrap();
        store
            .append(message(3, "a", "b", DeliveryState::Pending, 0, "three"))
            .unwrap();
        store.ack(sid("b"), Seq::new(2)).unwrap();
        store.set_state(mid(3), DeliveryState::Quarantined).unwrap();
        drop(store);
        let reopened = open(&fixture).unwrap();
        let state = |n| reopened.get(mid(n)).unwrap().unwrap().state;
        assert_eq!(state(1), DeliveryState::Acked);
        assert_eq!(state(2), DeliveryState::Pending);
        assert_eq!(state(3), DeliveryState::Quarantined);
        assert_eq!(reopened.get(mid(1)).unwrap().unwrap().body, "one");
        assert_eq!(reopened.unread(sid("b")).unwrap(), 1);
    }

    #[test]
    fn the_sequence_counter_survives_a_reopen_after_everything_was_pruned() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        store
            .append(message(1, "a", "b", DeliveryState::Pending, 0, "m"))
            .unwrap();
        store
            .append(message(2, "a", "b", DeliveryState::Pending, 0, "m"))
            .unwrap();
        assert_eq!(store.prune(at(5000), &policy()).unwrap(), 2);
        drop(store);
        let reopened = open(&fixture).unwrap();
        let next = reopened
            .append(message(3, "a", "b", DeliveryState::Pending, 0, "m"))
            .unwrap();
        assert_eq!(next, Seq::new(3));
    }

    #[test]
    fn open_compacts_the_log_and_drops_pruned_messages() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        for n in 1..=4 {
            store
                .append(message(n, "a", "b", DeliveryState::Pending, 0, "m"))
                .unwrap();
        }
        store.set_state(mid(1), DeliveryState::Acked).unwrap();
        assert_eq!(store.prune(at(5000), &policy()).unwrap(), 4);
        drop(store);
        let _reopened = open(&fixture).unwrap();
        assert_eq!(file_text(&fixture).lines().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_created_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        store
            .append(message(1, "a", "b", DeliveryState::Pending, 0, "m"))
            .unwrap();
        for name in ["messages.jsonl", "hub.lock"] {
            let mode = std::fs::metadata(fixture.path(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }

    #[test]
    fn a_truncated_last_line_is_ignored_with_a_warning_and_never_logged() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        store
            .append(message(1, "a", "b", DeliveryState::Pending, 0, "kept"))
            .unwrap();
        drop(store);
        let mut text = file_text(&fixture);
        text.push_str(&format!(
            r#"{{"op":"append","message":{{"id":"x","body":"{TOKEN}"#
        ));
        std::fs::write(fixture.path("messages.jsonl"), text).unwrap();
        let (reopened, log) = logged(|| open(&fixture).unwrap());
        assert!(log.contains("truncated final record"), "{log}");
        assert!(!log.contains(TOKEN));
        assert_eq!(reopened.unread(sid("b")).unwrap(), 1);
        assert!(!file_text(&fixture).contains(TOKEN));
    }

    #[test]
    fn a_corrupt_middle_line_fails_closed_without_leaking_the_body() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        store
            .append(message(1, "a", "b", DeliveryState::Pending, 0, TOKEN))
            .unwrap();
        store
            .append(message(2, "a", "b", DeliveryState::Pending, 0, "m"))
            .unwrap();
        drop(store);
        let text = file_text(&fixture);
        let mut lines: Vec<&str> = text.lines().collect();
        let broken = lines[1].replace("\"state\":\"pending\"", "\"state\":\"bogus\"");
        lines[1] = &broken;
        let corrupt = format!("{}\n", lines.join("\n"));
        std::fs::write(fixture.path("messages.jsonl"), &corrupt).unwrap();
        let error = open(&fixture).err().unwrap();
        assert!(matches!(error, StoreError::Corrupt(2)), "{error:?}");
        assert!(!format!("{error:?} {error}").contains(TOKEN));
        assert_eq!(file_text(&fixture), corrupt);
    }

    #[test]
    fn records_that_cannot_apply_are_corrupt() {
        let fixture = Fixture::new();
        let state = format!(
            r#"{{"op":"state","id":"{}","state":"acked"}}"#,
            mid(1).as_uuid()
        );
        std::fs::write(fixture.path("messages.jsonl"), format!("{state}\n")).unwrap();
        assert!(matches!(open(&fixture).err(), Some(StoreError::Corrupt(1))));
        let append = |n: u128, seq: u64| {
            let mut m = message(n, "a", "b", DeliveryState::Pending, 0, "m");
            m.seq = Seq::new(seq);
            serde_json::to_string(&Record::Append {
                message: Box::new(m),
            })
            .unwrap()
        };
        let counter = r#"{"op":"counter","next":5}"#;
        let cases = [
            format!("{}\n{}\n", append(1, 3), append(2, 2)),
            format!("{counter}\n{}\n", append(1, 4)),
            format!("{}\n{}\n", append(1, 3), append(1, 4)),
            format!("{}\n", append(1, u64::MAX)),
        ];
        for text in cases {
            std::fs::write(fixture.path("messages.jsonl"), text).unwrap();
            assert!(matches!(open(&fixture).err(), Some(StoreError::Corrupt(_))));
        }
    }

    #[test]
    fn an_inapplicable_record_between_good_ones_fails_closed_at_its_own_line() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        for n in 1..=2 {
            store
                .append(message(n, "a", "b", DeliveryState::Pending, 0, "m"))
                .unwrap();
        }
        drop(store);
        let text = file_text(&fixture);
        let good: Vec<&str> = text.lines().collect();
        let ghost = format!(r#"{{"op":"drop","id":"{}"}}"#, mid(9).as_uuid());
        let broken = format!("{}\n{ghost}\n{}\n{}\n", good[0], good[1], good[2]);
        std::fs::write(fixture.path("messages.jsonl"), &broken).unwrap();
        assert!(matches!(open(&fixture).err(), Some(StoreError::Corrupt(2))));
        assert_eq!(file_text(&fixture), broken);
    }

    #[test]
    fn an_exhausted_sequence_space_refuses_the_append() {
        let fixture = Fixture::new();
        let text = format!(r#"{{"op":"counter","next":{}}}"#, u64::MAX);
        std::fs::write(fixture.path("messages.jsonl"), format!("{text}\n")).unwrap();
        let store = open(&fixture).unwrap();
        let result = store.append(message(1, "a", "b", DeliveryState::Pending, 0, "m"));
        assert!(matches!(result, Err(StoreError::Io(_))));
        assert_eq!(store.get(mid(1)).unwrap(), None);
    }

    const LEGACY_QUARANTINED: &str = r#"{"op":"append","message":{"id":"00000000-0000-0000-0000-000000000001","seq":1,"from":"a","from_identity":"identity-a","to":"b","root":{"kind":"mcp","id":"00000000-0000-0000-0000-000000000001"},"kind":"info","subject":"s","body":"stored redacted text","in_reply_to":null,"thread_depth":0,"outbound_verdict":{"log_id":"v","content_hash":7},"state":"quarantined","sent_at":"2023-11-14T22:13:20Z"}}"#;

    #[test]
    fn a_legacy_quarantined_record_loads_held_and_is_not_deliverable() {
        let fixture = Fixture::new();
        let renumbered = LEGACY_QUARANTINED.replace("0001\",\"seq\":1", "0002\",\"seq\":2");
        let pending = renumbered.replace("\"quarantined\"", "\"pending\"");
        let journal = format!("{LEGACY_QUARANTINED}\n{pending}\n");
        assert!(!journal.contains("held") && !journal.contains("released"));
        std::fs::write(fixture.path("messages.jsonl"), journal).unwrap();
        let store = open(&fixture).unwrap();
        let loaded = store.get(mid(1)).unwrap().unwrap();
        assert_eq!(loaded.state, DeliveryState::Quarantined);
        assert_eq!(loaded.held, [ReasonCode::Other]);
        assert_eq!(loaded.released, None);
        let inbox = store.inbox(sid("b"), None, 10).unwrap();
        let waiting: Vec<MessageId> = inbox.iter().map(|m| m.id).collect();
        assert_eq!(waiting, [mid(2)]);
        assert_eq!(store.unread(sid("b")).unwrap(), 1);
        let listed = store.quarantined().unwrap();
        let held: Vec<MessageId> = listed.iter().map(|m| m.id).collect();
        assert_eq!(held, [mid(1)]);
        assert!(store.get(mid(2)).unwrap().unwrap().held.is_empty());
        drop(store);
        let reopened = open(&fixture).unwrap();
        assert_eq!(reopened.get(mid(1)).unwrap().unwrap().held, loaded.held);
        assert!(file_text(&fixture).contains(r#""held":["other"]"#));
    }

    #[test]
    fn a_transition_and_its_marker_survive_a_reopen_and_a_compaction() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        let mut parked = message(1, "a", "b", DeliveryState::Quarantined, 0, "stored");
        parked.held = vec![ReasonCode::PromptInjection];
        store.append(parked).unwrap();
        let release = Release { at: at(5) };
        let (quarantined, pending) = (DeliveryState::Quarantined, DeliveryState::Pending);
        let marker = Some(release);
        store
            .transition(mid(1), quarantined, pending, marker)
            .unwrap();
        assert!(file_text(&fixture).contains(r#""op":"transition""#));
        drop(store);
        for _ in 0..2 {
            let reopened = open(&fixture).unwrap();
            let found = reopened.get(mid(1)).unwrap().unwrap();
            assert_eq!(found.state, pending);
            assert_eq!(found.released, Some(release));
            assert_eq!(found.held, [ReasonCode::PromptInjection]);
            let stale = reopened.transition(mid(1), quarantined, DeliveryState::Expired, None);
            assert!(matches!(stale, Err(StoreError::Conflict)));
            assert_eq!(reopened.get(mid(1)).unwrap().unwrap(), found);
        }
    }

    #[test]
    fn a_caller_supplied_marker_never_reaches_the_journal() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        let mut forged = message(1, "a", "b", DeliveryState::Pending, 0, "m");
        forged.released = Some(Release { at: at(1) });
        store.append(forged).unwrap();
        assert!(!file_text(&fixture).contains("released"));
        assert_eq!(store.get(mid(1)).unwrap().unwrap().released, None);
    }

    #[test]
    fn a_journal_transition_that_cannot_apply_is_corrupt() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        let pending = message(1, "a", "b", DeliveryState::Pending, 0, "m");
        store.append(pending).unwrap();
        drop(store);
        let good = file_text(&fixture);
        let mut appends = good.lines().filter(|l| l.contains(r#""op":"append""#));
        let first = appends.next().unwrap();
        let tail = r#""to":"expired","release":null}"#;
        let transition = |n: u128, from: &str| {
            let id = mid(n).as_uuid();
            format!(r#"{{"op":"transition","id":"{id}","from":"{from}",{tail}"#)
        };
        let wrong_state = format!("{first}\n{}\n", transition(1, "quarantined"));
        std::fs::write(fixture.path("messages.jsonl"), wrong_state).unwrap();
        assert!(matches!(open(&fixture).err(), Some(StoreError::Corrupt(2))));
        let unknown = format!("{first}\n{}\n", transition(9, "pending"));
        std::fs::write(fixture.path("messages.jsonl"), unknown).unwrap();
        assert!(matches!(open(&fixture).err(), Some(StoreError::Corrupt(2))));
        let fine = format!("{first}\n{}\n", transition(1, "pending"));
        std::fs::write(fixture.path("messages.jsonl"), fine).unwrap();
        let reopened = open(&fixture).unwrap();
        let state = reopened.get(mid(1)).unwrap().unwrap().state;
        assert_eq!(state, DeliveryState::Expired);
    }

    #[test]
    fn a_failed_disk_write_in_a_transition_leaves_the_old_state_and_no_marker() {
        let (store, fail, written) = flaky_store();
        let mut parked = message(1, "a", "b", DeliveryState::Quarantined, 0, "stored");
        parked.held = vec![ReasonCode::SecretLeak];
        store.append(parked).unwrap();
        let written_before = written.lock().unwrap().clone();
        fail.store(true, Ordering::SeqCst);
        let (quarantined, pending) = (DeliveryState::Quarantined, DeliveryState::Pending);
        let release = Some(Release { at: at(3) });
        let refused = store.transition(mid(1), quarantined, pending, release);
        assert!(matches!(refused, Err(StoreError::Io(_))));
        let kept = store.get(mid(1)).unwrap().unwrap();
        assert_eq!((kept.state, kept.released), (quarantined, None));
        assert_eq!(*written.lock().unwrap(), written_before);
        fail.store(false, Ordering::SeqCst);
        let retried = store.transition(mid(1), quarantined, pending, release);
        assert_eq!(retried.unwrap().released, release);
    }

    #[derive(Debug, Clone)]
    enum HoldOp {
        Put { id: u8, state: u8 },
        SetState { id: u8, state: u8 },
        Move(u8, u8, u8, bool),
        Prune,
        Reopen,
    }

    fn to_move(parts: (u8, u8, u8, bool)) -> HoldOp {
        HoldOp::Move(parts.0, parts.1, parts.2, parts.3)
    }

    fn hold_strategy() -> impl Strategy<Value = HoldOp> {
        prop_oneof![
            (0u8..6, 0u8..6).prop_map(|(id, state)| HoldOp::Put { id, state }),
            (0u8..6, 0u8..6).prop_map(|(id, state)| HoldOp::SetState { id, state }),
            Strategy::prop_map((0u8..6, 0u8..6, 0u8..6, any::<bool>()), to_move),
            Just(HoldOp::Prune),
            Just(HoldOp::Reopen),
        ]
    }

    struct Seen {
        id: u8,
        seq: u64,
        state: DeliveryState,
        marked: bool,
    }

    fn allowed(from: DeliveryState, to: DeliveryState, release: bool) -> bool {
        let marked_moves = [(DeliveryState::Quarantined, DeliveryState::Pending)];
        !release || marked_moves.contains(&(from, to))
    }

    fn run_holds(
        ops: &[HoldOp],
        mut store: Box<dyn MessageStore>,
        mut reopen: impl FnMut(Box<dyn MessageStore>) -> Box<dyn MessageStore>,
    ) {
        let mut model: Vec<Seen> = Vec::new();
        let mut counter = 1u64;
        for op in ops {
            match op {
                HoldOp::Put { id, state } => {
                    let state = state_of(*state);
                    let record = message(u128::from(*id), "a", "b", state, 0, "m");
                    let result = store.append(record);
                    if model.iter().any(|e| e.id == *id) {
                        assert!(matches!(result, Err(StoreError::Duplicate)));
                    } else {
                        assert_eq!(result.unwrap().get(), counter);
                        model.push(Seen {
                            id: *id,
                            seq: counter,
                            state,
                            marked: false,
                        });
                        counter += 1;
                    }
                }
                HoldOp::SetState { id, state } => {
                    let state = state_of(*state);
                    let result = store.set_state(mid(u128::from(*id)), state);
                    match model.iter_mut().find(|e| e.id == *id) {
                        None => assert!(matches!(result, Err(StoreError::NotFound))),
                        Some(entry) => {
                            result.unwrap();
                            entry.state = state;
                        }
                    }
                }
                HoldOp::Move(id, from, to, release) => {
                    let stamp = Release { at: at(9) };
                    let marker = if *release { Some(stamp) } else { None };
                    let (from, to) = (state_of(*from), state_of(*to));
                    let result = store.transition(mid(u128::from(*id)), from, to, marker);
                    match model.iter_mut().find(|e| e.id == *id) {
                        None => assert!(matches!(result, Err(StoreError::NotFound))),
                        Some(entry) if entry.state == from && allowed(from, to, *release) => {
                            result.unwrap();
                            entry.state = to;
                            entry.marked = entry.marked || *release;
                        }
                        Some(_) => assert!(matches!(result, Err(StoreError::Conflict))),
                    }
                }
                HoldOp::Prune => {
                    let far = at(10_000_000);
                    store.prune(far, &policy()).unwrap();
                    model.retain(|e| e.state == DeliveryState::Quarantined);
                }
                HoldOp::Reopen => store = reopen(store),
            }
            let mut expected: Vec<(u64, u8)> = Vec::new();
            for entry in &model {
                if entry.state == DeliveryState::Quarantined {
                    expected.push((entry.seq, entry.id));
                }
            }
            expected.sort();
            let listed = store.quarantined().unwrap();
            let mut got: Vec<(u64, u8)> = Vec::new();
            for found in &listed {
                got.push((found.seq.get(), found.id.as_uuid().as_u128() as u8));
                assert!(!found.held.is_empty());
            }
            assert_eq!(got, expected);
            for entry in &model {
                let found = store.get(mid(u128::from(entry.id))).unwrap().unwrap();
                assert_eq!(found.state, entry.state);
                assert_eq!(found.released.is_some(), entry.marked);
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(40))]

        #[test]
        fn the_memory_store_listing_and_transitions_match_a_plain_model(ops in proptest::collection::vec(hold_strategy(), 0..40)) {
            run_holds(&ops, Box::new(InMemoryMessageStore::new()), |store| store);
        }

        #[test]
        fn the_jsonl_store_listing_and_transitions_match_a_plain_model_across_reopens(ops in proptest::collection::vec(hold_strategy(), 0..40)) {
            let fixture = Fixture::new();
            let store = Box::new(open(&fixture).unwrap());
            run_holds(&ops, store, |old| {
                drop(old);
                Box::new(open(&fixture).unwrap())
            });
        }
    }

    fn flaky_store() -> (
        InMemoryMessageStore,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ) {
        let (journal, fail, written) = flaky();
        let store = InMemoryMessageStore {
            engine: Engine::with_journal(Box::new(journal) as Box<dyn Journal>),
        };
        (store, fail, written)
    }

    #[test]
    fn a_failed_disk_write_delivers_nothing_and_changes_nothing() {
        let (store, fail, written) = flaky_store();
        store
            .append(message(1, "a", "b", DeliveryState::Delivered, 0, "kept"))
            .unwrap();
        let written_before = written.lock().unwrap().clone();
        fail.store(true, Ordering::SeqCst);
        let refused = store.append(message(2, "a", "b", DeliveryState::Pending, 0, TOKEN));
        assert!(matches!(refused, Err(StoreError::Io(_))));
        assert!(matches!(
            store.set_state(mid(1), DeliveryState::Quarantined),
            Err(StoreError::Io(_))
        ));
        assert!(matches!(
            store.ack(sid("b"), Seq::new(9)),
            Err(StoreError::Io(_))
        ));
        assert!(matches!(
            store.prune(at(100_000), &policy()),
            Err(StoreError::Io(_))
        ));
        assert_eq!(store.get(mid(2)).unwrap(), None);
        assert_eq!(
            store.get(mid(1)).unwrap().unwrap().state,
            DeliveryState::Delivered
        );
        assert_eq!(store.unread(sid("b")).unwrap(), 1);
        assert_eq!(*written.lock().unwrap(), written_before);
        fail.store(false, Ordering::SeqCst);
        let next = store
            .append(message(2, "a", "b", DeliveryState::Pending, 0, "retry"))
            .unwrap();
        assert_eq!(next, Seq::new(2));
    }

    #[test]
    fn no_error_or_debug_output_contains_a_message_body_or_subject() {
        let (store, fail, _written) = flaky_store();
        let mut m = message(1, "a", "b", DeliveryState::Pending, 0, TOKEN);
        m.subject = format!("subject-{TOKEN}");
        m.from_identity = format!("identity-{TOKEN}");
        let debug = format!("{m:?}");
        assert!(!debug.contains(TOKEN), "{debug}");
        assert!(debug.contains("body_bytes"));
        fail.store(true, Ordering::SeqCst);
        let error = store.append(m.clone()).err().unwrap();
        assert!(!format!("{error:?} {error}").contains(TOKEN));
        fail.store(false, Ordering::SeqCst);
        store.append(m).unwrap();
        let again = store
            .append(message(1, "a", "b", DeliveryState::Pending, 0, TOKEN))
            .err()
            .unwrap();
        assert!(!format!("{again:?} {again}").contains(TOKEN));
        let stored = store.get(mid(1)).unwrap().unwrap();
        assert!(!format!("{stored:?}").contains(TOKEN));
        assert_eq!(stored.body, TOKEN);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Append {
            id: u8,
            from: u8,
            to: u8,
            state: u8,
            sent: i64,
            body: String,
        },
        SetState {
            id: u8,
            state: u8,
        },
        Ack {
            to: u8,
            upto: u64,
        },
        Prune {
            now: i64,
        },
        Reopen,
    }

    fn state_of(code: u8) -> DeliveryState {
        [
            DeliveryState::Pending,
            DeliveryState::Delivered,
            DeliveryState::Acked,
            DeliveryState::Withheld,
            DeliveryState::Quarantined,
            DeliveryState::Expired,
        ][usize::from(code % 6)]
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..8, 0u8..3, 0u8..3, 0u8..6, 0i64..3000, "[a-z]{0,8}").prop_map(
                |(id, from, to, state, sent, body)| Op::Append {
                    id,
                    from,
                    to,
                    state,
                    sent,
                    body
                }
            ),
            (0u8..8, 0u8..6).prop_map(|(id, state)| Op::SetState { id, state }),
            (0u8..3, 0u64..12).prop_map(|(to, upto)| Op::Ack { to, upto }),
            (0i64..6000).prop_map(|now| Op::Prune { now }),
            Just(Op::Reopen),
        ]
    }

    struct Entry {
        id: u8,
        seq: u64,
        from: u8,
        to: u8,
        state: DeliveryState,
        sent: i64,
        size: usize,
    }

    fn name(n: u8) -> String {
        format!("p{n}")
    }

    fn check(store: &dyn MessageStore, model: &[Entry]) {
        for id in 0..8u8 {
            let found = store.get(mid(u128::from(id))).unwrap();
            match model.iter().find(|e| e.id == id) {
                None => assert_eq!(found, None),
                Some(entry) => {
                    let got = found.unwrap();
                    assert_eq!(got.seq.get(), entry.seq);
                    assert_eq!(got.state, entry.state);
                    assert_eq!(got.sent_at, at(entry.sent));
                }
            }
        }
        for to in 0..3u8 {
            let mut waiting: Vec<(u64, u8)> = Vec::new();
            for entry in model {
                if entry.to == to
                    && (entry.state == DeliveryState::Pending
                        || entry.state == DeliveryState::Delivered)
                {
                    waiting.push((entry.seq, entry.id));
                }
            }
            waiting.sort();
            let got: Vec<(u64, u8)> = store
                .inbox(sid(&name(to)), None, 1000)
                .unwrap()
                .iter()
                .map(|m| {
                    let id = (m.id.as_uuid().as_u128()) as u8;
                    (m.seq.get(), id)
                })
                .collect();
            assert_eq!(got, waiting);
            assert_eq!(store.unread(sid(&name(to))).unwrap(), waiting.len());
        }
        for from in 0..3u8 {
            let mut mine: Vec<&Entry> = model.iter().filter(|e| e.from == from).collect();
            mine.sort_by_key(|e| std::cmp::Reverse(e.seq));
            let mut expected: Vec<u64> = Vec::new();
            let mut total = 0;
            for entry in mine {
                total += entry.size;
                if total > 12 {
                    break;
                }
                expected.push(entry.seq);
            }
            expected.reverse();
            let got: Vec<u64> = store
                .outbound_window(sid(&name(from)), Recipient::Root, 12)
                .unwrap()
                .iter()
                .map(|m| m.seq.get())
                .collect();
            assert_eq!(got, expected);
        }
    }

    fn run(
        ops: &[Op],
        mut store: Box<dyn MessageStore>,
        mut reopen: impl FnMut(Box<dyn MessageStore>) -> Box<dyn MessageStore>,
    ) {
        let mut model: Vec<Entry> = Vec::new();
        let mut counter = 1u64;
        for op in ops {
            match op {
                Op::Append {
                    id,
                    from,
                    to,
                    state,
                    sent,
                    body,
                } => {
                    let record = message(
                        u128::from(*id),
                        &name(*from),
                        &name(*to),
                        state_of(*state),
                        *sent,
                        body,
                    );
                    let result = store.append(record);
                    if model.iter().any(|e| e.id == *id) {
                        assert!(matches!(result, Err(StoreError::Duplicate)));
                    } else {
                        assert_eq!(result.unwrap().get(), counter);
                        model.push(Entry {
                            id: *id,
                            seq: counter,
                            from: *from,
                            to: *to,
                            state: state_of(*state),
                            sent: *sent,
                            size: 1 + body.len(),
                        });
                        counter += 1;
                    }
                }
                Op::SetState { id, state } => {
                    let result = store.set_state(mid(u128::from(*id)), state_of(*state));
                    match model.iter_mut().find(|e| e.id == *id) {
                        None => assert!(matches!(result, Err(StoreError::NotFound))),
                        Some(entry) => {
                            result.unwrap();
                            entry.state = state_of(*state);
                        }
                    }
                }
                Op::Ack { to, upto } => {
                    store.ack(sid(&name(*to)), Seq::new(*upto)).unwrap();
                    for entry in model.iter_mut() {
                        if entry.to == *to
                            && entry.seq <= *upto
                            && entry.state == DeliveryState::Delivered
                        {
                            entry.state = DeliveryState::Acked;
                        }
                    }
                }
                Op::Prune { now } => {
                    let before = model.len();
                    model.retain(|e| {
                        let age = *now - e.sent;
                        match e.state {
                            DeliveryState::Quarantined => true,
                            DeliveryState::Acked => age <= 100,
                            _ => age <= 1000,
                        }
                    });
                    let removed = store.prune(at(*now), &policy()).unwrap();
                    assert_eq!(removed, before - model.len());
                }
                Op::Reopen => store = reopen(store),
            }
            check(store.as_ref(), &model);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(40))]

        #[test]
        fn the_memory_store_matches_the_model(ops in proptest::collection::vec(op_strategy(), 0..40)) {
            run(&ops, Box::new(InMemoryMessageStore::new()), |store| store);
        }

        #[test]
        fn replaying_the_jsonl_log_equals_the_model_at_every_reopen(ops in proptest::collection::vec(op_strategy(), 0..40)) {
            let fixture = Fixture::new();
            let store = Box::new(open(&fixture).unwrap());
            run(&ops, store, |old| {
                drop(old);
                Box::new(open(&fixture).unwrap())
            });
        }
    }
}
