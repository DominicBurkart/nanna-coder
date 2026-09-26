//! The running application of a task and the registry that tracks it.

use super::ports::PortLease;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// Wall-clock budget for `app_start` when no limits are configured.
pub const DEFAULT_MAX_WALL_CLOCK_SECS: u64 = 120;

/// Resource bounds for starting the application.
///
/// A plain struct for now; the identity `[limits]` table will populate it
/// once identities land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Hard bound on the whole `app_start` operation: frontend build, API
    /// build, process start and health wait together.
    pub max_wall_clock_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_wall_clock_secs: DEFAULT_MAX_WALL_CLOCK_SECS,
        }
    }
}

impl Limits {
    /// The wall-clock bound as a duration.
    pub fn max_wall_clock(&self) -> Duration {
        Duration::from_secs(self.max_wall_clock_secs)
    }
}

/// A started application, as reported to the agent.
///
/// `api_url` serves the JSON API and `frontend_url` serves the wasm bundle.
/// For the full-stack Rust profile the API binary serves both, so the three
/// URLs are the same origin; they stay separate fields so QA tooling can
/// target each without knowing the profile.
///
/// ```
/// use harness::apprun::AppInstance;
///
/// let app = AppInstance::local("task-7", 18001, 512, "/tmp/nanna-app-task-7.log");
/// assert_eq!(app.base_url, "http://127.0.0.1:18001");
/// assert_eq!(app.api_url, app.base_url);
/// assert_eq!(app.frontend_url, app.base_url);
/// assert_eq!(app.to_json()["pid"], 512);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInstance {
    pub task_id: String,
    pub base_url: String,
    pub api_url: String,
    pub frontend_url: String,
    pub pid: u32,
    pub log_path: String,
    pub port: u16,
}

impl AppInstance {
    /// An instance whose API and frontend are both served by the process
    /// listening on `port` inside the dev container.
    pub fn local(task_id: &str, port: u16, pid: u32, log_path: &str) -> Self {
        let base_url = format!("http://127.0.0.1:{port}");
        Self {
            task_id: task_id.to_string(),
            api_url: base_url.clone(),
            frontend_url: base_url.clone(),
            base_url,
            pid,
            log_path: log_path.to_string(),
            port,
        }
    }

    /// The tool-result shape: `{ task_id, base_url, api_url, frontend_url,
    /// pid, log_path, port }`.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("AppInstance serialises")
    }
}

#[derive(Debug)]
struct RunningApp {
    instance: AppInstance,
    _lease: PortLease,
}

/// Running applications keyed by task id, each holding its port lease.
///
/// Removing an entry drops its lease, which releases the port.
#[derive(Debug, Default)]
pub struct RunningApps {
    inner: Mutex<HashMap<String, RunningApp>>,
}

impl RunningApps {
    pub fn new() -> Self {
        Self::default()
    }

    /// The instance running for `task_id`, if any.
    pub fn get(&self, task_id: &str) -> Option<AppInstance> {
        self.lock().get(task_id).map(|app| app.instance.clone())
    }

    /// Record `instance` as running with `lease`; replaces (and releases)
    /// any previous entry for the same task.
    pub fn insert(&self, instance: AppInstance, lease: PortLease) {
        let task_id = instance.task_id.clone();
        self.lock().insert(
            task_id,
            RunningApp {
                instance,
                _lease: lease,
            },
        );
    }

    /// Forget the instance of `task_id`, releasing its port.
    pub fn remove(&self, task_id: &str) -> Option<AppInstance> {
        self.lock().remove(task_id).map(|app| app.instance)
    }

    /// Task ids with a running instance.
    pub fn task_ids(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, RunningApp>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprun::PortAllocator;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn allocator() -> (Arc<PortAllocator>, TempDir) {
        let dir = TempDir::new().unwrap();
        (Arc::new(PortAllocator::new(40000..=40009, dir.path())), dir)
    }

    #[test]
    fn local_instance_serves_api_and_frontend_from_one_origin() {
        let instance = AppInstance::local("task-1", 18000, 4242, "/tmp/nanna-app-task-1.log");
        assert_eq!(instance.base_url, "http://127.0.0.1:18000");
        assert_eq!(instance.api_url, instance.base_url);
        assert_eq!(instance.frontend_url, instance.base_url);
        assert_eq!(instance.pid, 4242);
        assert_eq!(instance.port, 18000);
        assert_eq!(instance.log_path, "/tmp/nanna-app-task-1.log");
        let json = instance.to_json();
        assert_eq!(json["task_id"], "task-1");
        assert_eq!(json["base_url"], "http://127.0.0.1:18000");
        assert_eq!(json["api_url"], "http://127.0.0.1:18000");
        assert_eq!(json["frontend_url"], "http://127.0.0.1:18000");
        assert_eq!(json["pid"], 4242);
        assert_eq!(json["log_path"], "/tmp/nanna-app-task-1.log");
        assert_eq!(json["port"], 18000);
        let back: AppInstance = serde_json::from_value(json).unwrap();
        assert_eq!(back, instance);
    }

    #[test]
    fn registry_insert_get_remove_releases_the_port() {
        let (allocator, _dir) = allocator();
        let apps = RunningApps::new();
        assert!(apps.is_empty());
        assert!(apps.get("t").is_none());
        let lease = allocator.allocate("t").unwrap();
        let instance = AppInstance::local("t", lease.port(), 1, "/tmp/t.log");
        apps.insert(instance.clone(), lease);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps.get("t"), Some(instance.clone()));
        assert_eq!(apps.task_ids(), vec!["t".to_string()]);
        assert_eq!(allocator.held(), vec![40000]);
        assert_eq!(apps.remove("t"), Some(instance));
        assert!(apps.is_empty());
        assert!(
            allocator.held().is_empty(),
            "removing the entry releases the lease"
        );
        assert_eq!(apps.remove("t"), None);
    }

    #[test]
    fn registry_replaces_an_entry_for_the_same_task() {
        let (allocator, _dir) = allocator();
        let apps = RunningApps::default();
        let first = allocator.allocate("t").unwrap();
        let second = allocator.allocate("t").unwrap();
        apps.insert(AppInstance::local("t", first.port(), 1, "/l"), first);
        apps.insert(AppInstance::local("t", second.port(), 2, "/l"), second);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps.get("t").unwrap().pid, 2);
        assert_eq!(
            allocator.held(),
            vec![40001],
            "the replaced lease is released"
        );
        assert!(format!("{apps:?}").contains("40001"));
    }

    #[test]
    fn limits_default_to_two_minutes() {
        let limits = Limits::default();
        assert_eq!(limits.max_wall_clock_secs, 120);
        assert_eq!(limits.max_wall_clock(), Duration::from_secs(120));
        let parsed: Limits = serde_json::from_str(r#"{"max_wall_clock_secs": 7}"#).unwrap();
        assert_eq!(parsed.max_wall_clock(), Duration::from_secs(7));
    }
}
