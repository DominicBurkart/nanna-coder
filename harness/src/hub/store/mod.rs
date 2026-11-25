#[cfg(test)]
#[macro_use]
mod contract;
mod activity;
mod activity_write;
mod engine;
mod journal;
mod messages;
mod sessions;

pub use activity::{
    activity_path_from, ActivityEntry, ActivityStore, Direction, InMemoryActivityStore,
    JsonlActivityStore, ACTIVITY_PATH_ENV, ACTIVITY_PER_SESSION_MAX, ACTIVITY_TOTAL_BYTES_MAX,
    ENTRY_OVERHEAD,
};
pub use activity_write::{
    ArgsSummary, MAX_ARGS_CHARS, MAX_SUBJECT_CHARS, MAX_TOOL_CHARS, MAX_TURN_BYTES,
};
pub use journal::HubLock;
pub use messages::{InMemoryMessageStore, JsonlMessageStore};
pub use sessions::{InMemorySessionStore, JsonlSessionStore};

use super::lineage::{RootId, SessionId};
use super::message::{DeliveryState, MessageId, Recipient, Release, Seq, StoredMessage};
use super::session::{SessionRecord, SessionStatus};
use super::summary::Summary;
use chrono::{DateTime, Duration, Utc};
use std::io;
use std::path::{Path, PathBuf};

/// Environment variable overriding the session store location.
pub const SESSIONS_PATH_ENV: &str = "NANNA_SESSIONS_PATH";

/// Environment variable overriding the message store location.
pub const MESSAGES_PATH_ENV: &str = "NANNA_MESSAGES_PATH";

/// Why a store operation failed. No variant carries record content: an error
/// can reach logs and tool output, and message bodies must not.
///
/// ```
/// use harness::hub::store::StoreError;
///
/// assert_eq!(StoreError::Corrupt(3).to_string(), "corrupt record at line 3");
/// assert_eq!(StoreError::Locked.to_string(), "hub state locked by another nanna process");
/// assert_eq!(StoreError::NotFound.to_string(), "no such record");
/// assert_eq!(StoreError::Duplicate.to_string(), "record already exists");
/// assert_eq!(StoreError::Conflict.to_string(), "record is not in the expected state");
/// ```
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("corrupt record at line {0}")]
    Corrupt(usize),
    #[error("hub state locked by another nanna process")]
    Locked,
    #[error("no such record")]
    NotFound,
    #[error("record already exists")]
    Duplicate,
    #[error("record is not in the expected state")]
    Conflict,
}

/// How long finished state is kept. Quarantined messages are never pruned:
/// they stay until a human resolves them.
///
/// ```
/// use chrono::Duration;
/// use harness::hub::store::Retention;
///
/// let policy = Retention::default();
/// assert_eq!(policy.completed_session_ttl, Duration::days(7));
/// assert_eq!(policy.acked_message_ttl, Duration::hours(24));
/// assert_eq!(policy.unacked_message_ttl, Duration::days(7));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    pub completed_session_ttl: Duration,
    pub acked_message_ttl: Duration,
    pub unacked_message_ttl: Duration,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            completed_session_ttl: Duration::days(7),
            acked_message_ttl: Duration::hours(24),
            unacked_message_ttl: Duration::days(7),
        }
    }
}

/// The part of a non-terminal session record that the orphan sweep reads: no agent-authored
/// text, so it is cheap to copy and safe to log.
///
/// ```
/// use chrono::Utc;
/// use harness::hub::lineage::SessionId;
/// use harness::hub::session::SessionStatus;
/// use harness::hub::store::ActiveSession;
///
/// let listed = ActiveSession {
///     id: SessionId::from_task(harness::task::TaskId("t".into())),
///     status: SessionStatus::Running,
///     last_activity: Utc::now(),
/// };
/// assert!(listed.status.is_active());
/// assert!(format!("{listed:?}").contains("Running"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSession {
    pub id: SessionId,
    pub status: SessionStatus,
    pub last_activity: DateTime<Utc>,
}

/// Storage for [`SessionRecord`]s. `prune` removes a whole root once every
/// session in it is terminal and the root's latest `last_activity` is older
/// than `completed_session_ttl`; it returns the number of sessions removed.
///
/// ```
/// use harness::hub::store::{InMemorySessionStore, SessionStore};
///
/// fn count_in_memory(store: &dyn SessionStore) {
///     let now = chrono::Utc::now();
///     assert_eq!(store.prune(now, &Default::default()).unwrap(), 0);
/// }
/// count_in_memory(&InMemorySessionStore::new());
/// ```
pub trait SessionStore: Send + Sync {
    /// Insert the record, or replace the one with the same id.
    ///
    /// ```
    /// use harness::hub::store::{InMemorySessionStore, SessionStore};
    /// let store = InMemorySessionStore::new();
    /// let _ = store.in_root(&harness::hub::lineage::RootId::mcp(uuid::Uuid::nil()));
    /// ```
    fn upsert(&self, rec: SessionRecord) -> Result<(), StoreError>;
    /// The record with this id, if any.
    ///
    /// ```
    /// use harness::hub::store::{InMemorySessionStore, SessionStore};
    /// let id = harness::hub::lineage::SessionId::from_task(harness::task::TaskId("x".into()));
    /// assert!(InMemorySessionStore::new().get(id).unwrap().is_none());
    /// ```
    fn get(&self, id: SessionId) -> Result<Option<SessionRecord>, StoreError>;
    /// Replace the record with this id by what `change` returns for it, reading and writing under
    /// one hold of the store's lock, so no other writer can run between the two. `change` returning
    /// `None` leaves the record as it was. Unknown id: `NotFound`.
    ///
    /// ```
    /// use harness::hub::store::{InMemorySessionStore, SessionStore, StoreError};
    /// let id = harness::hub::lineage::SessionId::from_task(harness::task::TaskId("x".into()));
    /// let store = InMemorySessionStore::new();
    /// assert!(matches!(store.update(id, Box::new(|_| None)), Err(StoreError::NotFound)));
    /// ```
    fn update(
        &self,
        id: SessionId,
        change: Box<dyn FnOnce(SessionRecord) -> Option<SessionRecord> + '_>,
    ) -> Result<(), StoreError>;
    /// Every session in `root`, ordered by `created_at` then id.
    ///
    /// ```
    /// use harness::hub::store::{InMemorySessionStore, SessionStore};
    /// let root = harness::hub::lineage::RootId::mcp(uuid::Uuid::nil());
    /// assert!(InMemorySessionStore::new().in_root(&root).unwrap().is_empty());
    /// ```
    fn in_root(&self, root: &RootId) -> Result<Vec<SessionRecord>, StoreError>;
    /// Set the status and `last_activity` to `at`; `ended_at` becomes `at`
    /// for a terminal status and `None` otherwise. Unknown id: `NotFound`.
    ///
    /// ```
    /// use harness::hub::session::SessionStatus;
    /// use harness::hub::store::{InMemorySessionStore, SessionStore, StoreError};
    /// let id = harness::hub::lineage::SessionId::from_task(harness::task::TaskId("x".into()));
    /// let result = InMemorySessionStore::new().set_status(id, SessionStatus::Running, chrono::Utc::now());
    /// assert!(matches!(result, Err(StoreError::NotFound)));
    /// ```
    fn set_status(
        &self,
        id: SessionId,
        s: SessionStatus,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError>;
    /// Replace the session's note. Unknown id: `NotFound`.
    ///
    /// ```
    /// use harness::hub::store::{InMemorySessionStore, SessionStore, StoreError};
    /// let id = harness::hub::lineage::SessionId::from_task(harness::task::TaskId("x".into()));
    /// let note = harness::hub::summary::Summary::compose("a", "running", "n");
    /// let result = InMemorySessionStore::new().set_note(id, note);
    /// assert!(matches!(result, Err(StoreError::NotFound)));
    /// ```
    fn set_note(&self, id: SessionId, note: Summary) -> Result<(), StoreError>;
    /// Remove expired roots; returns the number of sessions removed.
    ///
    /// ```
    /// use harness::hub::store::{InMemorySessionStore, SessionStore};
    /// let removed = InMemorySessionStore::new().prune(chrono::Utc::now(), &Default::default());
    /// assert_eq!(removed.unwrap(), 0);
    /// ```
    fn prune(&self, now: DateTime<Utc>, policy: &Retention) -> Result<usize, StoreError>;
    /// Every record that is not terminal (`Pending`, `Running` or `WaitingOnMessages`), ordered
    /// by id, as `(id, status, last_activity)`. A read failure is an error, never an empty list.
    ///
    /// ```
    /// use harness::hub::store::{InMemorySessionStore, SessionStore};
    /// assert!(InMemorySessionStore::new().active().unwrap().is_empty());
    /// ```
    fn active(&self) -> Result<Vec<ActiveSession>, StoreError>;
}

/// Storage for [`StoredMessage`]s. The store assigns `seq` on `append`
/// (starting at 1, never reused) and ignores the value the caller put in the
/// record. `inbox` and `unread` see only `Pending` and `Delivered` messages;
/// `Withheld`, `Quarantined`, `Expired` and `Acked` ones are invisible to
/// them. `prune` measures age from `sent_at`: `Acked` messages use
/// `acked_message_ttl`, `Quarantined` ones are kept, every other state uses
/// `unacked_message_ttl`.
///
/// ```
/// use harness::hub::store::{InMemoryMessageStore, MessageStore};
///
/// fn prune_empty(store: &dyn MessageStore) {
///     assert_eq!(store.prune(chrono::Utc::now(), &Default::default()).unwrap(), 0);
/// }
/// prune_empty(&InMemoryMessageStore::new());
/// ```
pub trait MessageStore: Send + Sync {
    /// Store the message and return the `Seq` it was given. A reused
    /// `MessageId` is `Duplicate`.
    ///
    /// ```
    /// use harness::hub::store::{InMemoryMessageStore, MessageStore};
    /// let store = InMemoryMessageStore::new();
    /// assert!(store.get(harness::hub::message::MessageId::new()).unwrap().is_none());
    /// ```
    fn append(&self, m: StoredMessage) -> Result<Seq, StoreError>;
    /// The message with this id, in any state.
    ///
    /// ```
    /// use harness::hub::store::{InMemoryMessageStore, MessageStore};
    /// let found = InMemoryMessageStore::new().get(harness::hub::message::MessageId::new());
    /// assert!(found.unwrap().is_none());
    /// ```
    fn get(&self, id: MessageId) -> Result<Option<StoredMessage>, StoreError>;
    /// Up to `limit` `Pending` or `Delivered` messages for `to` with
    /// `seq > after`, in ascending `Seq` order.
    ///
    /// ```
    /// use harness::hub::store::{InMemoryMessageStore, MessageStore};
    /// let to = harness::hub::lineage::SessionId::from_task(harness::task::TaskId("b".into()));
    /// assert!(InMemoryMessageStore::new().inbox(to, None, 10).unwrap().is_empty());
    /// ```
    fn inbox(
        &self,
        to: SessionId,
        after: Option<Seq>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StoreError>;
    /// Mark the `Delivered` messages for `to` with `seq <= upto` as `Acked`.
    /// Other states and other recipients are untouched; repeating is a no-op.
    ///
    /// ```
    /// use harness::hub::message::Seq;
    /// use harness::hub::store::{InMemoryMessageStore, MessageStore};
    /// let to = harness::hub::lineage::SessionId::from_task(harness::task::TaskId("b".into()));
    /// assert!(InMemoryMessageStore::new().ack(to, Seq::new(5)).is_ok());
    /// ```
    fn ack(&self, to: SessionId, upto: Seq) -> Result<(), StoreError>;
    /// Number of `Pending` or `Delivered` messages for `to`.
    ///
    /// ```
    /// use harness::hub::store::{InMemoryMessageStore, MessageStore};
    /// let to = harness::hub::lineage::SessionId::from_task(harness::task::TaskId("b".into()));
    /// assert_eq!(InMemoryMessageStore::new().unread(to).unwrap(), 0);
    /// ```
    fn unread(&self, to: SessionId) -> Result<usize, StoreError>;
    /// The sender's most recent messages in any state, oldest first, taking
    /// newest-first while the running total of subject and body bytes stays
    /// within `bytes`. A `Session` recipient narrows the window to messages
    /// to that session; the group recipients are resolved by the hub, so
    /// they select the sender's whole window.
    ///
    /// ```
    /// use harness::hub::message::Recipient;
    /// use harness::hub::store::{InMemoryMessageStore, MessageStore};
    /// let from = harness::hub::lineage::SessionId::from_task(harness::task::TaskId("a".into()));
    /// assert!(InMemoryMessageStore::new().outbound_window(from, Recipient::Parent, 1024).unwrap().is_empty());
    /// ```
    fn outbound_window(
        &self,
        from: SessionId,
        to: Recipient,
        bytes: usize,
    ) -> Result<Vec<StoredMessage>, StoreError>;
    /// Set the delivery state. Unknown id: `NotFound`.
    ///
    /// ```
    /// use harness::hub::message::{DeliveryState, MessageId};
    /// use harness::hub::store::{InMemoryMessageStore, MessageStore, StoreError};
    /// let result = InMemoryMessageStore::new().set_state(MessageId::new(), DeliveryState::Acked);
    /// assert!(matches!(result, Err(StoreError::NotFound)));
    /// ```
    fn set_state(&self, id: MessageId, s: DeliveryState) -> Result<(), StoreError>;
    /// Every `Quarantined` message of every root in ascending `Seq` order, and nothing else. The
    /// records carry the stored (already redacted) text and the reason codes of the hold; a read
    /// failure is an error, never an empty list.
    ///
    /// ```
    /// use harness::hub::store::{InMemoryMessageStore, MessageStore};
    /// assert!(InMemoryMessageStore::new().quarantined().unwrap().is_empty());
    /// ```
    fn quarantined(&self) -> Result<Vec<StoredMessage>, StoreError>;
    /// Move the message from `from` to `to` only if it is in `from` now, deciding and writing in
    /// one commit under the store's single lock, so of two writers racing for the same message
    /// exactly one wins and a writer holding a stale view never overwrites a newer state. Returns
    /// the record as written. A message in another state: `Conflict`; unknown id: `NotFound`.
    /// `release` is the only way a release marker reaches a record, and it is accepted only for
    /// `Quarantined` to `Pending`; any other combination is `Conflict` and changes nothing. A
    /// transition without `release` keeps the marker the record already has.
    ///
    /// ```
    /// use harness::hub::message::{DeliveryState, MessageId};
    /// use harness::hub::store::{InMemoryMessageStore, MessageStore, StoreError};
    /// let result = InMemoryMessageStore::new().transition(
    ///     MessageId::new(),
    ///     DeliveryState::Quarantined,
    ///     DeliveryState::Expired,
    ///     None,
    /// );
    /// assert!(matches!(result, Err(StoreError::NotFound)));
    /// ```
    fn transition(
        &self,
        id: MessageId,
        from: DeliveryState,
        to: DeliveryState,
        release: Option<Release>,
    ) -> Result<StoredMessage, StoreError>;
    /// Remove expired messages; returns how many.
    ///
    /// ```
    /// use harness::hub::store::{InMemoryMessageStore, MessageStore};
    /// let removed = InMemoryMessageStore::new().prune(chrono::Utc::now(), &Default::default());
    /// assert_eq!(removed.unwrap(), 0);
    /// ```
    fn prune(&self, now: DateTime<Utc>, policy: &Retention) -> Result<usize, StoreError>;
}

fn state_path_from(env: Option<&str>, home: Option<&Path>, file: &str) -> Option<PathBuf> {
    match env {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => home.map(|home| home.join(".local/state/nanna").join(file)),
    }
}

/// Where the session store lives: `NANNA_SESSIONS_PATH` when set and not
/// empty, otherwise `~/.local/state/nanna/sessions.jsonl`, or `None` when
/// neither is available.
///
/// ```
/// use harness::hub::store::sessions_path_from;
/// use std::path::{Path, PathBuf};
///
/// assert_eq!(sessions_path_from(Some("/x/s.jsonl"), None), Some(PathBuf::from("/x/s.jsonl")));
/// assert_eq!(
///     sessions_path_from(None, Some(Path::new("/home/u"))),
///     Some(PathBuf::from("/home/u/.local/state/nanna/sessions.jsonl"))
/// );
/// assert_eq!(sessions_path_from(Some(""), Some(Path::new("/h"))), sessions_path_from(None, Some(Path::new("/h"))));
/// assert_eq!(sessions_path_from(None, None), None);
/// ```
pub fn sessions_path_from(env: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    state_path_from(env, home, "sessions.jsonl")
}

/// Where the message store lives: `NANNA_MESSAGES_PATH` when set and not
/// empty, otherwise `~/.local/state/nanna/messages.jsonl`, or `None` when
/// neither is available.
///
/// ```
/// use harness::hub::store::messages_path_from;
/// use std::path::{Path, PathBuf};
///
/// assert_eq!(messages_path_from(Some("/x/m.jsonl"), None), Some(PathBuf::from("/x/m.jsonl")));
/// assert_eq!(
///     messages_path_from(None, Some(Path::new("/home/u"))),
///     Some(PathBuf::from("/home/u/.local/state/nanna/messages.jsonl"))
/// );
/// assert_eq!(messages_path_from(Some(""), None), None);
/// ```
pub fn messages_path_from(env: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    state_path_from(env, home, "messages.jsonl")
}

#[cfg(test)]
mod tests {
    use super::contract::*;
    use super::*;

    #[test]
    fn session_and_message_paths_prefer_a_non_empty_override_then_home() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(
            sessions_path_from(Some("/o/s.jsonl"), home),
            Some(PathBuf::from("/o/s.jsonl"))
        );
        assert_eq!(
            sessions_path_from(None, home),
            Some(PathBuf::from("/home/u/.local/state/nanna/sessions.jsonl"))
        );
        assert_eq!(
            sessions_path_from(Some(""), home),
            sessions_path_from(None, home)
        );
        assert_eq!(sessions_path_from(Some(""), None), None);
        assert_eq!(sessions_path_from(None, None), None);
        assert_eq!(
            messages_path_from(Some("/o/m.jsonl"), None),
            Some(PathBuf::from("/o/m.jsonl"))
        );
        assert_eq!(
            messages_path_from(None, home),
            Some(PathBuf::from("/home/u/.local/state/nanna/messages.jsonl"))
        );
        assert_eq!(
            messages_path_from(Some(""), home),
            messages_path_from(None, home)
        );
        assert_eq!(messages_path_from(None, None), None);
    }

    #[test]
    fn the_env_variable_names_are_the_documented_ones() {
        assert_eq!(SESSIONS_PATH_ENV, "NANNA_SESSIONS_PATH");
        assert_eq!(MESSAGES_PATH_ENV, "NANNA_MESSAGES_PATH");
    }

    #[test]
    fn default_retention_is_seven_days_one_day_seven_days() {
        let policy = Retention::default();
        assert_eq!(policy.completed_session_ttl, Duration::days(7));
        assert_eq!(policy.acked_message_ttl, Duration::hours(24));
        assert_eq!(policy.unacked_message_ttl, Duration::days(7));
    }

    #[test]
    fn store_errors_read_as_documented_and_carry_no_content() {
        assert_eq!(
            StoreError::Corrupt(3).to_string(),
            "corrupt record at line 3"
        );
        assert_eq!(
            StoreError::Locked.to_string(),
            "hub state locked by another nanna process"
        );
        assert_eq!(StoreError::NotFound.to_string(), "no such record");
        assert_eq!(StoreError::Duplicate.to_string(), "record already exists");
        assert_eq!(
            StoreError::Conflict.to_string(),
            "record is not in the expected state"
        );
        let io = StoreError::from(io::Error::other("disk"));
        assert_eq!(io.to_string(), "io: disk");
    }

    #[test]
    fn the_lock_reports_its_directory() {
        let fixture = Fixture::new();
        assert_eq!(fixture.lock.dir(), fixture.dir.path());
    }

    #[test]
    fn a_second_open_of_the_same_file_is_locked_until_the_first_store_drops() {
        let fixture = Fixture::new();
        let sessions = fixture.path("sessions.jsonl");
        let messages = fixture.path("messages.jsonl");
        let first = JsonlSessionStore::open(&sessions, &fixture.lock).unwrap();
        assert!(matches!(
            JsonlSessionStore::open(&sessions, &fixture.lock).err(),
            Some(StoreError::Locked)
        ));
        let first_messages = JsonlMessageStore::open(&messages, &fixture.lock).unwrap();
        assert!(matches!(
            JsonlMessageStore::open(&messages, &fixture.lock).err(),
            Some(StoreError::Locked)
        ));
        drop(first);
        assert!(JsonlSessionStore::open(&sessions, &fixture.lock).is_ok());
        drop(first_messages);
        assert!(JsonlMessageStore::open(&messages, &fixture.lock).is_ok());
    }

    #[test]
    fn a_failed_open_releases_its_claim() {
        let fixture = Fixture::new();
        let path = fixture.path("sessions.jsonl");
        std::fs::write(&path, "{\"op\":\n{\"op\":\n").unwrap();
        assert!(matches!(
            JsonlSessionStore::open(&path, &fixture.lock).err(),
            Some(StoreError::Corrupt(1))
        ));
        std::fs::write(&path, "").unwrap();
        assert!(JsonlSessionStore::open(&path, &fixture.lock).is_ok());
    }

    #[test]
    fn in_memory_stores_start_empty_through_new_and_default() {
        let sessions = InMemorySessionStore::default();
        assert!(sessions.in_root(&root(1)).unwrap().is_empty());
        let messages = InMemoryMessageStore::default();
        assert_eq!(messages.unread(sid("a")).unwrap(), 0);
        assert!(InMemorySessionStore::new().get(sid("a")).unwrap().is_none());
    }
}
