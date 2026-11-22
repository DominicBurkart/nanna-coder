use crate::task::{FailureDiagnostics, Task, TaskId, TaskStatus};
use chrono::{DateTime, Utc};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const STATE_DIR_ENV: &str = "NANNA_STATE_DIR";
pub const RETENTION_DAYS_ENV: &str = "NANNA_RESULT_RETENTION_DAYS";
pub const DEFAULT_RETENTION_DAYS: u64 = 14;
pub const RESULTS_DIR: &str = "results";
pub const MAX_RESULT_BYTES: u64 = 64 * 1024 * 1024;
pub const PRUNE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum ResultStoreError {
    #[error("result store {path}: {reason}")]
    Unsafe { path: PathBuf, reason: String },
    #[error("result store io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("task id {0:?} cannot name a result file")]
    BadId(String),
    #[error("result file {path} does not parse: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> ResultStoreError + '_ {
    move |source| ResultStoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

pub fn state_dir_from(explicit: Option<OsString>, queue_path: &Path) -> PathBuf {
    match explicit.filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => queue_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    }
}

pub fn retention_from(raw: Option<OsString>) -> Duration {
    let days = raw
        .and_then(|value| value.into_string().ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .filter(|days| *days > 0)
        .unwrap_or(DEFAULT_RETENTION_DAYS);
    Duration::from_secs(days.saturating_mul(24 * 60 * 60))
}

pub fn is_parked_failure(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Failed { diagnostics, .. }
            if matches!(
                diagnostics.error_type.as_str(),
                "RestoreAuditFailed" | "RestoreAuditDeferred"
            )
    )
}

pub fn daemon_restarted_status(now: DateTime<Utc>, iterations: usize) -> TaskStatus {
    TaskStatus::Failed {
        finished_at: now,
        error: "the daemon restarted while this task was running and no result was recorded; resubmit the task (retryable)".to_string(),
        diagnostics: FailureDiagnostics {
            changes: None,
            error_type: "DaemonRestarted".to_string(),
            iterations_completed: iterations,
            last_tool_call: None,
            partial_changes: None,
            tool_call_history: vec![],
            last_agent_state: None,
            conversation_snapshot: None,
            denials: vec![],
            action_audit: vec![],
        },
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Debug, Clone)]
pub struct ResultStore {
    dir: PathBuf,
    retention: Duration,
    gate: std::sync::Arc<std::sync::Mutex<()>>,
}

impl ResultStore {
    pub fn open(state_dir: &Path, retention: Duration) -> Result<Self, ResultStoreError> {
        let dir = state_dir.join(RESULTS_DIR);
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(&dir).map_err(io_at(&dir))?;
        let meta = std::fs::symlink_metadata(&dir).map_err(io_at(&dir))?;
        if !meta.file_type().is_dir() {
            return Err(ResultStoreError::Unsafe {
                path: dir,
                reason: "not a real directory (symlinks are not followed)".to_string(),
            });
        }
        Ok(Self {
            dir,
            retention,
            gate: std::sync::Arc::default(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn retention(&self) -> Duration {
        self.retention
    }

    fn path_for(&self, id: &TaskId) -> Result<PathBuf, ResultStoreError> {
        if !valid_id(&id.0) {
            return Err(ResultStoreError::BadId(id.0.clone()));
        }
        Ok(self.dir.join(format!("{}.json", id.0)))
    }

    pub fn persist(&self, task: &Task) -> Result<(), ResultStoreError> {
        let target = self.path_for(&task.id)?;
        let body = serde_json::to_vec(task).map_err(|source| ResultStoreError::Parse {
            path: target.clone(),
            source,
        })?;
        let temp = self.dir.join(format!(
            ".{}.{}.tmp",
            task.id.0,
            uuid::Uuid::new_v4().simple()
        ));
        let written = (|| -> std::io::Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NOCTTY);
            }
            let mut file = options.open(&temp)?;
            file.write_all(&body)?;
            file.sync_all()?;
            std::fs::rename(&temp, &target)
        })();
        if let Err(source) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(ResultStoreError::Io {
                path: target,
                source,
            });
        }
        if let Ok(dir) = std::fs::File::open(&self.dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    pub fn persist_latest(
        &self,
        read: impl FnOnce() -> Option<Task>,
    ) -> Result<bool, ResultStoreError> {
        let _turn = self
            .gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(task) = read() else {
            return Ok(false);
        };
        let durable =
            matches!(task.status, TaskStatus::Running { .. }) || task.status.is_terminal();
        if !durable || is_parked_failure(&task.status) {
            return Ok(false);
        }
        self.persist(&task)?;
        Ok(true)
    }

    fn read_regular(path: &Path) -> Result<Vec<u8>, ResultStoreError> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY);
        }
        let file = options.open(path).map_err(io_at(path))?;
        let meta = file.metadata().map_err(io_at(path))?;
        if !meta.file_type().is_file() {
            return Err(ResultStoreError::Unsafe {
                path: path.to_path_buf(),
                reason: "not a regular file".to_string(),
            });
        }
        if meta.len() > MAX_RESULT_BYTES {
            return Err(ResultStoreError::Unsafe {
                path: path.to_path_buf(),
                reason: format!("larger than {MAX_RESULT_BYTES} bytes"),
            });
        }
        let mut body = Vec::with_capacity(meta.len() as usize);
        file.take(MAX_RESULT_BYTES)
            .read_to_end(&mut body)
            .map_err(io_at(path))?;
        Ok(body)
    }

    pub fn load(&self, id: &TaskId) -> Result<Task, ResultStoreError> {
        let path = self.path_for(id)?;
        let body = Self::read_regular(&path)?;
        let task: Task =
            serde_json::from_slice(&body).map_err(|source| ResultStoreError::Parse {
                path: path.clone(),
                source,
            })?;
        if task.id != *id {
            return Err(ResultStoreError::Unsafe {
                path,
                reason: format!("file holds task {} but is named for {}", task.id, id),
            });
        }
        Ok(task)
    }

    fn ids(&self) -> Vec<TaskId> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let stem = name.strip_suffix(".json")?;
                valid_id(stem).then(|| TaskId(stem.to_string()))
            })
            .collect()
    }

    pub fn load_all(&self) -> Vec<Task> {
        let mut tasks: Vec<Task> = self
            .ids()
            .into_iter()
            .filter_map(|id| match self.load(&id) {
                Ok(task) => Some(task),
                Err(error) => {
                    tracing::warn!(task_id = %id, %error, "Skipping an unreadable persisted task result");
                    None
                }
            })
            .collect();
        tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        tasks
    }

    pub fn interrupted_ids(&self) -> Vec<TaskId> {
        self.load_all()
            .into_iter()
            .filter(|task| matches!(task.status, TaskStatus::Running { .. }))
            .map(|task| task.id)
            .collect()
    }

    pub fn prune(&self, now: SystemTime) -> usize {
        let Some(cutoff) = now.checked_sub(self.retention) else {
            return 0;
        };
        let mut removed = 0;
        for id in self.ids() {
            let Ok(path) = self.path_for(&id) else {
                continue;
            };
            let modified = std::fs::symlink_metadata(&path).and_then(|meta| meta.modified());
            if matches!(modified, Ok(at) if at < cutoff) && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') && name.ends_with(".tmp") {
                    let stale = entry
                        .metadata()
                        .and_then(|meta| meta.modified())
                        .map(|at| at < cutoff)
                        .unwrap_or(false);
                    if stale {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
        removed
    }
}

pub fn spawn_pruning(
    manager: std::sync::Arc<crate::task::TaskManager>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRUNE_EVERY);
        tick.tick().await;
        loop {
            tick.tick().await;
            let removed = manager.prune_results().await;
            if removed > 0 {
                tracing::info!(removed, "Pruned expired task results");
            }
        }
    })
}

pub fn drop_interrupted(
    queue: &dyn crate::scheduler::QueueStore,
    results: &ResultStore,
) -> Result<Vec<TaskId>, crate::scheduler::QueueStoreError> {
    let interrupted = results.interrupted_ids();
    for id in &interrupted {
        queue.remove(id)?;
    }
    Ok(interrupted)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn task(id: &str, status: TaskStatus) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId(id.to_string()),
            description: "d".into(),
            repo_path: PathBuf::from("/repos/r"),
            branch: "HEAD".into(),
            model: "m".into(),
            status,
            created_at: now,
            last_updated_at: now,
            ttl_ms: None,
            not_before: None,
            identity_hint: Some("rust-implementer".into()),
            origin: None,
            lineage: None,
        }
    }

    fn cancelled() -> TaskStatus {
        TaskStatus::Cancelled {
            finished_at: Utc::now(),
            iterations_completed: 3,
        }
    }

    fn running() -> TaskStatus {
        TaskStatus::Running {
            started_at: Utc::now(),
            iterations: 0,
        }
    }

    fn store(dir: &Path) -> ResultStore {
        ResultStore::open(dir, Duration::from_secs(14 * 86400)).unwrap()
    }

    #[test]
    fn state_dir_defaults_beside_the_queue_and_honours_an_override() {
        let queue = Path::new("/state/queue.jsonl");
        assert_eq!(state_dir_from(None, queue), PathBuf::from("/state"));
        assert_eq!(
            state_dir_from(Some("/else".into()), queue),
            PathBuf::from("/else")
        );
        assert_eq!(
            state_dir_from(Some("".into()), queue),
            PathBuf::from("/state")
        );
        assert_eq!(
            state_dir_from(None, Path::new("queue.jsonl")),
            PathBuf::from(".")
        );
    }

    #[test]
    fn retention_defaults_to_fourteen_days_and_rejects_nonsense() {
        let days = |n: u64| Duration::from_secs(n * 86400);
        assert_eq!(retention_from(None), days(14));
        assert_eq!(retention_from(Some("3".into())), days(3));
        assert_eq!(retention_from(Some(" 30 ".into())), days(30));
        for bad in ["0", "-1", "abc", ""] {
            assert_eq!(retention_from(Some(bad.into())), days(14), "{bad}");
        }
    }

    #[test]
    fn a_persisted_task_loads_back_equal() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let original = task("abc-123", cancelled());
        store.persist(&original).unwrap();
        let back = store.load(&original.id).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert_eq!(store.load_all().len(), 1);
    }

    #[test]
    fn persisting_again_replaces_the_record_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.persist(&task("t1", running())).unwrap();
        store.persist(&task("t1", cancelled())).unwrap();
        let loaded = store.load(&TaskId("t1".into())).unwrap();
        assert!(matches!(loaded.status, TaskStatus::Cancelled { .. }));
        let names: Vec<String> = std::fs::read_dir(store.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["t1.json".to_string()]);
    }

    #[test]
    fn files_are_private_regular_files_in_a_private_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.persist(&task("t1", cancelled())).unwrap();
        let file = std::fs::metadata(store.dir().join("t1.json")).unwrap();
        assert!(file.is_file());
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let results = std::fs::metadata(store.dir()).unwrap();
        assert_eq!(results.permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn a_symlinked_results_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join(RESULTS_DIR)).unwrap();
        let refused = ResultStore::open(dir.path(), Duration::from_secs(60));
        assert!(matches!(refused, Err(ResultStoreError::Unsafe { .. })));
    }

    #[test]
    fn a_symlink_planted_at_a_result_path_is_replaced_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let victim = tempfile::tempdir().unwrap();
        let target = victim.path().join("victim");
        std::fs::write(&target, "keep").unwrap();
        let store = store(dir.path());
        std::os::unix::fs::symlink(&target, store.dir().join("t1.json")).unwrap();
        store.persist(&task("t1", cancelled())).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        assert!(std::fs::symlink_metadata(store.dir().join("t1.json"))
            .unwrap()
            .is_file());
    }

    #[test]
    fn a_symlink_or_fifo_at_a_result_path_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.persist(&task("good", cancelled())).unwrap();
        let real = dir.path().join("outside.json");
        std::fs::write(
            &real,
            serde_json::to_vec(&task("link", cancelled())).unwrap(),
        )
        .unwrap();
        std::os::unix::fs::symlink(&real, store.dir().join("link.json")).unwrap();
        let fifo = std::ffi::CString::new(store.dir().join("pipe.json").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let ids: Vec<String> = store.load_all().into_iter().map(|t| t.id.0).collect();
        assert_eq!(ids, vec!["good".to_string()]);
    }

    #[test]
    fn hostile_ids_cannot_name_a_path_outside_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        for id in ["../escape", "a/b", "", ".hidden", "a b", "x\0y"] {
            let refused = store.persist(&task(id, cancelled()));
            assert!(matches!(refused, Err(ResultStoreError::BadId(_))), "{id:?}");
        }
    }

    #[test]
    fn a_file_named_for_one_task_but_holding_another_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        std::fs::write(
            store.dir().join("a.json"),
            serde_json::to_vec(&task("b", cancelled())).unwrap(),
        )
        .unwrap();
        assert!(store.load_all().is_empty());
    }

    #[test]
    fn corrupt_and_unrelated_files_are_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        std::fs::write(store.dir().join("bad.json"), b"{not json").unwrap();
        std::fs::write(store.dir().join("notes.txt"), b"hi").unwrap();
        store.persist(&task("ok", cancelled())).unwrap();
        let ids: Vec<String> = store.load_all().into_iter().map(|t| t.id.0).collect();
        assert_eq!(ids, vec!["ok".to_string()]);
    }

    #[test]
    fn older_files_missing_optional_fields_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let mut value = serde_json::to_value(task("old", cancelled())).unwrap();
        let object = value.as_object_mut().unwrap();
        for key in ["not_before", "identity_hint", "origin", "lineage", "ttl_ms"] {
            object.remove(key);
        }
        std::fs::write(
            store.dir().join("old.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let loaded = store.load(&TaskId("old".into())).unwrap();
        assert!(loaded.identity_hint.is_none() && loaded.lineage.is_none());
    }

    #[test]
    fn interrupted_ids_lists_only_records_left_running() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.persist(&task("run", running())).unwrap();
        store.persist(&task("done", cancelled())).unwrap();
        assert_eq!(store.interrupted_ids(), vec![TaskId("run".into())]);
    }

    #[test]
    fn prune_removes_only_records_older_than_the_retention() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.persist(&task("old", cancelled())).unwrap();
        store.persist(&task("new", cancelled())).unwrap();
        let age = |name: &str, days: u64| {
            let file = std::fs::File::options()
                .write(true)
                .open(store.dir().join(name))
                .unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(days * 86400))
                .unwrap();
        };
        age("old.json", 15);
        age("new.json", 13);
        assert_eq!(store.prune(SystemTime::now()), 1);
        let ids: Vec<String> = store.load_all().into_iter().map(|t| t.id.0).collect();
        assert_eq!(ids, vec!["new".to_string()]);
    }

    #[test]
    fn prune_honours_a_shorter_retention() {
        let dir = tempfile::tempdir().unwrap();
        let store = ResultStore::open(dir.path(), Duration::from_secs(86400)).unwrap();
        store.persist(&task("t", cancelled())).unwrap();
        assert_eq!(store.prune(SystemTime::now()), 0);
        assert_eq!(
            store.prune(SystemTime::now() + Duration::from_secs(2 * 86400)),
            1
        );
    }

    #[test]
    fn persist_latest_writes_what_the_reader_returns_at_write_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let mut slot = Some(task("t", running()));
        assert!(store.persist_latest(|| slot.take()).unwrap());
        assert!(matches!(
            store.load(&TaskId("t".into())).unwrap().status,
            TaskStatus::Running { .. }
        ));
        let mut newer = Some(task("t", cancelled()));
        assert!(store.persist_latest(|| newer.take()).unwrap());
        assert!(matches!(
            store.load(&TaskId("t".into())).unwrap().status,
            TaskStatus::Cancelled { .. }
        ));
    }

    #[test]
    fn persist_latest_skips_missing_pending_and_parked_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        assert!(!store.persist_latest(|| None).unwrap());
        assert!(!store
            .persist_latest(|| Some(task("p", TaskStatus::Pending)))
            .unwrap());
        let mut parked = daemon_restarted_status(Utc::now(), 0);
        if let TaskStatus::Failed { diagnostics, .. } = &mut parked {
            diagnostics.error_type = "RestoreAuditFailed".into();
        }
        assert!(!store.persist_latest(|| Some(task("q", parked))).unwrap());
        assert!(store.load_all().is_empty());
    }

    #[test]
    fn parked_failures_are_recognised() {
        let mut parked = daemon_restarted_status(Utc::now(), 0);
        if let TaskStatus::Failed { diagnostics, .. } = &mut parked {
            diagnostics.error_type = "RestoreAuditDeferred".into();
        }
        assert!(is_parked_failure(&parked));
        assert!(!is_parked_failure(&daemon_restarted_status(Utc::now(), 0)));
        assert!(!is_parked_failure(&cancelled()));
    }
}
