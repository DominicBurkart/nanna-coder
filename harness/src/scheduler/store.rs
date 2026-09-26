use super::QueuedTask;
use crate::task::TaskId;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;

/// Failure to persist or reload the queue.
#[derive(Debug, Error)]
pub enum QueueStoreError {
    #[error("queue store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("queue store record is not valid JSON: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("queue store rejected the operation: {0}")]
    Rejected(String),
}

/// Durable record of queued entries.
///
/// The store only tracks entries that have not reached a terminal state:
/// [`insert`](Self::insert) on submission, [`remove`](Self::remove) on
/// completion, failure or cancellation. A rebuilt manager reloads everything
/// still present, so a task that was running when the process died is
/// queued again on restart.
pub trait QueueStore: Send + Sync {
    /// All persisted entries, in no particular order.
    fn load(&self) -> Result<Vec<QueuedTask>, QueueStoreError>;
    /// Persist an entry, replacing any existing record with the same id.
    fn insert(&self, task: &QueuedTask) -> Result<(), QueueStoreError>;
    /// Forget an entry. Removing an unknown id is not an error.
    fn remove(&self, id: &TaskId) -> Result<(), QueueStoreError>;
}

/// Store that lives in process memory. Clones share the same records, which
/// lets tests rebuild a manager from the store it was using.
///
/// ```
/// use harness::scheduler::{InMemoryQueueStore, QueueStore, QueuedTask};
/// use std::path::PathBuf;
///
/// let store = InMemoryQueueStore::default();
/// let task = QueuedTask::new("t", PathBuf::from("/r"), "HEAD", "m", 1);
/// store.insert(&task).unwrap();
/// assert_eq!(store.clone().load().unwrap(), vec![task.clone()]);
/// store.remove(&task.id).unwrap();
/// assert!(store.load().unwrap().is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct InMemoryQueueStore {
    entries: Arc<Mutex<Vec<QueuedTask>>>,
}

impl QueueStore for InMemoryQueueStore {
    fn load(&self) -> Result<Vec<QueuedTask>, QueueStoreError> {
        Ok(self.entries.lock().unwrap().clone())
    }

    fn insert(&self, task: &QueuedTask) -> Result<(), QueueStoreError> {
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|t| t.id != task.id);
        entries.push(task.clone());
        Ok(())
    }

    fn remove(&self, id: &TaskId) -> Result<(), QueueStoreError> {
        self.entries.lock().unwrap().retain(|t| &t.id != id);
        Ok(())
    }
}

/// Environment variable overriding the queue log location.
pub const QUEUE_PATH_ENV: &str = "NANNA_QUEUE_PATH";

/// Location of the queue log for this user: `NANNA_QUEUE_PATH` when set,
/// otherwise `$HOME/.local/state/nanna/queue.jsonl`, or `None` when neither
/// is available.
pub fn default_queue_path() -> Option<PathBuf> {
    queue_path_from(std::env::var_os(QUEUE_PATH_ENV), std::env::var_os("HOME"))
}

/// Pure form of [`default_queue_path`].
///
/// ```
/// use harness::scheduler::queue_path_from;
/// use std::path::PathBuf;
///
/// assert_eq!(
///     queue_path_from(Some("/var/lib/nanna/q.jsonl".into()), Some("/home/u".into())),
///     Some(PathBuf::from("/var/lib/nanna/q.jsonl"))
/// );
/// assert_eq!(
///     queue_path_from(None, Some("/home/u".into())),
///     Some(PathBuf::from("/home/u/.local/state/nanna/queue.jsonl"))
/// );
/// assert_eq!(queue_path_from(None, None), None);
/// ```
pub fn queue_path_from(
    override_path: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    override_path.map(PathBuf::from).or_else(|| {
        home.map(|h| {
            PathBuf::from(h)
                .join(".local")
                .join("state")
                .join("nanna")
                .join("queue.jsonl")
        })
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Record {
    Insert { task: QueuedTask },
    Remove { id: TaskId },
}

/// Append-only JSON Lines store: one [`Record`] per line. `load` replays the
/// log and rewrites it with only the live entries, so the file stays
/// proportional to the queue depth.
///
/// ```
/// use harness::scheduler::{JsonlQueueStore, QueueStore, QueuedTask};
/// use std::path::PathBuf;
///
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("queue.jsonl");
/// let task = QueuedTask::new("t", PathBuf::from("/r"), "HEAD", "m", 1);
/// JsonlQueueStore::open(&path).unwrap().insert(&task).unwrap();
/// let reopened = JsonlQueueStore::open(&path).unwrap();
/// assert_eq!(reopened.load().unwrap(), vec![task]);
/// ```
#[derive(Debug, Clone)]
pub struct JsonlQueueStore {
    path: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

impl JsonlQueueStore {
    /// Open or create the log at `path`, creating parent directories.
    pub fn open(path: &Path) -> Result<Self, QueueStoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Location of the log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn append(&self, record: &Record) -> Result<(), QueueStoreError> {
        let _guard = self.write_lock.lock().unwrap();
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    fn replay(&self) -> Result<Vec<QueuedTask>, QueueStoreError> {
        let mut live: Vec<QueuedTask> = Vec::new();
        for line in BufReader::new(File::open(&self.path)?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Record>(&line)? {
                Record::Insert { task } => {
                    live.retain(|t| t.id != task.id);
                    live.push(task);
                }
                Record::Remove { id } => live.retain(|t| t.id != id),
            }
        }
        Ok(live)
    }
}

impl QueueStore for JsonlQueueStore {
    fn load(&self) -> Result<Vec<QueuedTask>, QueueStoreError> {
        let _guard = self.write_lock.lock().unwrap();
        let live = self.replay()?;
        let mut compacted = String::new();
        for task in &live {
            compacted.push_str(&serde_json::to_string(&Record::Insert {
                task: task.clone(),
            })?);
            compacted.push('\n');
        }
        fs::write(&self.path, compacted)?;
        Ok(live)
    }

    fn insert(&self, task: &QueuedTask) -> Result<(), QueueStoreError> {
        self.append(&Record::Insert { task: task.clone() })
    }

    fn remove(&self, id: &TaskId) -> Result<(), QueueStoreError> {
        self.append(&Record::Remove { id: id.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(desc: &str) -> QueuedTask {
        QueuedTask::new(desc, PathBuf::from("/r"), "HEAD", "m", 1)
    }

    #[test]
    fn in_memory_insert_replaces_same_id() {
        let store = InMemoryQueueStore::default();
        let mut t = task("a");
        store.insert(&t).unwrap();
        t.description = "b".to_string();
        store.insert(&t).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].description, "b");
        store.remove(&TaskId("unknown".to_string())).unwrap();
        assert_eq!(store.load().unwrap().len(), 1);
    }

    #[test]
    fn jsonl_round_trip_replays_and_compacts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("queue.jsonl");
        let store = JsonlQueueStore::open(&path).unwrap();
        assert_eq!(store.path(), path.as_path());
        let a = task("a");
        let b = task("b");
        store.insert(&a).unwrap();
        store.insert(&b).unwrap();
        store.remove(&a.id).unwrap();
        let mut updated = b.clone();
        updated.description = "b2".to_string();
        store.insert(&updated).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 4);

        let loaded = JsonlQueueStore::open(&path).unwrap().load().unwrap();
        assert_eq!(loaded, vec![updated.clone()]);
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);
        assert_eq!(store.load().unwrap(), vec![updated]);
    }

    #[test]
    fn jsonl_skips_blank_lines_and_rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.jsonl");
        fs::write(&path, "\n\n").unwrap();
        let store = JsonlQueueStore::open(&path).unwrap();
        assert!(store.load().unwrap().is_empty());
        fs::write(&path, "{not json}\n").unwrap();
        let err = store.load().unwrap_err();
        assert!(matches!(err, QueueStoreError::Serde(_)));
        assert!(err.to_string().contains("not valid JSON"));
    }

    #[test]
    fn default_queue_path_reads_environment() {
        let resolved = default_queue_path();
        let expected = queue_path_from(std::env::var_os(QUEUE_PATH_ENV), std::env::var_os("HOME"));
        assert_eq!(resolved, expected);
    }

    #[test]
    fn jsonl_reports_io_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.jsonl");
        let store = JsonlQueueStore::open(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(matches!(store.load().unwrap_err(), QueueStoreError::Io(_)));
        assert!(matches!(
            store.insert(&task("a")).unwrap_err(),
            QueueStoreError::Io(_)
        ));
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "").unwrap();
        let err = JsonlQueueStore::open(&blocker.join("x")).unwrap_err();
        assert!(err.to_string().contains("I/O error"));
        assert_eq!(
            QueueStoreError::Rejected("nope".to_string()).to_string(),
            "queue store rejected the operation: nope"
        );
    }
}
