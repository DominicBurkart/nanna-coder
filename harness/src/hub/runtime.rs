//! The production assembly of the message hub: the stores, the one session registry and the hub.

use super::oracle::SystemGit;
use super::registry::SessionRegistry;
use super::router::{BindingTable, HubRouter, ProductionAuditors};
use super::send::{AuditorProvider, MessageHub};
use super::store::{
    messages_path_from, sessions_path_from, HubLock, JsonlMessageStore, JsonlSessionStore,
    MessageStore, SessionStore, StoreError,
};
use crate::leases::{Clock, SystemClock};
use crate::scheduler::QueueStoreError;
use crate::task::{RestoreParts, TaskManager, RESTORE_AUDIT_DEADLINE};
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
    router: Arc<HubRouter>,
    _locks: Vec<Arc<HubLock>>,
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
        HubRuntime::open_with(paths, factory, Arc::new(SystemClock))
    }

    pub(crate) fn open_with(
        paths: &HubPaths,
        auditors: AuditorFactory,
        clock: Arc<dyn Clock>,
    ) -> Self {
        match open_stores(paths) {
            Ok((sessions, messages, locks)) => {
                let mut runtime = HubRuntime::assemble(sessions, messages, auditors, clock);
                if let Some(parts) = runtime.parts.as_mut() {
                    parts._locks = locks;
                }
                runtime
            }
            Err(error) => {
                tracing::error!(%error, "message hub unavailable: its stores could not be opened; message tools are disabled");
                Self::unavailable()
            }
        }
    }

    pub(crate) fn assemble(
        sessions: Arc<dyn SessionStore>,
        messages: Arc<dyn MessageStore>,
        auditors: AuditorFactory,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let table = BindingTable::new();
        let auditors = auditors(&sessions, &messages, &table);
        let registry = Arc::new(SessionRegistry::new(Arc::clone(&sessions)));
        let hub = MessageHub::new(
            sessions,
            messages,
            auditors,
            clock,
            Default::default(),
            Arc::clone(&registry),
        );
        Self {
            parts: Some(Parts {
                registry,
                router: Arc::new(HubRouter::new(Arc::new(hub), table)),
                _locks: Vec::new(),
            }),
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
}
