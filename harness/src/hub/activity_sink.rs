//! The writer of `ToolCall` activity entries: one sink per audited session, built by the host.

use super::lineage::SessionId;
use super::store::{ActivityEntry, ActivityStore};
use crate::action_auditor::SharedOracle;
use crate::leases::Clock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const APPEND_FAILED: &str = "activity log append failed; the tool result is unchanged";

/// Where one session's tool calls are logged. The session, the oracle and the clock come from the
/// host binding of the task, never from a tool call.
///
/// An append failure never reaches the caller of a tool: it is counted and logged with the
/// session id and the count only.
///
/// ```
/// use harness::hub::activity_sink::ActivitySink;
/// use harness::hub::lineage::SessionId;
/// use harness::hub::store::{ActivityStore, InMemoryActivityStore};
/// use harness::leases::SystemClock;
/// use std::sync::Arc;
///
/// let store = Arc::new(InMemoryActivityStore::new());
/// let session = SessionId::from_task(harness::task::TaskId("s".into()));
/// let oracle = Arc::new(harness::action_auditor::NullOracle);
/// let sink = ActivitySink::new(store.clone(), session.clone(), oracle, Arc::new(SystemClock));
/// assert!(sink.record("write_file", &serde_json::json!({"path": "a"}), &["path"], true));
/// assert_eq!(store.for_session(session, 4096).unwrap().len(), 1);
/// assert_eq!(sink.failures(), 0);
/// ```
pub struct ActivitySink {
    store: Arc<dyn ActivityStore>,
    session: SessionId,
    oracle: SharedOracle,
    clock: Arc<dyn Clock>,
    failures: AtomicU64,
}

impl std::fmt::Debug for ActivitySink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivitySink")
            .field("session", &self.session)
            .field("failures", &self.failures())
            .finish()
    }
}

impl ActivitySink {
    /// A sink appending to `store` for `session`, vouching for identifiers with `oracle`.
    pub fn new(
        store: Arc<dyn ActivityStore>,
        session: SessionId,
        oracle: SharedOracle,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            session,
            oracle,
            clock,
            failures: AtomicU64::new(0),
        }
    }

    /// Log one call of the registered tool `tool`: its argument names (only those in `known`
    /// by name, the rest as `?`), the kind and length of each value, and whether it succeeded.
    /// Returns whether the entry was appended; a failure is counted and logged, never raised.
    ///
    /// ```
    /// use harness::hub::activity_sink::ActivitySink;
    /// use harness::hub::lineage::SessionId;
    /// use harness::hub::store::{ActivityEntry, ActivityStore, InMemoryActivityStore};
    /// use harness::leases::SystemClock;
    /// use std::sync::Arc;
    ///
    /// let store = Arc::new(InMemoryActivityStore::new());
    /// let session = SessionId::from_task(harness::task::TaskId("s".into()));
    /// let oracle = Arc::new(harness::action_auditor::NullOracle);
    /// let sink = ActivitySink::new(store.clone(), session.clone(), oracle, Arc::new(SystemClock));
    /// sink.record("write_file", &serde_json::json!({"path": "a", "zz": 1}), &["path"], false);
    /// let kept = store.for_session(session, 4096).unwrap();
    /// let ActivityEntry::ToolCall { args_summary, ok, .. } = &kept[0] else { panic!() };
    /// assert_eq!(args_summary.as_str(), "?=num path=str:1");
    /// assert!(!ok);
    /// ```
    pub fn record(&self, tool: &str, args: &serde_json::Value, known: &[&str], ok: bool) -> bool {
        let at = self.clock.now();
        let entry = ActivityEntry::tool_call(self.session.clone(), at, tool, args, known, ok);
        let appended = self.store.append_with(entry, self.oracle.as_ref()).is_ok();
        if !appended {
            self.note_failure();
        }
        appended
    }

    /// How many appends failed so far.
    ///
    /// ```
    /// use harness::hub::activity_sink::ActivitySink;
    /// use harness::hub::lineage::SessionId;
    /// use harness::hub::store::InMemoryActivityStore;
    /// use harness::leases::SystemClock;
    /// use std::sync::Arc;
    ///
    /// let session = SessionId::from_task(harness::task::TaskId("s".into()));
    /// let oracle = Arc::new(harness::action_auditor::NullOracle);
    /// let store = Arc::new(InMemoryActivityStore::new());
    /// assert_eq!(ActivitySink::new(store, session, oracle, Arc::new(SystemClock)).failures(), 0);
    /// ```
    pub fn failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }

    fn note_failure(&self) {
        let count = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::error!(session = %self.session, failures = count, "{APPEND_FAILED}");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::action_auditor::{IdentifierOracle, NullOracle};
    use crate::hub::store::{InMemoryActivityStore, Retention, StoreError};
    use chrono::{DateTime, Utc};
    use std::sync::Mutex;

    pub(crate) struct FailingStore {
        attempts: Mutex<usize>,
        inner: InMemoryActivityStore,
    }

    impl FailingStore {
        pub(crate) fn new() -> Arc<Self> {
            let attempts = Mutex::new(0);
            let inner = InMemoryActivityStore::new();
            Arc::new(Self { attempts, inner })
        }

        pub(crate) fn attempts(&self) -> usize {
            *self.attempts.lock().unwrap()
        }
    }

    impl ActivityStore for FailingStore {
        fn append_with(
            &self,
            _e: ActivityEntry,
            _o: &dyn IdentifierOracle,
        ) -> Result<(), StoreError> {
            *self.attempts.lock().unwrap() += 1;
            Err(StoreError::Locked)
        }

        fn for_session(
            &self,
            id: SessionId,
            budget: usize,
        ) -> Result<Vec<ActivityEntry>, StoreError> {
            self.inner.for_session(id, budget)
        }

        fn prune(&self, now: DateTime<Utc>, policy: &Retention) -> Result<usize, StoreError> {
            self.inner.prune(now, policy)
        }

        fn forget(&self, sessions: &[SessionId]) -> Result<usize, StoreError> {
            self.inner.forget(sessions)
        }
    }

    #[derive(Clone, Default)]
    pub(crate) struct LogBuf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    static SERIAL: Mutex<()> = Mutex::new(());

    pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    type Held = (
        tracing::subscriber::DefaultGuard,
        std::sync::MutexGuard<'static, ()>,
    );

    pub(crate) fn capture() -> (LogBuf, Held) {
        let lock = serial();
        let buf = LogBuf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();
        (buf, (guard, lock))
    }

    pub(crate) fn text(buf: &LogBuf) -> String {
        String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
    }

    fn session() -> SessionId {
        SessionId::from_task(crate::task::TaskId("sink-session-77".into()))
    }

    fn sink_over(store: Arc<dyn ActivityStore>) -> ActivitySink {
        let oracle = Arc::new(NullOracle);
        ActivitySink::new(
            store,
            session(),
            oracle,
            Arc::new(crate::leases::SystemClock),
        )
    }

    const PLANTED: &str = "Zx9Qw3Lm7Rt5Yp2Vb8Nc4";

    #[test]
    fn a_good_append_returns_true_and_counts_no_failure() {
        let store = Arc::new(InMemoryActivityStore::new());
        let sink = sink_over(store.clone());
        assert!(sink.record(
            "write_file",
            &serde_json::json!({"path": "a"}),
            &["path"],
            true
        ));
        assert_eq!(sink.failures(), 0);
        assert_eq!(store.for_session(session(), 4096).unwrap().len(), 1);
    }

    #[test]
    fn each_failed_append_is_counted_and_logged_with_the_session_and_the_count_only() {
        let (buf, _guard) = capture();
        let failing = FailingStore::new();
        let sink = sink_over(failing.clone());
        let args = serde_json::json!({PLANTED: "body"});
        assert!(!sink.record(PLANTED, &args, &[], true));
        assert!(!sink.record(PLANTED, &args, &[], false));
        assert_eq!(sink.failures(), 2);
        assert_eq!(failing.attempts(), 2);
        let log = text(&buf);
        assert!(log.contains("sink-session-77"), "{log}");
        assert!(
            log.contains("failures=1") && log.contains("failures=2"),
            "{log}"
        );
        for secret in [PLANTED, "body", "Locked", "locked"] {
            assert!(!log.contains(secret), "{log}");
        }
    }

    #[test]
    fn the_debug_form_names_the_session_and_the_count_only() {
        let _serial = serial();
        let failing = FailingStore::new();
        let sink = sink_over(failing);
        sink.record(PLANTED, &serde_json::json!({PLANTED: 1}), &[], true);
        let debug = format!("{sink:?}");
        assert!(debug.contains("sink-session-77") && debug.contains("failures: 1"));
        assert!(!debug.contains(PLANTED));
    }

    #[test]
    fn the_failing_store_double_fails_only_appends() {
        let failing = FailingStore::new();
        let entry = ActivityEntry::StatusChange {
            session: session(),
            at: Utc::now(),
            status: crate::hub::session::SessionStatus::Running,
        };
        assert!(matches!(failing.append(entry), Err(StoreError::Locked)));
        assert!(failing.for_session(session(), 10).unwrap().is_empty());
        let policy = Retention::default();
        assert_eq!(failing.prune(Utc::now(), &policy).unwrap(), 0);
        assert_eq!(failing.forget(&[session()]).unwrap(), 0);
    }
}
