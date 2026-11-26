//! The production assembly of the message hub: the stores, the one session registry and the hub.

use super::doorbell::{Doorbell, NoopDoorbell};
use super::oracle::SystemGit;
use super::registry::SessionRegistry;
use super::router::{BindingTable, HubRouter, ProductionAuditors};
use super::send::{AuditorProvider, MessageHub};
use super::store::{
    activity_path_from, messages_path_from, sessions_path_from, ActivityStore, HubLock,
    JsonlActivityStore, JsonlMessageStore, JsonlSessionStore, MessageStore, SessionStore,
    StoreError,
};
use crate::leases::{Clock, SystemClock};
use crate::scheduler::QueueStoreError;
use crate::task::{RestoreParts, TaskManager, RESTORE_AUDIT_DEADLINE};
use chrono::{DateTime, Utc};
use model::ModelProvider;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

/// Where the session and message journals live.
///
/// ```
/// use harness::hub::runtime::HubPaths;
/// use std::path::Path;
///
/// let paths = HubPaths::beside(Path::new("/s/queue.jsonl"), None, Some("/m/messages.jsonl"));
/// assert_eq!(paths.sessions, Path::new("/s/sessions.jsonl"));
/// assert_eq!(paths.messages, Path::new("/m/messages.jsonl"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubPaths {
    pub sessions: PathBuf,
    pub messages: PathBuf,
}

impl HubPaths {
    /// The journals named by `NANNA_SESSIONS_PATH` and `NANNA_MESSAGES_PATH`, else next to `queue`.
    pub fn resolve(queue: &Path) -> Self {
        let sessions = std::env::var(super::store::SESSIONS_PATH_ENV).ok();
        let messages = std::env::var(super::store::MESSAGES_PATH_ENV).ok();
        Self::beside(queue, sessions.as_deref(), messages.as_deref())
    }

    /// The activity journal: `activity_env` when it is a non-empty path, else `activity.jsonl`
    /// beside the sessions journal. `HubPaths` itself does not carry it.
    ///
    /// ```
    /// use harness::hub::runtime::HubPaths;
    /// use std::path::Path;
    ///
    /// let paths = HubPaths::beside(Path::new("/s/queue.jsonl"), None, None);
    /// assert_eq!(paths.activity(None), Path::new("/s/activity.jsonl"));
    /// assert_eq!(paths.activity(Some("")), Path::new("/s/activity.jsonl"));
    /// assert_eq!(paths.activity(Some("/a/log.jsonl")), Path::new("/a/log.jsonl"));
    /// ```
    pub fn activity(&self, activity_env: Option<&str>) -> PathBuf {
        let beside = self.sessions.with_file_name("activity.jsonl");
        activity_path_from(activity_env, None).unwrap_or(beside)
    }

    /// The journals named by the two environment values, each defaulting to a file next to `queue`.
    pub fn beside(queue: &Path, sessions_env: Option<&str>, messages_env: Option<&str>) -> Self {
        let beside = |file: &str| queue.with_file_name(file);
        Self {
            sessions: sessions_path_from(sessions_env, None)
                .unwrap_or_else(|| beside("sessions.jsonl")),
            messages: messages_path_from(messages_env, None)
                .unwrap_or_else(|| beside("messages.jsonl")),
        }
    }
}

pub(crate) type AuditorFactory = Box<
    dyn FnOnce(
        &Arc<dyn SessionStore>,
        &Arc<dyn MessageStore>,
        &Arc<BindingTable>,
    ) -> Arc<dyn AuditorProvider>,
>;

struct Parts {
    registry: Arc<SessionRegistry>,
    clock: Arc<dyn Clock>,
    started: DateTime<Utc>,
    router: Arc<HubRouter>,
    activity: Option<Arc<dyn ActivityStore>>,
    _locks: Vec<Arc<HubLock>>,
}

const SWEEP_DONE: &str = "message hub closed orphaned sessions";
const SWEEP_FAILED: &str =
    "message hub orphan sweep failed; orphans it did not reach stay open until the next start";

impl Parts {
    fn close_orphans(&self) {
        match self.registry.sweep_orphans(self.started, self.clock.now()) {
            Ok(closed) => tracing::info!(closed = closed.len(), "{SWEEP_DONE}"),
            Err(error) => tracing::error!(%error, "{SWEEP_FAILED}"),
        }
    }
}

/// The hub of one host process, or the fact that it is unavailable.
///
/// ```
/// use harness::hub::runtime::HubRuntime;
///
/// assert!(!HubRuntime::unavailable().is_available());
/// ```
pub struct HubRuntime {
    parts: Option<Parts>,
}

impl std::fmt::Debug for HubRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubRuntime")
            .field("available", &self.is_available())
            .finish()
    }
}

fn lock_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

type Opened = (
    Arc<dyn SessionStore>,
    Arc<dyn MessageStore>,
    Vec<Arc<HubLock>>,
);

fn open_stores(paths: &HubPaths) -> Result<Opened, StoreError> {
    let first = HubLock::acquire(lock_dir(&paths.sessions))?;
    let sessions = Arc::new(JsonlSessionStore::open(&paths.sessions, &first)?);
    let same = lock_dir(&paths.sessions) == lock_dir(&paths.messages);
    let second = if same {
        Arc::clone(&first)
    } else {
        HubLock::acquire(lock_dir(&paths.messages))?
    };
    let messages = Arc::new(JsonlMessageStore::open(&paths.messages, &second)?);
    let mut locks = vec![first];
    if !same {
        locks.push(second);
    }
    Ok((sessions, messages, locks))
}

fn lock_for(
    dir: &Path,
    paths: &HubPaths,
    locks: &[Arc<HubLock>],
) -> Result<Arc<HubLock>, StoreError> {
    let beside_sessions = lock_dir(&paths.sessions) == dir;
    let beside_messages = lock_dir(&paths.messages) == dir;
    let held = if beside_sessions {
        locks.first()
    } else if beside_messages {
        locks.last()
    } else {
        None
    };
    match held {
        Some(lock) => Ok(Arc::clone(lock)),
        None => HubLock::acquire(dir),
    }
}

fn activity_store(
    path: &Path,
    paths: &HubPaths,
    locks: &mut Vec<Arc<HubLock>>,
) -> Result<Arc<dyn ActivityStore>, StoreError> {
    let lock = lock_for(lock_dir(path), paths, locks)?;
    let store = JsonlActivityStore::open(path, &lock)?;
    locks.push(lock);
    Ok(Arc::new(store))
}

fn open_activity(
    paths: &HubPaths,
    env: Option<&str>,
    locks: &mut Vec<Arc<HubLock>>,
) -> Option<Arc<dyn ActivityStore>> {
    match activity_store(&paths.activity(env), paths, locks) {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::error!(%error, "{ACTIVITY_UNAVAILABLE}");
            None
        }
    }
}

const ACTIVITY_UNAVAILABLE: &str =
    "activity log unavailable: tool calls are not logged and the librarian has no activity; the hub is otherwise up";

impl HubRuntime {
    /// A runtime with no hub: no registry, no tools, and the gate blocks message tools.
    pub fn unavailable() -> Self {
        Self { parts: None }
    }

    /// Open the journals at `paths` and build the hub, auditing messages with `model`.
    ///
    /// Locked or unreadable stores give an unavailable runtime and a loud error log, so the
    /// host still starts without a hub.
    pub fn open(paths: &HubPaths, provider: Arc<dyn ModelProvider>, model: &str) -> Self {
        HubRuntime::open_with_doorbell(paths, provider, model, Arc::new(NoopDoorbell))
    }

    /// [`HubRuntime::open`] with `doorbell` rung after every delivered message.
    pub fn open_with_doorbell(
        paths: &HubPaths,
        provider: Arc<dyn ModelProvider>,
        model: &str,
        doorbell: Arc<dyn Doorbell>,
    ) -> Self {
        let model = model.to_string();
        let factory: AuditorFactory = Box::new(move |sessions, messages, table| {
            let (sessions, messages) = (Arc::clone(sessions), Arc::clone(messages));
            let table = Arc::clone(table);
            let auditors = ProductionAuditors::new(
                provider,
                &model,
                sessions,
                messages,
                table,
                Arc::new(SystemGit),
            );
            Arc::new(auditors)
        });
        HubRuntime::open_with(paths, factory, Arc::new(SystemClock), doorbell)
    }

    pub(crate) fn open_with(
        paths: &HubPaths,
        auditors: AuditorFactory,
        clock: Arc<dyn Clock>,
        doorbell: Arc<dyn Doorbell>,
    ) -> Self {
        match open_stores(paths) {
            Ok((sessions, messages, mut locks)) => {
                let env = std::env::var(super::store::ACTIVITY_PATH_ENV).ok();
                let activity = open_activity(paths, env.as_deref(), &mut locks);
                let mut rt = Self::assemble_with(sessions, messages, auditors, clock, doorbell);
                if let Some(parts) = rt.parts.as_mut() {
                    parts._locks = locks;
                }
                rt.log_activity_to(activity);
                rt
            }
            Err(error) => {
                tracing::error!(%error, "message hub unavailable: its stores could not be opened; message tools are disabled");
                Self::unavailable()
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn assemble(
        sessions: Arc<dyn SessionStore>,
        messages: Arc<dyn MessageStore>,
        auditors: AuditorFactory,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let doorbell = Arc::new(NoopDoorbell);
        Self::assemble_with(sessions, messages, auditors, clock, doorbell)
    }

    pub(crate) fn assemble_with(
        sessions: Arc<dyn SessionStore>,
        messages: Arc<dyn MessageStore>,
        auditors: AuditorFactory,
        clock: Arc<dyn Clock>,
        doorbell: Arc<dyn Doorbell>,
    ) -> Self {
        let table = BindingTable::new();
        let auditors = auditors(&sessions, &messages, &table);
        let registry = Arc::new(SessionRegistry::new(Arc::clone(&sessions)));
        let started = clock.now();
        let hub = MessageHub::new(
            sessions,
            messages,
            auditors,
            Arc::clone(&clock),
            Default::default(),
            Arc::clone(&registry),
        );
        let hub = hub.with_doorbell(doorbell);
        Self {
            parts: Some(Parts {
                registry,
                clock,
                started,
                router: Arc::new(HubRouter::new(Arc::new(hub), table)),
                activity: None,
                _locks: Vec::new(),
            }),
        }
    }

    /// The activity log, when the hub and the log both opened.
    ///
    /// ```
    /// use harness::hub::runtime::HubRuntime;
    ///
    /// assert!(HubRuntime::unavailable().activity().is_none());
    /// ```
    pub fn activity(&self) -> Option<Arc<dyn ActivityStore>> {
        self.parts.as_ref().and_then(|parts| parts.activity.clone())
    }

    pub(crate) fn log_activity_to(&mut self, store: Option<Arc<dyn ActivityStore>>) {
        if let (Some(parts), Some(store)) = (self.parts.as_mut(), store) {
            parts.router.set_activity(Arc::clone(&store));
            parts.activity = Some(store);
        }
    }

    /// Whether the hub opened.
    pub fn is_available(&self) -> bool {
        self.parts.is_some()
    }

    /// The hub, when available.
    pub fn hub(&self) -> Option<Arc<MessageHub>> {
        self.parts.as_ref().map(|parts| parts.router.hub())
    }

    /// The router the action gates hold weakly: dangling when the runtime is unavailable, so
    /// every message tool is blocked.
    pub fn router_weak(&self) -> Weak<HubRouter> {
        match &self.parts {
            Some(parts) => Arc::downgrade(&parts.router),
            None => Weak::new(),
        }
    }

    /// The one session registry, when available.
    pub fn registry(&self) -> Option<Arc<SessionRegistry>> {
        self.parts.as_ref().map(|parts| Arc::clone(&parts.registry))
    }

    /// Give `manager` this runtime's registry and hub. An unavailable runtime changes nothing.
    pub fn attach(&self, manager: TaskManager) -> TaskManager {
        match &self.parts {
            Some(parts) => manager
                .with_session_registry(Arc::clone(&parts.registry))
                .with_hub(Arc::clone(&parts.router)),
            None => manager,
        }
    }

    /// Restore a manager over `parts`, sharing this runtime's registry with the hub.
    pub async fn restore(&self, parts: RestoreParts<'_>) -> Result<TaskManager, QueueStoreError> {
        match &self.parts {
            Some(hub_parts) => {
                let registry = Arc::clone(&hub_parts.registry);
                let manager =
                    TaskManager::restore_within_sessions(parts, RESTORE_AUDIT_DEADLINE, registry)
                        .await?;
                hub_parts.close_orphans();
                Ok(manager.with_hub(Arc::clone(&hub_parts.router)))
            }
            None => TaskManager::restore_without_sessions(parts, RESTORE_AUDIT_DEADLINE).await,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::action_auditor::{FailClosedMessageAuditor, MessageAuditor, ModelMessageAuditor};
    use crate::hub::activity_sink::tests::{capture, serial, text};
    use crate::hub::lineage::{Lineage, RootId, SessionBinding};
    use crate::hub::store::{InMemoryMessageStore, InMemorySessionStore};
    use std::fs;

    fn provider() -> Arc<dyn ModelProvider> {
        Arc::new(model::OllamaProvider::new(model::OllamaConfig::default()).unwrap())
    }

    pub(crate) fn constant_auditors(auditor: Arc<dyn MessageAuditor>) -> AuditorFactory {
        Box::new(move |_, _, _| Arc::new(auditor))
    }

    fn paths(dir: &Path) -> HubPaths {
        HubPaths {
            sessions: dir.join("sessions.jsonl"),
            messages: dir.join("messages.jsonl"),
        }
    }

    #[test]
    fn paths_default_beside_the_queue_and_follow_a_non_empty_override() {
        let queue = Path::new("/s/queue.jsonl");
        let by_default = HubPaths::beside(queue, None, None);
        assert_eq!(by_default.sessions, Path::new("/s/sessions.jsonl"));
        assert_eq!(by_default.messages, Path::new("/s/messages.jsonl"));
        let empty = HubPaths::beside(queue, Some(""), Some(""));
        assert_eq!(empty, by_default);
        let moved = HubPaths::beside(queue, Some("/a/x.jsonl"), Some("/b/y.jsonl"));
        assert_eq!(moved.sessions, Path::new("/a/x.jsonl"));
        assert_eq!(moved.messages, Path::new("/b/y.jsonl"));
    }

    #[test]
    fn resolve_reads_the_two_environment_variables() {
        let queue = Path::new("/s/queue.jsonl");
        let sessions = std::env::var(super::super::store::SESSIONS_PATH_ENV).ok();
        let messages = std::env::var(super::super::store::MESSAGES_PATH_ENV).ok();
        assert_eq!(
            HubPaths::resolve(queue),
            HubPaths::beside(queue, sessions.as_deref(), messages.as_deref())
        );
    }

    #[test]
    fn an_opened_runtime_has_one_registry_shared_with_the_hub() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = HubRuntime::open(&paths(dir.path()), provider(), "m");
        assert!(runtime.is_available());
        let (hub, registry) = (runtime.hub().unwrap(), runtime.registry().unwrap());
        assert!(Arc::ptr_eq(hub.registry(), &registry));
        assert!(dir.path().join("sessions.jsonl").exists());
        assert!(dir.path().join("messages.jsonl").exists());
        assert!(dir.path().join("hub.lock").exists());
        assert_eq!(format!("{runtime:?}"), "HubRuntime { available: true }");
    }

    #[test]
    fn stores_that_are_locked_give_a_runtime_without_a_hub() {
        let dir = tempfile::tempdir().unwrap();
        let first = HubRuntime::open(&paths(dir.path()), provider(), "m");
        assert!(first.is_available());
        let second = HubRuntime::open(&paths(dir.path()), provider(), "m");
        assert!(!second.is_available());
        assert!(second.hub().is_none() && second.registry().is_none());
        drop(first);
        let third = HubRuntime::open(&paths(dir.path()), provider(), "m");
        assert!(third.is_available());
    }

    #[test]
    fn each_store_directory_is_locked_separately() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let split = HubPaths {
            sessions: a.path().join("sessions.jsonl"),
            messages: b.path().join("messages.jsonl"),
        };
        let first = HubRuntime::open(&split, provider(), "m");
        assert!(first.is_available());
        let sessions_taken = HubPaths {
            sessions: a.path().join("sessions.jsonl"),
            messages: tempfile::tempdir().unwrap().path().join("m.jsonl"),
        };
        assert!(!HubRuntime::open(&sessions_taken, provider(), "m").is_available());
        let messages_taken = HubPaths {
            sessions: tempfile::tempdir().unwrap().path().join("s.jsonl"),
            messages: b.path().join("messages.jsonl"),
        };
        assert!(!HubRuntime::open(&messages_taken, provider(), "m").is_available());
    }

    fn quiet_auditors() -> AuditorFactory {
        let auditor = FailClosedMessageAuditor::new(ModelMessageAuditor::new(provider(), "m"));
        constant_auditors(Arc::new(auditor))
    }

    fn stores(paths: &HubPaths) -> Vec<Arc<HubLock>> {
        open_stores(paths).unwrap().2
    }

    #[test]
    fn the_activity_journal_is_the_override_when_non_empty_else_beside_the_sessions_journal() {
        let paths = HubPaths::beside(Path::new("/s/queue.jsonl"), Some("/a/x.jsonl"), None);
        assert_eq!(paths.activity(None), Path::new("/a/activity.jsonl"));
        assert_eq!(paths.activity(Some("")), Path::new("/a/activity.jsonl"));
        assert_eq!(paths.activity(Some("/b/y.jsonl")), Path::new("/b/y.jsonl"));
        assert_eq!(paths.activity(Some("")), paths.activity(None));
    }

    #[test]
    fn an_opened_runtime_opens_the_activity_log_beside_the_sessions_under_the_same_lock() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = HubRuntime::open(&paths(dir.path()), provider(), "m");
        assert!(runtime.is_available());
        assert!(runtime.activity().is_some());
        assert!(dir.path().join("activity.jsonl").exists());
        let again = HubRuntime::open(&paths(dir.path()), provider(), "m");
        assert!(!again.is_available() && again.activity().is_none());
    }

    #[test]
    fn an_activity_log_in_the_messages_directory_shares_that_directorys_lock() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let split = HubPaths {
            sessions: a.path().join("sessions.jsonl"),
            messages: b.path().join("messages.jsonl"),
        };
        let in_b = b.path().join("activity.jsonl");
        let mut locks = stores(&split);
        assert_eq!(locks.len(), 2);
        assert!(open_activity(&split, in_b.to_str(), &mut locks).is_some());
        assert!(in_b.exists());
    }

    #[test]
    fn an_activity_log_beside_the_sessions_shares_the_sessions_lock_even_when_the_messages_differ()
    {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let split = HubPaths {
            sessions: a.path().join("sessions.jsonl"),
            messages: b.path().join("messages.jsonl"),
        };
        let mut locks = stores(&split);
        assert!(open_activity(&split, None, &mut locks).is_some());
        assert!(a.path().join("activity.jsonl").exists());
    }

    #[test]
    fn an_activity_log_in_a_third_directory_takes_its_own_lock() {
        let (dir, elsewhere) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let target = elsewhere.path().join("log.jsonl");
        let mut locks = stores(&paths(dir.path()));
        let store = open_activity(&paths(dir.path()), target.to_str(), &mut locks);
        assert!(store.is_some() && target.exists());
        assert!(!dir.path().join("activity.jsonl").exists());
        assert!(matches!(
            HubLock::acquire(elsewhere.path()),
            Err(StoreError::Locked)
        ));
        drop((store, locks));
        assert!(HubLock::acquire(elsewhere.path()).is_ok());
    }

    #[test]
    fn an_activity_log_that_cannot_open_gives_no_store_and_a_loud_log() {
        let (buf, _guard) = capture();
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "x").unwrap();
        let unopenable = blocker.join("activity.jsonl");
        let mut locks = stores(&paths(dir.path()));
        assert!(open_activity(&paths(dir.path()), unopenable.to_str(), &mut locks).is_none());
        let log = text(&buf);
        assert!(log.contains("activity log unavailable"), "{log}");
        assert!(log.contains("ERROR"), "{log}");
        assert!(!log.contains("file/activity.jsonl"), "{log}");
    }

    #[test]
    fn an_activity_log_locked_by_another_process_gives_no_store() {
        let _serial = serial();
        let (dir, elsewhere) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let held = HubLock::acquire(elsewhere.path()).unwrap();
        let target = elsewhere.path().join("log.jsonl");
        let mut locks = stores(&paths(dir.path()));
        assert!(open_activity(&paths(dir.path()), target.to_str(), &mut locks).is_none());
        drop(held);
    }

    #[test]
    fn a_runtime_whose_activity_log_cannot_open_still_has_its_hub_and_no_sink() {
        let _serial = serial();
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("activity.jsonl")).unwrap();
        let runtime = HubRuntime::open(&paths(dir.path()), provider(), "m");
        assert!(runtime.is_available());
        assert!(runtime.activity().is_none());
        let router = runtime.router_weak().upgrade().unwrap();
        assert!(router.activity_sink(&sender()).is_none());
    }

    fn sender() -> SessionBinding {
        let lineage = Lineage::root_child(RootId::mcp(uuid::Uuid::from_u128(7)));
        let card = crate::identity::example();
        SessionBinding::from_audited(&card, crate::task::TaskId("s-1".to_string()), lineage)
    }

    #[test]
    fn an_opened_runtime_builds_a_sink_that_writes_to_the_log_it_exposes() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = HubRuntime::open(&paths(dir.path()), provider(), "m");
        let router = runtime.router_weak().upgrade().unwrap();
        let session = sender();
        let sink = router.activity_sink(&session).unwrap();
        assert!(sink.record(
            "write_file",
            &serde_json::json!({"path": "a"}),
            &["path"],
            true
        ));
        let kept = runtime.activity().unwrap();
        let entries = kept.for_session(session.session().clone(), 4096).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session(), session.session());
    }

    #[test]
    fn an_assembled_runtime_without_a_log_builds_no_sink() {
        let runtime = HubRuntime::assemble(
            Arc::new(InMemorySessionStore::new()),
            Arc::new(InMemoryMessageStore::new()),
            quiet_auditors(),
            Arc::new(SystemClock),
        );
        assert!(runtime.activity().is_none());
        let router = runtime.router_weak().upgrade().unwrap();
        assert!(router.activity_sink(&sender()).is_none());
    }

    #[test]
    fn stores_that_cannot_be_created_give_a_runtime_without_a_hub() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "x").unwrap();
        let under_a_file = HubPaths {
            sessions: blocker.join("sessions.jsonl"),
            messages: blocker.join("messages.jsonl"),
        };
        assert!(!HubRuntime::open(&under_a_file, provider(), "m").is_available());
        let bare = HubPaths {
            sessions: PathBuf::from("sessions.jsonl"),
            messages: PathBuf::from("messages.jsonl"),
        };
        assert_eq!(lock_dir(&bare.sessions), Path::new("."));
    }

    #[test]
    fn the_auditors_and_the_router_share_one_binding_table() {
        let seen: Arc<std::sync::Mutex<Option<Arc<BindingTable>>>> = Arc::default();
        let capture = seen.clone();
        let auditor = FailClosedMessageAuditor::new(ModelMessageAuditor::new(provider(), "m"));
        let shared: Arc<dyn MessageAuditor> = Arc::new(auditor);
        let factory: AuditorFactory = Box::new(move |_, _, table| {
            *capture.lock().unwrap() = Some(Arc::clone(table));
            Arc::new(shared)
        });
        let runtime = HubRuntime::assemble(
            Arc::new(InMemorySessionStore::new()),
            Arc::new(InMemoryMessageStore::new()),
            factory,
            Arc::new(SystemClock),
        );
        let router = runtime.router_weak().upgrade().unwrap();
        let given = seen.lock().unwrap().clone().unwrap();
        assert!(Arc::ptr_eq(&given, &router.table()));
        assert!(Arc::ptr_eq(&router.hub(), &runtime.hub().unwrap()));
    }

    #[derive(Default)]
    struct Tally(std::sync::Mutex<Vec<usize>>);

    impl Doorbell for Tally {
        fn ring(&self, _to: &super::super::lineage::SessionId, unread: usize) {
            self.0.lock().unwrap().push(unread);
        }
    }

    fn seat(runtime: &HubRuntime, name: &str) -> SessionBinding {
        let lineage = Lineage::root_child(RootId::mcp(uuid::Uuid::from_u128(5)));
        let mut card = crate::identity::example();
        card.scope.message_reach = crate::hub::message::MessageReach::Root;
        let task = crate::task::TaskId(name.to_string());
        let binding = SessionBinding::from_audited(&card, task, lineage);
        let registry = runtime.registry().unwrap();
        registry.register(&binding, "subtask", Utc::now()).unwrap();
        binding
    }

    fn mail(to: &SessionBinding) -> crate::hub::message::SendRequest {
        crate::hub::message::SendRequest {
            to: crate::hub::message::Recipient::Session {
                id: to.session().clone(),
            },
            kind: crate::hub::message::MessageKind::Info,
            subject: "status".to_string(),
            body: "hello".to_string(),
            in_reply_to: None,
        }
    }

    fn rules() -> AuditorFactory {
        constant_auditors(Arc::new(crate::action_auditor::RuleMessageAuditor::new()))
    }

    fn assembled(tally: &Arc<Tally>) -> HubRuntime {
        let sessions = Arc::new(InMemorySessionStore::new());
        let messages = Arc::new(InMemoryMessageStore::new());
        let bell = tally.clone();
        HubRuntime::assemble_with(sessions, messages, rules(), Arc::new(SystemClock), bell)
    }

    #[tokio::test]
    async fn an_assembled_runtime_rings_the_doorbell_it_was_given_after_a_delivery() {
        let tally = Arc::new(Tally::default());
        let runtime = assembled(&tally);
        let (a, b) = (seat(&runtime, "a"), seat(&runtime, "b"));
        runtime.hub().unwrap().send(&a, mail(&b)).await.unwrap();
        assert_eq!(*tally.0.lock().unwrap(), [1]);
    }

    #[tokio::test]
    async fn an_opened_runtime_rings_the_doorbell_it_was_given_and_the_default_rings_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let tally = Arc::new(Tally::default());
        let (clock, bell) = (Arc::new(SystemClock), tally.clone());
        let runtime = HubRuntime::open_with(&paths(dir.path()), rules(), clock, bell);
        let (a, b) = (seat(&runtime, "a"), seat(&runtime, "b"));
        runtime.hub().unwrap().send(&a, mail(&b)).await.unwrap();
        assert_eq!(*tally.0.lock().unwrap(), [1]);
        let sessions = Arc::new(InMemorySessionStore::new());
        let messages = Arc::new(InMemoryMessageStore::new());
        let clock = Arc::new(SystemClock);
        let quiet = HubRuntime::assemble(sessions, messages, rules(), clock);
        let (c, d) = (seat(&quiet, "c"), seat(&quiet, "d"));
        quiet.hub().unwrap().send(&c, mail(&d)).await.unwrap();
        assert_eq!(*tally.0.lock().unwrap(), [1]);
    }

    #[test]
    fn open_with_doorbell_builds_an_available_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let tally = Arc::new(Tally::default());
        let opened = HubRuntime::open_with_doorbell(&paths(dir.path()), provider(), "m", tally);
        assert!(opened.is_available());
    }

    #[test]
    fn an_unavailable_runtime_has_a_dangling_router() {
        assert!(HubRuntime::unavailable().router_weak().upgrade().is_none());
    }

    #[test]
    fn an_unavailable_runtime_attaches_nothing() {
        let manager = HubRuntime::unavailable().attach(TaskManager::new(0));
        assert!(manager.hub().is_none());
    }

    #[test]
    fn an_available_runtime_attaches_its_hub() {
        let auditor = FailClosedMessageAuditor::new(ModelMessageAuditor::new(provider(), "m"));
        let runtime = HubRuntime::assemble(
            Arc::new(InMemorySessionStore::new()),
            Arc::new(InMemoryMessageStore::new()),
            constant_auditors(Arc::new(auditor)),
            Arc::new(SystemClock),
        );
        let manager = runtime.attach(TaskManager::new(0));
        assert!(Arc::ptr_eq(
            &manager.hub().unwrap(),
            &runtime.hub().unwrap()
        ));
    }

    mod orphan_sweep {
        use super::*;
        use crate::hub::lineage::{Lineage, RootId, SessionBinding, SessionId};
        use crate::hub::registry::test_support::Flaky;
        use crate::hub::registry::ORPHAN_SUMMARY;
        use crate::hub::session::{SessionRecord, SessionStatus};
        use crate::leases::SimulatedClock;
        use crate::scheduler::{HybridPolicy, InMemoryQueueStore, QueueStore, QueuedTask};
        use crate::task::TaskStatus;
        use std::sync::atomic::Ordering;
        use std::sync::Mutex;

        const PLANTED: &str = "PLANTED-ORPHAN-SUBTASK-58";

        #[derive(Clone, Default)]
        struct LogBuf(Arc<Mutex<Vec<u8>>>);

        impl std::io::Write for LogBuf {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        fn capture() -> (LogBuf, tracing::subscriber::DefaultGuard) {
            let buf = LogBuf::default();
            let writer = buf.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_ansi(false)
                .finish();
            (buf, tracing::subscriber::set_default(subscriber))
        }

        fn text(buf: &LogBuf) -> String {
            String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
        }

        fn catalog() -> crate::identity::IdentityCatalog {
            crate::identity::IdentityCatalog::load(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/identities/global"
            ))
            .unwrap()
        }

        fn queued(dir: &Path, text: &str) -> QueuedTask {
            QueuedTask::new(text, dir.to_path_buf(), "HEAD", "mock", 20)
                .with_identity_hint(Some("pr-shepherd".to_string()))
        }

        fn lineage() -> Lineage {
            Lineage::root_child(RootId::mcp(uuid::Uuid::from_u128(7)))
        }

        fn stale(name: &str, status: SessionStatus, last: DateTime<Utc>) -> SessionRecord {
            let binding = SessionBinding::from_audited(
                &crate::identity::example(),
                crate::task::TaskId(name.to_string()),
                lineage(),
            );
            let scratch = Arc::new(InMemorySessionStore::new());
            let origin = SessionRegistry::new(scratch.clone());
            origin.register(&binding, PLANTED, last).unwrap();
            let mut record = scratch.get(binding.session().clone()).unwrap().unwrap();
            record.status = status;
            record.ended_at = (!status.is_active()).then_some(last);
            record
        }

        fn id(name: &str) -> SessionId {
            SessionId::from_task(crate::task::TaskId(name.to_string()))
        }

        fn assembled(sessions: Arc<dyn SessionStore>, clock: Arc<dyn Clock>) -> HubRuntime {
            let auditor = FailClosedMessageAuditor::new(ModelMessageAuditor::new(provider(), "m"));
            let auditors: Arc<dyn MessageAuditor> = Arc::new(auditor);
            let factory = constant_auditors(auditors);
            HubRuntime::assemble(
                sessions,
                Arc::new(InMemoryMessageStore::new()),
                factory,
                clock,
            )
        }

        async fn restore_with(runtime: &HubRuntime, entries: &[&QueuedTask]) -> TaskManager {
            let queue = InMemoryQueueStore::default();
            for entry in entries {
                queue.insert(entry).unwrap();
            }
            let gate = crate::auditor::test_support::fixture_gate();
            let leases = Arc::new(crate::leases::InMemoryLeaseStore::default());
            let parts = RestoreParts {
                max_concurrent_tasks: 0,
                policy: Box::new(HybridPolicy::default()),
                store: Box::new(queue),
                leases: leases.clone(),
                provider: provider(),
                identities: &catalog(),
                escalations: Arc::new(crate::escalation::EscalationLog::in_memory()).into(),
                action_gate: TaskManager::action_policy_gate(
                    leases,
                    crate::windows::WindowSet::default(),
                    None,
                    crate::action_auditor::ActionAuditLog::in_memory(),
                ),
                spawn_gate: &gate,
            };
            runtime.restore(parts).await.unwrap()
        }

        fn later() -> DateTime<Utc> {
            Utc::now() + chrono::Duration::days(30)
        }

        #[tokio::test]
        async fn restore_registers_every_queued_session_before_it_closes_the_orphans() {
            let (buf, _guard) = capture();
            std::io::Write::flush(&mut buf.clone()).unwrap();
            let repo = tempfile::tempdir().unwrap();
            let restored = queued(repo.path(), "Check PR status");
            let store = Arc::new(InMemorySessionStore::new());
            let clock = Arc::new(SimulatedClock::new(later()));
            let started = clock.now();
            let old = started - chrono::Duration::hours(2);
            let running = SessionStatus::Running;
            store.upsert(stale(&restored.id.0, running, old)).unwrap();
            store.upsert(stale("orphan", running, old)).unwrap();
            store
                .upsert(stale(
                    "orphan-waiting",
                    SessionStatus::WaitingOnMessages,
                    old,
                ))
                .unwrap();
            store
                .upsert(stale("done", SessionStatus::Completed, old))
                .unwrap();
            let live_at = started + chrono::Duration::minutes(1);
            store.upsert(stale("live", running, live_at)).unwrap();
            let runtime = assembled(store.clone(), clock.clone());
            clock.advance(chrono::Duration::hours(1));
            let manager = restore_with(&runtime, &[&restored]).await;
            let registry = runtime.registry().unwrap();
            let kept = store.get(id(&restored.id.0)).unwrap().unwrap();
            assert_eq!(kept.status, SessionStatus::Pending);
            assert!(registry.tracks(&id(&restored.id.0)));
            let polled = manager.poll(&restored.id).await.unwrap().status;
            assert!(!matches!(polled, TaskStatus::Failed { .. }), "{polled:?}");
            for name in ["orphan", "orphan-waiting"] {
                let closed = store.get(id(name)).unwrap().unwrap();
                assert_eq!(closed.status, SessionStatus::Failed, "{name}");
                assert_eq!(
                    closed.result_summary.as_deref(),
                    Some(ORPHAN_SUMMARY),
                    "{name}"
                );
                assert_eq!(closed.ended_at, Some(clock.now()), "{name}");
            }
            let live = store.get(id("live")).unwrap().unwrap();
            assert_eq!(live.status, SessionStatus::Running);
            let done = store.get(id("done")).unwrap().unwrap();
            assert_eq!(done.status, SessionStatus::Completed);
            assert_eq!(done.result_summary, None);
            let logs = text(&buf);
            assert!(logs.contains("closed=2"), "{logs}");
            assert!(!logs.contains(PLANTED), "{logs}");
        }

        #[tokio::test]
        async fn the_start_time_is_taken_when_the_runtime_is_assembled_not_when_the_sweep_runs() {
            let store = Arc::new(InMemorySessionStore::new());
            let clock = Arc::new(SimulatedClock::new(later()));
            let started = clock.now();
            let running = SessionStatus::Running;
            let between = started + chrono::Duration::minutes(30);
            store.upsert(stale("between", running, between)).unwrap();
            let before = started - chrono::Duration::seconds(1);
            store.upsert(stale("before", running, before)).unwrap();
            let runtime = assembled(store.clone(), clock.clone());
            clock.advance(chrono::Duration::hours(1));
            let _manager = restore_with(&runtime, &[]).await;
            assert_eq!(store.get(id("between")).unwrap().unwrap().status, running);
            let swept = store.get(id("before")).unwrap().unwrap();
            assert_eq!(swept.status, SessionStatus::Failed);
            assert_eq!(swept.ended_at, Some(clock.now()));
        }

        #[tokio::test]
        async fn a_failed_listing_is_logged_without_text_and_the_restore_still_returns() {
            let (buf, _guard) = capture();
            let flaky = Flaky::new();
            let old = Utc::now() - chrono::Duration::hours(2);
            flaky
                .inner
                .upsert(stale("orphan", SessionStatus::Running, old))
                .unwrap();
            let clock = Arc::new(SimulatedClock::new(later()));
            let runtime = assembled(flaky.clone(), clock);
            flaky.reads.store(true, Ordering::SeqCst);
            let _manager = restore_with(&runtime, &[]).await;
            flaky.reads.store(false, Ordering::SeqCst);
            let orphan = flaky.inner.get(id("orphan")).unwrap().unwrap();
            assert_eq!(orphan.status, SessionStatus::Running);
            let logs = text(&buf);
            assert!(logs.contains("orphan sweep failed"), "{logs}");
            assert!(!logs.contains("closed="), "{logs}");
            assert!(!logs.contains(PLANTED), "{logs}");
        }
    }
}
