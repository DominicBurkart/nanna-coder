use crate::changes::{
    base64_decode, base64_encode, CapturedFile, ChangeCapture, ChangeStatus,
    MAX_FILE_CAPTURE_BYTES, MAX_TOTAL_CAPTURE_BYTES,
};
use crate::task::{FailureDiagnostics, Task, TaskId, TaskStatus};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const STATE_DIR_ENV: &str = "NANNA_STATE_DIR";
pub const RETENTION_DAYS_ENV: &str = "NANNA_RESULT_RETENTION_DAYS";
pub const DEFAULT_RETENTION_DAYS: u64 = 14;
pub const RESULTS_DIR: &str = "results";
pub const MAX_RESULT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_SIDECAR_BYTES: u64 = 64 * 1024 * 1024;
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

const SIDECAR_SUFFIX: &str = ".changes.json";
const SIDECAR_VERSION: u32 = 1;

#[derive(Debug, Default, serde::Deserialize)]
struct Envelope {
    #[serde(default)]
    closes: Option<String>,
    #[serde(default)]
    changes_sidecar: Option<usize>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SidecarFile {
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    old_path: Option<String>,
    status: String,
    binary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_base64: Option<String>,
    size: usize,
    capture_truncated: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Sidecar {
    version: u32,
    task_id: String,
    base_ref: Option<String>,
    patch_original_bytes: Option<usize>,
    files: Vec<SidecarFile>,
}

impl Sidecar {
    fn from_capture(id: &TaskId, capture: &ChangeCapture) -> Self {
        Self {
            version: SIDECAR_VERSION,
            task_id: id.0.clone(),
            base_ref: capture.base_ref.clone(),
            patch_original_bytes: capture.patch_original_bytes,
            files: capture
                .files
                .iter()
                .map(|file| SidecarFile {
                    path: file.path.clone(),
                    old_path: file.old_path.clone(),
                    status: file.status.as_str().to_string(),
                    binary: file.binary,
                    content_base64: file.content.as_deref().map(base64_encode),
                    size: file.size,
                    capture_truncated: file.capture_truncated,
                })
                .collect(),
        }
    }

    fn into_capture(self, id: &TaskId, expected_files: usize) -> Option<ChangeCapture> {
        if self.version != SIDECAR_VERSION
            || self.task_id != id.0
            || self.files.len() != expected_files
        {
            return None;
        }
        let mut total = 0usize;
        let mut files = Vec::with_capacity(self.files.len());
        for file in self.files {
            let content = match file.content_base64 {
                Some(text) => {
                    if text.len() / 4 * 3 > MAX_FILE_CAPTURE_BYTES {
                        return None;
                    }
                    let bytes = base64_decode(&text)?;
                    if bytes.len() > MAX_FILE_CAPTURE_BYTES {
                        return None;
                    }
                    total += bytes.len();
                    if total > MAX_TOTAL_CAPTURE_BYTES {
                        return None;
                    }
                    if file.capture_truncated {
                        if file.size < bytes.len() {
                            return None;
                        }
                    } else if file.size != bytes.len() {
                        return None;
                    }
                    Some(bytes)
                }
                None => None,
            };
            files.push(CapturedFile {
                path: file.path,
                old_path: file.old_path,
                status: ChangeStatus::parse(&file.status)?,
                binary: file.binary,
                content,
                size: file.size,
                capture_truncated: file.capture_truncated,
            });
        }
        Some(ChangeCapture {
            base_ref: self.base_ref,
            files,
            patch_original_bytes: self.patch_original_bytes,
        })
    }
}

#[derive(Debug)]
pub struct LoadedRecord {
    pub task: Task,
    pub closes: Option<String>,
    pub changes_lost: bool,
}

fn capture_of(task: &Task) -> Option<&ChangeCapture> {
    match &task.status {
        TaskStatus::Completed { result, .. } => result.changes.as_ref(),
        TaskStatus::Failed { diagnostics, .. } => diagnostics.changes.as_ref(),
        _ => None,
    }
}

fn attach_capture(task: &mut Task, capture: ChangeCapture) {
    match &mut task.status {
        TaskStatus::Completed { result, .. } => result.changes = Some(capture),
        TaskStatus::Failed { diagnostics, .. } => diagnostics.changes = Some(capture),
        _ => {}
    }
}

#[derive(Debug, Clone)]
pub struct ResultStore {
    dir: PathBuf,
    retention: Duration,
    gate: std::sync::Arc<std::sync::Mutex<()>>,
    closes: std::sync::Arc<std::sync::Mutex<HashMap<TaskId, String>>>,
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
            closes: std::sync::Arc::default(),
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

    fn sidecar_path_for(&self, id: &TaskId) -> Result<PathBuf, ResultStoreError> {
        if !valid_id(&id.0) {
            return Err(ResultStoreError::BadId(id.0.clone()));
        }
        Ok(self.dir.join(format!("{}{SIDECAR_SUFFIX}", id.0)))
    }

    fn closes_map(&self) -> std::sync::MutexGuard<'_, HashMap<TaskId, String>> {
        self.closes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_closes(&self, id: &TaskId, closes: String) {
        let Ok(path) = self.path_for(id) else {
            return;
        };
        let _turn = self
            .gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.closes_map().insert(id.clone(), closes.clone());
        let Ok(body) = Self::read_bounded(&path, MAX_RESULT_BYTES) else {
            return;
        };
        let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
            return;
        };
        let Some(object) = value.as_object_mut() else {
            return;
        };
        object.insert("closes".to_string(), closes.into());
        let Ok(updated) = serde_json::to_vec(&value) else {
            return;
        };
        if let Err(error) = self.write_atomic(&id.0, &path, &updated) {
            tracing::error!(task_id = %id, %error, "Failed to record closes on the task result");
        }
    }

    fn record_bytes(
        &self,
        task: &Task,
        closes: Option<String>,
        changes_sidecar: Option<usize>,
        target: &Path,
    ) -> Result<Vec<u8>, ResultStoreError> {
        let parse = |source| ResultStoreError::Parse {
            path: target.to_path_buf(),
            source,
        };
        if closes.is_none() && changes_sidecar.is_none() {
            return serde_json::to_vec(task).map_err(parse);
        }
        let mut value = serde_json::to_value(task).map_err(parse)?;
        if let Some(object) = value.as_object_mut() {
            if let Some(closes) = closes {
                object.insert("closes".to_string(), closes.into());
            }
            if let Some(files) = changes_sidecar {
                object.insert("changes_sidecar".to_string(), files.into());
            }
        }
        serde_json::to_vec(&value).map_err(parse)
    }

    fn write_sidecar(&self, id: &TaskId, capture: &ChangeCapture) -> Result<(), ResultStoreError> {
        let target = self.sidecar_path_for(id)?;
        let body = serde_json::to_vec(&Sidecar::from_capture(id, capture)).map_err(|source| {
            ResultStoreError::Parse {
                path: target.clone(),
                source,
            }
        })?;
        if body.len() as u64 > MAX_SIDECAR_BYTES {
            return Err(ResultStoreError::Unsafe {
                path: target,
                reason: format!("captured contents exceed {MAX_SIDECAR_BYTES} bytes"),
            });
        }
        self.write_atomic(&id.0, &target, &body)
    }

    pub fn persist(&self, task: &Task) -> Result<(), ResultStoreError> {
        let target = self.path_for(&task.id)?;
        let changes_sidecar = match capture_of(task) {
            Some(capture) => {
                if let Err(error) = self.write_sidecar(&task.id, capture) {
                    tracing::error!(task_id = %task.id, %error, "Failed to persist the captured file contents");
                }
                Some(capture.files.len())
            }
            None => None,
        };
        let closes = self.closes_map().get(&task.id).cloned();
        let body = self.record_bytes(task, closes, changes_sidecar, &target)?;
        self.write_atomic(&task.id.0, &target, &body)
    }

    fn write_atomic(&self, stem: &str, target: &Path, body: &[u8]) -> Result<(), ResultStoreError> {
        let target = target.to_path_buf();
        let temp = self
            .dir
            .join(format!(".{}.{}.tmp", stem, uuid::Uuid::new_v4().simple()));
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
            file.write_all(body)?;
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
        if matches!(task.status, TaskStatus::Running { .. })
            && matches!(self.read_task(&task.id), Ok((on_disk, _)) if on_disk.status.is_terminal())
        {
            return Ok(false);
        }
        self.persist(&task)?;
        Ok(true)
    }

    fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, ResultStoreError> {
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
        if meta.len() > max {
            return Err(ResultStoreError::Unsafe {
                path: path.to_path_buf(),
                reason: format!("larger than {max} bytes"),
            });
        }
        let mut body = Vec::with_capacity(meta.len() as usize);
        file.take(max).read_to_end(&mut body).map_err(io_at(path))?;
        Ok(body)
    }

    fn read_task(&self, id: &TaskId) -> Result<(Task, Envelope), ResultStoreError> {
        let path = self.path_for(id)?;
        let body = Self::read_bounded(&path, MAX_RESULT_BYTES)?;
        let parse = |source| ResultStoreError::Parse {
            path: path.clone(),
            source,
        };
        let task: Task = serde_json::from_slice(&body).map_err(parse)?;
        let envelope: Envelope = serde_json::from_slice(&body).map_err(parse)?;
        if task.id != *id {
            return Err(ResultStoreError::Unsafe {
                path,
                reason: format!("file holds task {} but is named for {}", task.id, id),
            });
        }
        Ok((task, envelope))
    }

    fn read_sidecar(&self, id: &TaskId, expected_files: usize) -> Option<ChangeCapture> {
        let path = self.sidecar_path_for(id).ok()?;
        let loaded = Self::read_bounded(&path, MAX_SIDECAR_BYTES)
            .and_then(|body| {
                serde_json::from_slice::<Sidecar>(&body).map_err(|source| ResultStoreError::Parse {
                    path: path.clone(),
                    source,
                })
            })
            .map(|sidecar| sidecar.into_capture(id, expected_files));
        match loaded {
            Ok(Some(capture)) => Some(capture),
            Ok(None) => {
                tracing::warn!(task_id = %id, "Ignoring a captured-contents file that does not match its result");
                None
            }
            Err(error) => {
                tracing::warn!(task_id = %id, %error, "Ignoring unreadable captured contents");
                None
            }
        }
    }

    pub fn load_record(&self, id: &TaskId) -> Result<LoadedRecord, ResultStoreError> {
        let (mut task, envelope) = self.read_task(id)?;
        let mut changes_lost = false;
        if let Some(expected) = envelope.changes_sidecar {
            match self.read_sidecar(id, expected) {
                Some(capture) => attach_capture(&mut task, capture),
                None => changes_lost = true,
            }
        }
        if let Some(closes) = &envelope.closes {
            self.closes_map().insert(id.clone(), closes.clone());
        }
        Ok(LoadedRecord {
            task,
            closes: envelope.closes,
            changes_lost,
        })
    }

    pub fn load(&self, id: &TaskId) -> Result<Task, ResultStoreError> {
        self.load_record(id).map(|record| record.task)
    }

    pub fn load_all_records(&self) -> Vec<LoadedRecord> {
        let mut records: Vec<LoadedRecord> = self
            .ids()
            .into_iter()
            .filter_map(|id| match self.load_record(&id) {
                Ok(record) => Some(record),
                Err(error) => {
                    tracing::warn!(task_id = %id, %error, "Skipping an unreadable persisted task result");
                    None
                }
            })
            .collect();
        records.sort_by(|a, b| {
            a.task
                .created_at
                .cmp(&b.task.created_at)
                .then(a.task.id.cmp(&b.task.id))
        });
        records
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
            .filter_map(|id| match self.read_task(&id) {
                Ok((task, _)) => Some(task),
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
                if let Ok(sidecar) = self.sidecar_path_for(&id) {
                    let _ = std::fs::remove_file(sidecar);
                }
                self.closes_map().remove(&id);
            }
        }
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(stem) = name.strip_suffix(SIDECAR_SUFFIX) {
                    let orphan = valid_id(stem)
                        && std::fs::symlink_metadata(self.dir.join(format!("{stem}.json")))
                            .is_err();
                    let stale = entry
                        .metadata()
                        .and_then(|meta| meta.modified())
                        .map(|at| at < cutoff)
                        .unwrap_or(false);
                    if orphan && stale {
                        let _ = std::fs::remove_file(entry.path());
                    }
                    continue;
                }
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

    fn completed_with(capture: Option<crate::changes::ChangeCapture>) -> TaskStatus {
        TaskStatus::Completed {
            finished_at: Utc::now(),
            result: crate::task::TaskResult {
                result_summary: "done".into(),
                changes_patch: None,
                format_patch: None,
                files_modified: vec![],
                tool_calls_made: vec![],
                denials: vec![],
                action_audit: vec![],
                iterations: 1,
                model_used: "m".into(),
                blast_radius: None,
                qa_summary: Default::default(),
                budget: Default::default(),
                changes: capture,
            },
        }
    }

    fn failed_with(capture: Option<crate::changes::ChangeCapture>) -> TaskStatus {
        let mut status = daemon_restarted_status(Utc::now(), 2);
        if let TaskStatus::Failed { diagnostics, .. } = &mut status {
            diagnostics.error_type = "MaxIterationsExceeded".into();
            diagnostics.changes = capture;
        }
        status
    }

    fn rich_capture() -> crate::changes::ChangeCapture {
        use crate::changes::{CapturedFile, ChangeCapture, ChangeStatus};
        let file = |path: &str, status, content: Option<Vec<u8>>| CapturedFile {
            path: path.to_string(),
            old_path: None,
            status,
            binary: false,
            size: content.as_ref().map_or(0, Vec::len),
            content,
            capture_truncated: false,
        };
        let mut binary = file(
            "img.bin",
            ChangeStatus::Added,
            Some((0..=255u8).chain([0, 255, 1]).collect()),
        );
        binary.binary = true;
        let mut renamed = file(
            "to.rs",
            ChangeStatus::Renamed,
            Some(b"fn a() {}\n".to_vec()),
        );
        renamed.old_path = Some("from.rs".into());
        let mut truncated = file(
            "big.txt",
            ChangeStatus::Modified,
            Some("caf\u{e9} ".repeat(10).into_bytes()),
        );
        truncated.size = 9_000_000;
        truncated.capture_truncated = true;
        ChangeCapture {
            base_ref: Some("a".repeat(40)),
            files: vec![
                file(
                    "a.rs",
                    ChangeStatus::Added,
                    Some(b"hello \"world\"\n".to_vec()),
                ),
                binary,
                file("gone.rs", ChangeStatus::Deleted, None),
                renamed,
                truncated,
                file("unreadable.rs", ChangeStatus::Modified, None),
            ],
            patch_original_bytes: Some(123_456),
        }
    }

    fn same_capture(a: &crate::changes::ChangeCapture, b: &crate::changes::ChangeCapture) -> bool {
        a.base_ref == b.base_ref
            && a.patch_original_bytes == b.patch_original_bytes
            && a.files.len() == b.files.len()
            && a.files.iter().zip(&b.files).all(|(x, y)| {
                x.path == y.path
                    && x.old_path == y.old_path
                    && x.status == y.status
                    && x.binary == y.binary
                    && x.content == y.content
                    && x.size == y.size
                    && x.capture_truncated == y.capture_truncated
            })
    }

    fn captured_of(task: &Task) -> Option<&crate::changes::ChangeCapture> {
        match &task.status {
            TaskStatus::Completed { result, .. } => result.changes.as_ref(),
            TaskStatus::Failed { diagnostics, .. } => diagnostics.changes.as_ref(),
            _ => None,
        }
    }

    fn sidecar_path(store: &ResultStore, id: &str) -> PathBuf {
        store.dir().join(format!("{id}.changes.json"))
    }

    #[test]
    fn a_capture_round_trips_through_its_sidecar_for_completed_and_failed_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let capture = rich_capture();
        store
            .persist(&task("done", completed_with(Some(capture.clone()))))
            .unwrap();
        store
            .persist(&task("failed", failed_with(Some(capture.clone()))))
            .unwrap();
        for id in ["done", "failed"] {
            let record = store.load_record(&TaskId(id.into())).unwrap();
            assert!(!record.changes_lost, "{id}");
            assert!(
                same_capture(captured_of(&record.task).unwrap(), &capture),
                "{id}"
            );
        }
    }

    #[test]
    fn the_sidecar_is_a_private_regular_file_and_encodes_bytes_as_base64() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store
            .persist(&task("t1", completed_with(Some(rich_capture()))))
            .unwrap();
        let path = sidecar_path(&store, "t1");
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let binary = value["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == "img.bin")
            .unwrap();
        assert!(binary["content_base64"].is_string());
        let names: Vec<String> = std::fs::read_dir(store.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }

    #[test]
    fn the_main_record_stays_loadable_by_the_plain_task_reader() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store
            .persist(&task("t1", completed_with(Some(rich_capture()))))
            .unwrap();
        let bytes = std::fs::read(store.dir().join("t1.json")).unwrap();
        let plain: Task = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(plain.id.0, "t1");
        let ids: Vec<String> = store.load_all().into_iter().map(|t| t.id.0).collect();
        assert_eq!(ids, vec!["t1".to_string()]);
    }

    #[test]
    fn a_record_without_a_sidecar_marker_loads_with_no_changes_and_is_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        std::fs::write(
            store.dir().join("old.json"),
            serde_json::to_vec(&task("old", completed_with(None))).unwrap(),
        )
        .unwrap();
        let record = store.load_record(&TaskId("old".into())).unwrap();
        assert!(captured_of(&record.task).is_none());
        assert!(!record.changes_lost);
        assert!(record.closes.is_none());
    }

    fn damaged(corrupt: impl FnOnce(&Path)) -> (tempfile::TempDir, ResultStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store
            .persist(&task("t1", completed_with(Some(rich_capture()))))
            .unwrap();
        corrupt(&sidecar_path(&store, "t1"));
        (dir, store)
    }

    fn edit_sidecar(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        edit(&mut value);
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    fn assert_degraded(store: &ResultStore) {
        let record = store.load_record(&TaskId("t1".into())).unwrap();
        assert!(captured_of(&record.task).is_none());
        assert!(record.changes_lost);
        assert!(matches!(record.task.status, TaskStatus::Completed { .. }));
        assert_eq!(store.load_all().len(), 1);
    }

    #[test]
    fn a_corrupt_sidecar_degrades_to_no_changes_without_failing_the_result() {
        let (_dir, store) = damaged(|p| std::fs::write(p, b"{not json").unwrap());
        assert_degraded(&store);
    }

    #[test]
    fn a_missing_sidecar_degrades_to_no_changes_without_failing_the_result() {
        let (_dir, store) = damaged(|p| std::fs::remove_file(p).unwrap());
        assert_degraded(&store);
    }

    #[test]
    fn an_oversize_sidecar_is_refused_without_being_read_into_memory() {
        let (_dir, store) = damaged(|p| {
            let file = std::fs::OpenOptions::new().write(true).open(p).unwrap();
            file.set_len(MAX_SIDECAR_BYTES + 1).unwrap();
        });
        assert_degraded(&store);
    }

    #[test]
    fn a_symlinked_sidecar_is_not_followed() {
        let (_dir, store) = damaged(|p| {
            let body = std::fs::read(p).unwrap();
            std::fs::remove_file(p).unwrap();
            let outside = p.parent().unwrap().parent().unwrap().join("outside.json");
            std::fs::write(&outside, body).unwrap();
            std::os::unix::fs::symlink(&outside, p).unwrap();
        });
        assert_degraded(&store);
    }

    #[test]
    fn a_sidecar_belonging_to_another_task_is_rejected() {
        let (_dir, store) = damaged(|p| {
            edit_sidecar(p, |v| v["task_id"] = "other".into());
        });
        assert_degraded(&store);
    }

    #[test]
    fn a_sidecar_with_the_wrong_file_count_is_rejected() {
        let (_dir, store) = damaged(|p| {
            edit_sidecar(p, |v| {
                v["files"].as_array_mut().unwrap().pop();
            });
        });
        assert_degraded(&store);
    }

    #[test]
    fn a_sidecar_whose_bytes_exceed_the_capture_caps_is_rejected() {
        let (_dir, store) = damaged(|p| {
            edit_sidecar(p, |v| {
                let over = crate::changes::MAX_FILE_CAPTURE_BYTES + 1;
                v["files"][0]["content_base64"] =
                    crate::changes::base64_encode(&vec![7u8; over]).into();
                v["files"][0]["size"] = over.into();
            });
        });
        assert_degraded(&store);
    }

    #[test]
    fn invalid_base64_in_a_sidecar_is_rejected() {
        let (_dir, store) = damaged(|p| {
            edit_sidecar(p, |v| {
                v["files"][0]["content_base64"] = "!!!not base64!!!".into();
            });
        });
        assert_degraded(&store);
    }

    #[test]
    fn the_sidecar_is_written_before_the_record_so_a_visible_result_has_its_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let record = store.dir().join("t1.json");
        std::fs::create_dir(&record).unwrap();
        let refused = store.persist(&task("t1", completed_with(Some(rich_capture()))));
        assert!(refused.is_err());
        assert!(sidecar_path(&store, "t1").is_file());
    }

    #[test]
    fn a_failed_sidecar_write_still_records_the_result_and_marks_the_changes_lost() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        std::fs::create_dir(sidecar_path(&store, "t1")).unwrap();
        store
            .persist(&task("t1", completed_with(Some(rich_capture()))))
            .unwrap();
        assert_degraded(&store);
    }

    #[test]
    fn a_capture_too_large_for_the_sidecar_bound_is_marked_lost_not_written() {
        use crate::changes::{CapturedFile, ChangeCapture, ChangeStatus};
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let file = |n: usize| CapturedFile {
            path: format!("f{n}.bin"),
            old_path: None,
            status: ChangeStatus::Added,
            binary: true,
            content: Some(vec![255u8; crate::changes::MAX_FILE_CAPTURE_BYTES]),
            size: crate::changes::MAX_FILE_CAPTURE_BYTES,
            capture_truncated: false,
        };
        let capture = ChangeCapture {
            base_ref: None,
            files: (0..16).map(file).collect(),
            patch_original_bytes: None,
        };
        store
            .persist(&task("t1", completed_with(Some(capture))))
            .unwrap();
        assert!(!sidecar_path(&store, "t1").exists());
        assert_degraded(&store);
    }

    #[test]
    fn closes_is_kept_in_the_record_envelope_and_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.set_closes(&TaskId("t1".into()), "#462".into());
        store.persist(&task("t1", completed_with(None))).unwrap();
        let record = store.load_record(&TaskId("t1".into())).unwrap();
        assert_eq!(record.closes.as_deref(), Some("#462"));
        let bytes = std::fs::read(store.dir().join("t1.json")).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value.get("task").is_none());
        assert_eq!(value["id"], "t1");
        let reopened = ResultStore::open(dir.path(), Duration::from_secs(86400)).unwrap();
        assert_eq!(
            reopened
                .load_record(&TaskId("t1".into()))
                .unwrap()
                .closes
                .as_deref(),
            Some("#462")
        );
    }

    #[test]
    fn closes_set_after_the_record_was_written_is_added_and_the_changes_marker_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store
            .persist(&task("t1", completed_with(Some(rich_capture()))))
            .unwrap();
        store.set_closes(&TaskId("t1".into()), "#9".into());
        let record = store.load_record(&TaskId("t1".into())).unwrap();
        assert_eq!(record.closes.as_deref(), Some("#9"));
        assert!(!record.changes_lost);
        assert!(captured_of(&record.task).is_some());
    }

    #[test]
    fn a_task_without_closes_or_changes_writes_the_plain_task_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let original = task("t1", cancelled());
        store.persist(&original).unwrap();
        assert_eq!(
            std::fs::read(store.dir().join("t1.json")).unwrap(),
            serde_json::to_vec(&original).unwrap()
        );
    }

    #[test]
    fn prune_removes_the_sidecar_with_its_record_and_orphans_past_retention() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store
            .persist(&task("old", completed_with(Some(rich_capture()))))
            .unwrap();
        store
            .persist(&task("new", completed_with(Some(rich_capture()))))
            .unwrap();
        std::fs::write(sidecar_path(&store, "orphan-old"), b"{}").unwrap();
        std::fs::write(sidecar_path(&store, "orphan-new"), b"{}").unwrap();
        let age = |name: &str, days: u64| {
            let file = std::fs::File::options()
                .write(true)
                .open(store.dir().join(name))
                .unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(days * 86400))
                .unwrap();
        };
        age("old.json", 15);
        age("old.changes.json", 15);
        age("orphan-old.changes.json", 15);
        age("new.json", 1);
        store.prune(SystemTime::now());
        assert!(!store.dir().join("old.json").exists());
        assert!(!sidecar_path(&store, "old").exists());
        assert!(!sidecar_path(&store, "orphan-old").exists());
        assert!(store.dir().join("new.json").exists());
        assert!(sidecar_path(&store, "new").exists());
        assert!(sidecar_path(&store, "orphan-new").exists());
    }

    #[test]
    fn a_sidecar_name_never_counts_as_a_result_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        std::fs::write(sidecar_path(&store, "x"), b"{}").unwrap();
        assert!(store.load_all().is_empty());
        assert!(store.interrupted_ids().is_empty());
    }

    #[test]
    fn a_running_snapshot_cannot_replace_a_terminal_record_or_its_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let mut terminal = Some(task("t1", completed_with(Some(rich_capture()))));
        assert!(store.persist_latest(|| terminal.take()).unwrap());
        let mut stale = Some(task("t1", running()));
        assert!(!store.persist_latest(|| stale.take()).unwrap());
        let record = store.load_record(&TaskId("t1".into())).unwrap();
        assert!(matches!(record.task.status, TaskStatus::Completed { .. }));
        assert!(captured_of(&record.task).is_some());
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
