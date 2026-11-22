//! The production assembly of the message hub: the stores, the one session registry and the hub.

use super::registry::SessionRegistry;
use super::send::{AuditorProvider, MessageHub};
use super::store::{
    messages_path_from, sessions_path_from, HubLock, JsonlMessageStore, JsonlSessionStore,
    MessageStore, SessionStore, StoreError,
};
use crate::action_auditor::{FailClosedMessageAuditor, MessageAuditor, ModelMessageAuditor};
use crate::leases::{Clock, SystemClock};
use crate::scheduler::QueueStoreError;
use crate::task::{RestoreParts, TaskManager, RESTORE_AUDIT_DEADLINE};
use model::ModelProvider;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

struct Parts {
    registry: Arc<SessionRegistry>,
    hub: Arc<MessageHub>,
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
        let auditor = FailClosedMessageAuditor::new(ModelMessageAuditor::new(provider, model));
        let auditors: Arc<dyn MessageAuditor> = Arc::new(auditor);
        HubRuntime::open_with(paths, Arc::new(auditors), Arc::new(SystemClock))
    }

    pub(crate) fn open_with(
        paths: &HubPaths,
        auditors: Arc<dyn AuditorProvider>,
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
        auditors: Arc<dyn AuditorProvider>,
        clock: Arc<dyn Clock>,
    ) -> Self {
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
                hub: Arc::new(hub),
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
        self.parts.as_ref().map(|parts| Arc::clone(&parts.hub))
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
                .with_hub(Arc::clone(&parts.hub)),
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
                Ok(manager.with_hub(Arc::clone(&hub_parts.hub)))
            }
            None => TaskManager::restore_without_sessions(parts, RESTORE_AUDIT_DEADLINE).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::store::{InMemoryMessageStore, InMemorySessionStore};
    use std::fs;

    struct Offline;

    #[async_trait::async_trait]
    impl ModelProvider for Offline {
        async fn chat(
            &self,
            _request: model::types::ChatRequest,
        ) -> model::ModelResult<model::types::ChatResponse> {
            Err(model::ModelError::ServiceUnavailable {
                message: "offline".to_string(),
            })
        }

        async fn list_models(&self) -> model::ModelResult<Vec<model::types::ModelInfo>> {
            Ok(Vec::new())
        }

        async fn health_check(&self) -> model::ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "offline"
        }
    }

    fn provider() -> Arc<dyn ModelProvider> {
        Arc::new(Offline)
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
    fn an_unavailable_runtime_attaches_nothing() {
        let manager = HubRuntime::unavailable().attach(TaskManager::new(0));
        assert!(manager.hub().is_none());
    }

    #[test]
    fn an_available_runtime_attaches_its_hub() {
        let runtime = HubRuntime::assemble(
            Arc::new(InMemorySessionStore::new()),
            Arc::new(InMemoryMessageStore::new()),
            Arc::new(
                Arc::new(FailClosedMessageAuditor::new(ModelMessageAuditor::new(
                    provider(),
                    "m",
                ))) as Arc<dyn MessageAuditor>,
            ),
            Arc::new(SystemClock),
        );
        let manager = runtime.attach(TaskManager::new(0));
        assert!(Arc::ptr_eq(
            &manager.hub().unwrap(),
            &runtime.hub().unwrap()
        ));
    }
}
