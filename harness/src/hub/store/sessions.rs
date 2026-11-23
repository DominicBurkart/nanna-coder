use super::engine::{Engine, Table};
use super::journal::HubLock;
use super::{ActiveSession, Retention, SessionStore, StoreError};
use crate::hub::lineage::{RootId, SessionId};
use crate::hub::session::{SessionRecord, SessionStatus};
use crate::hub::summary::Summary;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum Record {
    Upsert {
        record: Box<SessionRecord>,
    },
    Status {
        id: SessionId,
        status: SessionStatus,
        at: DateTime<Utc>,
    },
    Note {
        id: SessionId,
        #[serde(default, deserialize_with = "crate::hub::session::lenient_note")]
        note: Option<Summary>,
    },
    Drop {
        id: SessionId,
    },
}

#[derive(Clone, Default)]
pub(super) struct SessionTable {
    by_id: BTreeMap<SessionId, SessionRecord>,
}

impl Table for SessionTable {
    type Record = Record;

    fn apply(&mut self, record: &Record) -> Result<(), StoreError> {
        match record {
            Record::Upsert { record } => {
                self.by_id.insert(record.id.clone(), (**record).clone());
            }
            Record::Status { id, status, at } => {
                let found = self.by_id.get_mut(id).ok_or(StoreError::NotFound)?;
                found.status = *status;
                found.last_activity = *at;
                found.ended_at = (!status.is_active()).then_some(*at);
            }
            Record::Note { id, note } => {
                let found = self.by_id.get_mut(id).ok_or(StoreError::NotFound)?;
                found.note = note.clone();
            }
            Record::Drop { id } => {
                self.by_id.remove(id).ok_or(StoreError::NotFound)?;
            }
        }
        Ok(())
    }

    fn records(&self) -> Vec<Record> {
        self.by_id
            .values()
            .map(|record| Record::Upsert {
                record: Box::new(record.clone()),
            })
            .collect()
    }
}

impl SessionTable {
    fn active(&self) -> Vec<ActiveSession> {
        let mut listed = Vec::new();
        for record in self.by_id.values() {
            if record.status.is_active() {
                listed.push(ActiveSession {
                    id: record.id.clone(),
                    status: record.status,
                    last_activity: record.last_activity,
                });
            }
        }
        listed
    }

    fn expired_ids(&self, now: DateTime<Utc>, policy: &Retention) -> Vec<SessionId> {
        let mut roots: HashMap<&RootId, (bool, DateTime<Utc>, Vec<&SessionId>)> = HashMap::new();
        for record in self.by_id.values() {
            let entry = roots.entry(record.lineage.root()).or_insert((
                true,
                record.last_activity,
                Vec::new(),
            ));
            entry.0 &= !record.status.is_active();
            entry.1 = entry.1.max(record.last_activity);
            entry.2.push(&record.id);
        }
        roots
            .into_values()
            .filter(|(finished, latest, _)| {
                *finished && now.signed_duration_since(*latest) > policy.completed_session_ttl
            })
            .flat_map(|(_, _, ids)| ids.into_iter().cloned())
            .collect()
    }
}

macro_rules! session_store_impl {
    ($ty:ty) => {
        impl SessionStore for $ty {
            fn upsert(&self, rec: SessionRecord) -> Result<(), StoreError> {
                self.engine.commit(|_| {
                    Ok((
                        (),
                        vec![Record::Upsert {
                            record: Box::new(rec),
                        }],
                    ))
                })
            }

            fn get(&self, id: SessionId) -> Result<Option<SessionRecord>, StoreError> {
                Ok(self.engine.read(|table| table.by_id.get(&id).cloned()))
            }

            fn in_root(&self, root: &RootId) -> Result<Vec<SessionRecord>, StoreError> {
                let mut found: Vec<SessionRecord> = self.engine.read(|table| {
                    table
                        .by_id
                        .values()
                        .filter(|record| record.lineage.root() == root)
                        .cloned()
                        .collect()
                });
                found.sort_by(|a, b| {
                    a.created_at
                        .cmp(&b.created_at)
                        .then_with(|| a.id.cmp(&b.id))
                });
                Ok(found)
            }

            fn update(
                &self,
                id: SessionId,
                change: Box<dyn FnOnce(SessionRecord) -> Option<SessionRecord> + '_>,
            ) -> Result<(), StoreError> {
                self.engine.commit(|table| {
                    let current = table.by_id.get(&id).ok_or(StoreError::NotFound)?;
                    let kept = change(current.clone()).map(|next| Record::Upsert {
                        record: Box::new(next),
                    });
                    Ok(((), kept.into_iter().collect()))
                })
            }

            fn set_status(
                &self,
                id: SessionId,
                s: SessionStatus,
                at: DateTime<Utc>,
            ) -> Result<(), StoreError> {
                self.engine
                    .commit(|_| Ok(((), vec![Record::Status { id, status: s, at }])))
            }

            fn set_note(&self, id: SessionId, note: Summary) -> Result<(), StoreError> {
                self.engine.commit(|_| {
                    Ok((
                        (),
                        vec![Record::Note {
                            id,
                            note: Some(note),
                        }],
                    ))
                })
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

            fn active(&self) -> Result<Vec<ActiveSession>, StoreError> {
                Ok(self.engine.read(SessionTable::active))
            }
        }
    };
}

/// Volatile [`SessionStore`] for tests and for hubs that do not persist.
///
/// ```
/// use harness::hub::store::{InMemorySessionStore, SessionStore};
/// use harness::hub::lineage::SessionId;
/// use harness::task::TaskId;
///
/// let store = InMemorySessionStore::new();
/// assert!(store.get(SessionId::from_task(TaskId("none".into()))).unwrap().is_none());
/// ```
pub struct InMemorySessionStore {
    engine: Engine<SessionTable>,
}

impl InMemorySessionStore {
    /// An empty store.
    ///
    /// ```
    /// let store = harness::hub::store::InMemorySessionStore::new();
    /// let _same = harness::hub::store::InMemorySessionStore::default();
    /// drop(store);
    /// ```
    pub fn new() -> Self {
        Self {
            engine: Engine::memory(),
        }
    }
}

impl Default for InMemorySessionStore {
    fn default() -> Self {
        Self::new()
    }
}

session_store_impl!(InMemorySessionStore);

/// Durable [`SessionStore`]: one JSON record per line in a file created with
/// mode `0600`, replayed and compacted on [`open`](Self::open). Every change
/// is applied to a clone, appended to the file, and only then adopted, so a
/// failed write leaves the table unchanged. The caller holds the
/// [`HubLock`] of the directory.
///
/// ```
/// use chrono::Utc;
/// use harness::effects::EffectClass;
/// use harness::hub::lineage::{Lineage, RootId, SessionId};
/// use harness::hub::session::{ReadScope, SessionKind, SessionRecord, SessionStatus};
/// use harness::hub::store::{HubLock, JsonlSessionStore, SessionStore};
/// use harness::identity::DevLoop;
/// use harness::task::TaskId;
///
/// let dir = tempfile::tempdir().unwrap();
/// let lock = HubLock::acquire(dir.path()).unwrap();
/// let path = dir.path().join("sessions.jsonl");
/// let now = Utc::now();
/// let id = SessionId::from_task(TaskId("t-1".into()));
/// let record = SessionRecord {
///     id: id.clone(),
///     lineage: Lineage::root_child(RootId::mcp(uuid::Uuid::nil())),
///     kind: SessionKind::Agent,
///     identity: "coder".into(),
///     role: "writes code".into(),
///     dev_loop: DevLoop::Inner,
///     max_effect: EffectClass::Workspace,
///     tools: vec![],
///     write_paths: vec![],
///     read_paths: ReadScope::Unrestricted,
///     subtask: "fix".into(),
///     note: None,
///     result_summary: None,
///     touched_paths: vec![],
///     status: SessionStatus::Running,
///     created_at: now,
///     last_activity: now,
///     ended_at: None,
/// };
/// JsonlSessionStore::open(&path, &lock).unwrap().upsert(record.clone()).unwrap();
/// let reopened = JsonlSessionStore::open(&path, &lock).unwrap();
/// assert_eq!(reopened.get(id).unwrap(), Some(record));
/// ```
pub struct JsonlSessionStore {
    engine: Engine<SessionTable>,
}

impl JsonlSessionStore {
    /// Replay `path` (creating it, mode `0600`, when absent), ignore a
    /// truncated final line, fail with [`StoreError::Corrupt`] on any other
    /// unreadable line, and rewrite the file with only the live records.
    /// `lock` must protect the directory that holds `path`.
    ///
    /// ```
    /// use harness::hub::store::{HubLock, JsonlSessionStore};
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let lock = HubLock::acquire(dir.path()).unwrap();
    /// assert!(JsonlSessionStore::open(&dir.path().join("sessions.jsonl"), &lock).is_ok());
    /// assert!(JsonlSessionStore::open(&dir.path().join("sub").join("s.jsonl"), &lock).is_err());
    /// ```
    pub fn open(path: &Path, lock: &Arc<HubLock>) -> Result<Self, StoreError> {
        Ok(Self {
            engine: Engine::open(path, lock)?,
        })
    }
}

session_store_impl!(JsonlSessionStore);

#[cfg(test)]
mod tests {
    use super::super::contract::*;
    use super::super::journal::Journal;
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::Ordering;

    fn note(text: &str) -> Summary {
        Summary::compose("a", "running", text)
    }

    mod memory {
        use super::*;
        session_contract_tests!(((), InMemorySessionStore::new()));
    }

    mod jsonl {
        use super::*;
        session_contract_tests!({
            let fixture = Fixture::new();
            let store =
                JsonlSessionStore::open(&fixture.path("sessions.jsonl"), &fixture.lock).unwrap();
            (fixture, store)
        });
    }

    fn open(fixture: &Fixture) -> Result<JsonlSessionStore, StoreError> {
        JsonlSessionStore::open(&fixture.path("sessions.jsonl"), &fixture.lock)
    }

    fn lines(fixture: &Fixture) -> Vec<String> {
        std::fs::read_to_string(fixture.path("sessions.jsonl"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn state_survives_a_reopen() {
        let fixture = Fixture::new();
        let r = root(1);
        let store = open(&fixture).unwrap();
        store
            .upsert(session("a", &r, SessionStatus::Running, 0, 0))
            .unwrap();
        store
            .upsert(session("b", &r, SessionStatus::Running, 1, 1))
            .unwrap();
        store
            .set_status(sid("a"), SessionStatus::Completed, at(9))
            .unwrap();
        store.set_note(sid("a"), note("remember")).unwrap();
        let before = store.in_root(&r).unwrap();
        drop(store);
        let reopened = open(&fixture).unwrap();
        assert_eq!(reopened.in_root(&r).unwrap(), before);
        assert_eq!(
            reopened.get(sid("a")).unwrap().unwrap().note,
            Some(note("remember"))
        );
    }

    #[test]
    fn the_active_listing_survives_a_reopen_and_follows_the_replayed_status() {
        let fixture = Fixture::new();
        let r = root(1);
        let store = open(&fixture).unwrap();
        store
            .upsert(session("a", &r, SessionStatus::Running, 0, 4))
            .unwrap();
        store
            .upsert(session("b", &r, SessionStatus::Pending, 0, 5))
            .unwrap();
        store
            .set_status(sid("b"), SessionStatus::Failed, at(9))
            .unwrap();
        let before = store.active().unwrap();
        assert_eq!(before.len(), 1);
        drop(store);
        let reopened = open(&fixture).unwrap();
        assert_eq!(reopened.active().unwrap(), before);
    }

    #[test]
    fn a_pruned_root_stays_pruned_after_a_reopen() {
        let fixture = Fixture::new();
        let r = root(1);
        let store = open(&fixture).unwrap();
        store
            .upsert(session("a", &r, SessionStatus::Completed, 0, 0))
            .unwrap();
        store
            .upsert(session("b", &root(2), SessionStatus::Running, 0, 0))
            .unwrap();
        assert_eq!(store.prune(at(5000), &policy()).unwrap(), 1);
        drop(store);
        let reopened = open(&fixture).unwrap();
        assert_eq!(reopened.get(sid("a")).unwrap(), None);
        assert!(reopened.get(sid("b")).unwrap().is_some());
    }

    #[test]
    fn open_compacts_the_log_to_one_line_per_live_session() {
        let fixture = Fixture::new();
        let r = root(1);
        let store = open(&fixture).unwrap();
        for n in 0..5 {
            store
                .upsert(session("a", &r, SessionStatus::Running, 0, n))
                .unwrap();
        }
        store
            .set_status(sid("a"), SessionStatus::Failed, at(7))
            .unwrap();
        assert_eq!(lines(&fixture).len(), 6);
        drop(store);
        let reopened = open(&fixture).unwrap();
        assert_eq!(lines(&fixture).len(), 1);
        assert_eq!(
            reopened.get(sid("a")).unwrap().unwrap().status,
            SessionStatus::Failed
        );
    }

    #[cfg(unix)]
    #[test]
    fn files_are_created_with_mode_0600_and_a_wide_file_is_narrowed() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: std::path::PathBuf| {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        };
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        store
            .upsert(session("a", &root(1), SessionStatus::Running, 0, 0))
            .unwrap();
        assert_eq!(mode(fixture.path("sessions.jsonl")), 0o600);
        assert_eq!(mode(fixture.path("hub.lock")), 0o600);
        drop(store);
        std::fs::set_permissions(
            fixture.path("sessions.jsonl"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        std::fs::write(fixture.path("sessions.jsonl.tmp"), b"stale").unwrap();
        std::fs::set_permissions(
            fixture.path("sessions.jsonl.tmp"),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        let _reopened = open(&fixture).unwrap();
        assert_eq!(mode(fixture.path("sessions.jsonl")), 0o600);
    }

    #[test]
    fn a_journal_written_before_summary_notes_replays_and_drops_an_invalid_note() {
        let fixture = Fixture::new();
        let r = root(1);
        let store = open(&fixture).unwrap();
        store
            .upsert(session("a", &r, SessionStatus::Running, 0, 0))
            .unwrap();
        store.set_note(sid("a"), note("kept")).unwrap();
        drop(store);
        let mut text = std::fs::read_to_string(fixture.path("sessions.jsonl")).unwrap();
        text.push_str(&format!(
            "{{\"op\":\"note\",\"id\":\"a\",\"note\":\"{TOKEN}   {TOKEN}\"}}\n"
        ));
        std::fs::write(fixture.path("sessions.jsonl"), text).unwrap();
        let reopened = open(&fixture).unwrap();
        assert_eq!(reopened.get(sid("a")).unwrap().unwrap().note, None);
        assert!(!std::fs::read_to_string(fixture.path("sessions.jsonl"))
            .unwrap()
            .contains(TOKEN));
    }

    #[test]
    fn a_journal_written_before_read_paths_replays_as_unknown_and_compaction_names_it() {
        let fixture = Fixture::new();
        let r = root(1);
        let store = open(&fixture).unwrap();
        let mut record = session("a", &r, SessionStatus::Running, 0, 0);
        record.read_paths = crate::hub::session::ReadScope::Unrestricted;
        store.upsert(record).unwrap();
        drop(store);
        let legacy: Vec<String> = lines(&fixture)
            .iter()
            .map(|line| line.replace("\"read_paths\":\"unrestricted\",", ""))
            .collect();
        assert!(legacy.iter().all(|line| !line.contains("read_paths")));
        std::fs::write(fixture.path("sessions.jsonl"), legacy.join("\n") + "\n").unwrap();
        let reopened = open(&fixture).unwrap();
        let loaded = reopened.get(sid("a")).unwrap().unwrap();
        assert_eq!(loaded.read_paths, crate::hub::session::ReadScope::Unknown);
        assert!(lines(&fixture)
            .iter()
            .all(|line| line.contains("\"read_paths\":\"unknown\"")));
    }

    #[test]
    fn a_truncated_last_line_is_ignored_with_a_warning_and_dropped_on_compaction() {
        let fixture = Fixture::new();
        let r = root(1);
        let store = open(&fixture).unwrap();
        store
            .upsert(session("a", &r, SessionStatus::Running, 0, 0))
            .unwrap();
        drop(store);
        let mut text = std::fs::read_to_string(fixture.path("sessions.jsonl")).unwrap();
        text.push_str(&format!(r#"{{"op":"note","id":"a","note":"{TOKEN}"#));
        std::fs::write(fixture.path("sessions.jsonl"), text).unwrap();
        let (reopened, log) = logged(|| open(&fixture).unwrap());
        assert!(log.contains("truncated final record"), "{log}");
        assert!(!log.contains(TOKEN));
        assert_eq!(reopened.get(sid("a")).unwrap().unwrap().note, None);
        assert_eq!(lines(&fixture).len(), 1);
        assert!(!std::fs::read_to_string(fixture.path("sessions.jsonl"))
            .unwrap()
            .contains(TOKEN));
    }

    #[test]
    fn a_corrupt_middle_line_fails_closed_and_leaves_the_file_untouched() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        let r = root(1);
        store
            .upsert(session("a", &r, SessionStatus::Running, 0, 0))
            .unwrap();
        store
            .upsert(session("b", &r, SessionStatus::Running, 0, 0))
            .unwrap();
        drop(store);
        let good = lines(&fixture);
        let corrupt = format!(
            "{}\n{{\"op\":\"nope\",\"x\":\"{TOKEN}\"}}\n{}\n",
            good[0], good[1]
        );
        std::fs::write(fixture.path("sessions.jsonl"), &corrupt).unwrap();
        let error = open(&fixture).err().unwrap();
        assert!(matches!(error, StoreError::Corrupt(2)), "{error:?}");
        assert!(!format!("{error:?} {error}").contains(TOKEN));
        assert_eq!(
            std::fs::read_to_string(fixture.path("sessions.jsonl")).unwrap(),
            corrupt
        );
    }

    #[test]
    fn an_unreadable_final_line_that_was_terminated_is_corrupt_not_truncated() {
        let fixture = Fixture::new();
        std::fs::write(fixture.path("sessions.jsonl"), "{\"op\":\n").unwrap();
        assert!(matches!(open(&fixture).err(), Some(StoreError::Corrupt(1))));
    }

    #[test]
    fn a_record_that_does_not_apply_is_corrupt() {
        let fixture = Fixture::new();
        let text = r#"{"op":"drop","id":"ghost"}"#;
        std::fs::write(fixture.path("sessions.jsonl"), format!("\n{text}\n")).unwrap();
        assert!(matches!(open(&fixture).err(), Some(StoreError::Corrupt(2))));
    }

    #[test]
    fn an_inapplicable_record_between_good_ones_fails_closed_at_its_own_line() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        let r = root(1);
        store
            .upsert(session("a", &r, SessionStatus::Running, 0, 0))
            .unwrap();
        store
            .upsert(session("b", &r, SessionStatus::Running, 1, 1))
            .unwrap();
        drop(store);
        let good = lines(&fixture);
        let ghost = r#"{"op":"status","id":"ghost","status":"failed","at":"2023-11-14T22:13:20Z"}"#;
        let broken = format!("{}\n{ghost}\n{}\n", good[0], good[1]);
        std::fs::write(fixture.path("sessions.jsonl"), &broken).unwrap();
        assert!(matches!(open(&fixture).err(), Some(StoreError::Corrupt(2))));
        assert_eq!(
            std::fs::read_to_string(fixture.path("sessions.jsonl")).unwrap(),
            broken
        );
    }

    #[test]
    fn a_path_that_cannot_be_read_is_an_io_error() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.path("sessions.jsonl")).unwrap();
        assert!(matches!(open(&fixture).err(), Some(StoreError::Io(_))));
    }

    #[test]
    fn a_second_hub_lock_on_the_directory_is_refused_while_a_store_is_open() {
        let fixture = Fixture::new();
        let _store = open(&fixture).unwrap();
        assert!(matches!(
            super::super::HubLock::acquire(fixture.dir.path()),
            Err(StoreError::Locked)
        ));
    }

    #[test]
    fn the_lock_keeps_the_directory_claimed_for_as_long_as_the_store_lives() {
        let fixture = Fixture::new();
        let store = open(&fixture).unwrap();
        let Fixture { dir, lock } = fixture;
        drop(lock);
        assert!(matches!(
            super::super::HubLock::acquire(dir.path()),
            Err(StoreError::Locked)
        ));
        drop(store);
        assert!(super::super::HubLock::acquire(dir.path()).is_ok());
    }

    #[test]
    fn a_store_outside_the_locked_directory_is_refused() {
        let fixture = Fixture::new();
        let other = tempfile::tempdir().unwrap();
        let result = JsonlSessionStore::open(&other.path().join("sessions.jsonl"), &fixture.lock);
        assert!(matches!(result.err(), Some(StoreError::Io(_))));
    }

    fn flaky_store() -> (
        InMemorySessionStore,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ) {
        let (journal, fail, written) = flaky();
        let store = InMemorySessionStore {
            engine: Engine::with_journal(Box::new(journal) as Box<dyn Journal>),
        };
        (store, fail, written)
    }

    #[test]
    fn a_failed_disk_write_leaves_the_table_unchanged_for_every_operation() {
        let (store, fail, written) = flaky_store();
        let r = root(1);
        store
            .upsert(session("a", &r, SessionStatus::Running, 0, 0))
            .unwrap();
        let before = store.in_root(&r).unwrap();
        let written_before = written.lock().unwrap().clone();
        fail.store(true, Ordering::SeqCst);
        assert!(matches!(
            store.upsert(session("b", &r, SessionStatus::Running, 0, 0)),
            Err(StoreError::Io(_))
        ));
        assert!(matches!(
            store.set_status(sid("a"), SessionStatus::Completed, at(5)),
            Err(StoreError::Io(_))
        ));
        assert!(matches!(
            store.set_note(sid("a"), note("n")),
            Err(StoreError::Io(_))
        ));
        assert!(matches!(
            store.prune(at(100_000), &policy()),
            Err(StoreError::Io(_))
        ));
        assert_eq!(store.in_root(&r).unwrap(), before);
        assert_eq!(*written.lock().unwrap(), written_before);
        fail.store(false, Ordering::SeqCst);
        store
            .set_status(sid("a"), SessionStatus::Completed, at(5))
            .unwrap();
        assert_eq!(
            store.get(sid("a")).unwrap().unwrap().status,
            SessionStatus::Completed
        );
    }

    #[test]
    fn the_journal_gets_exactly_the_replayable_records() {
        let (store, _fail, written) = flaky_store();
        store
            .upsert(session("a", &root(1), SessionStatus::Running, 0, 0))
            .unwrap();
        store
            .set_status(sid("a"), SessionStatus::Failed, at(3))
            .unwrap();
        let text = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        let ops: Vec<String> = text
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["op"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(ops, ["upsert", "status"]);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Upsert {
            n: u8,
            root: u8,
            status: u8,
            created: i64,
            last: i64,
        },
        Status {
            n: u8,
            status: u8,
            at: i64,
        },
        Note {
            n: u8,
            note: String,
        },
        Prune {
            now: i64,
        },
        Reopen,
    }

    fn status_of(code: u8) -> SessionStatus {
        [
            SessionStatus::Pending,
            SessionStatus::Running,
            SessionStatus::WaitingOnMessages,
            SessionStatus::Completed,
            SessionStatus::Failed,
            SessionStatus::Cancelled,
        ][usize::from(code % 6)]
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..6, 0u8..3, 0u8..6, 0i64..3000, 0i64..3000).prop_map(
                |(n, root, status, created, last)| Op::Upsert {
                    n,
                    root,
                    status,
                    created,
                    last
                }
            ),
            (0u8..6, 0u8..6, 0i64..3000).prop_map(|(n, status, at)| Op::Status { n, status, at }),
            (0u8..6, "[a-z]{0,6}").prop_map(|(n, note)| Op::Note { n, note }),
            (0i64..6000).prop_map(|now| Op::Prune { now }),
            Just(Op::Reopen),
        ]
    }

    struct ModelEntry {
        n: u8,
        root: u8,
        status: SessionStatus,
        created: i64,
        last: i64,
        ended: Option<i64>,
        note: Option<Summary>,
    }

    fn model_prune(model: &mut Vec<ModelEntry>, now: i64) -> usize {
        let mut removed = 0;
        for r in 0..3u8 {
            let members: Vec<&ModelEntry> = model.iter().filter(|e| e.root == r).collect();
            if members.is_empty() {
                continue;
            }
            let mut all_done = true;
            let mut newest = i64::MIN;
            for member in &members {
                if matches!(
                    member.status,
                    SessionStatus::Pending
                        | SessionStatus::Running
                        | SessionStatus::WaitingOnMessages
                ) {
                    all_done = false;
                }
                if member.last > newest {
                    newest = member.last;
                }
            }
            if all_done && now - newest > 1000 {
                removed += members.len();
                model.retain(|e| e.root != r);
            }
        }
        removed
    }

    fn check_against_model(store: &dyn SessionStore, model: &[ModelEntry]) {
        for n in 0..6u8 {
            let name = format!("s{n}");
            let found = store.get(sid(&name)).unwrap();
            match model.iter().find(|e| e.n == n) {
                None => assert_eq!(found, None, "{name}"),
                Some(entry) => {
                    let got = found.unwrap_or_else(|| panic!("{name} missing"));
                    assert_eq!(got.status, entry.status, "{name}");
                    assert_eq!(got.lineage.root(), &root(u128::from(entry.root)), "{name}");
                    assert_eq!(got.created_at, at(entry.created), "{name}");
                    assert_eq!(got.last_activity, at(entry.last), "{name}");
                    assert_eq!(got.ended_at, entry.ended.map(at), "{name}");
                    assert_eq!(got.note, entry.note, "{name}");
                }
            }
        }
        for r in 0..3u8 {
            let mut expected: Vec<(i64, String)> = model
                .iter()
                .filter(|e| e.root == r)
                .map(|e| (e.created, format!("s{}", e.n)))
                .collect();
            expected.sort();
            let got: Vec<(i64, String)> = store
                .in_root(&root(u128::from(r)))
                .unwrap()
                .iter()
                .map(|s| {
                    (
                        s.created_at.timestamp() - at(0).timestamp(),
                        s.id.to_string(),
                    )
                })
                .collect();
            assert_eq!(got, expected, "root {r}");
        }
    }

    fn run_model(
        ops: &[Op],
        mut store: Box<dyn SessionStore>,
        mut reopen: impl FnMut(Box<dyn SessionStore>) -> Box<dyn SessionStore>,
    ) {
        let mut model: Vec<ModelEntry> = Vec::new();
        for op in ops {
            match op {
                Op::Upsert {
                    n,
                    root: r,
                    status,
                    created,
                    last,
                } => {
                    let name = format!("s{n}");
                    let mut record = session(
                        &name,
                        &root(u128::from(*r)),
                        status_of(*status),
                        *created,
                        *last,
                    );
                    record.note = None;
                    store.upsert(record).unwrap();
                    model.retain(|e| e.n != *n);
                    model.push(ModelEntry {
                        n: *n,
                        root: *r,
                        status: status_of(*status),
                        created: *created,
                        last: *last,
                        ended: None,
                        note: None,
                    });
                }
                Op::Status {
                    n,
                    status,
                    at: when,
                } => {
                    let result =
                        store.set_status(sid(&format!("s{n}")), status_of(*status), at(*when));
                    match model.iter_mut().find(|e| e.n == *n) {
                        None => assert!(matches!(result, Err(StoreError::NotFound))),
                        Some(entry) => {
                            result.unwrap();
                            entry.status = status_of(*status);
                            entry.last = *when;
                            entry.ended = match entry.status {
                                SessionStatus::Completed
                                | SessionStatus::Failed
                                | SessionStatus::Cancelled => Some(*when),
                                _ => None,
                            };
                        }
                    }
                }
                Op::Note { n, note: note_text } => {
                    let summary = note(note_text);
                    let result = store.set_note(sid(&format!("s{n}")), summary.clone());
                    match model.iter_mut().find(|e| e.n == *n) {
                        None => assert!(matches!(result, Err(StoreError::NotFound))),
                        Some(entry) => {
                            result.unwrap();
                            entry.note = Some(summary);
                        }
                    }
                }
                Op::Prune { now } => {
                    let removed = store.prune(at(*now), &policy()).unwrap();
                    assert_eq!(removed, model_prune(&mut model, *now));
                }
                Op::Reopen => store = reopen(store),
            }
            check_against_model(store.as_ref(), &model);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(40))]

        #[test]
        fn the_memory_store_matches_the_model(ops in proptest::collection::vec(op_strategy(), 0..40)) {
            run_model(&ops, Box::new(InMemorySessionStore::new()), |store| store);
        }

        #[test]
        fn replaying_the_jsonl_log_equals_the_model_at_every_reopen(ops in proptest::collection::vec(op_strategy(), 0..40)) {
            let fixture = Fixture::new();
            let store = Box::new(open(&fixture).unwrap());
            run_model(&ops, store, |old| {
                drop(old);
                Box::new(open(&fixture).unwrap())
            });
        }
    }
}
