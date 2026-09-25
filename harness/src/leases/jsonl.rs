use super::store::LeaseTable;
use super::{Lease, LeaseError, LeaseName, LeaseStore};
use crate::scheduler::default_queue_path;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Environment variable overriding the lease log location.
pub const LEASE_PATH_ENV: &str = "NANNA_LEASE_PATH";

/// Location of the lease log for this user: `NANNA_LEASE_PATH` when set,
/// otherwise `leases.jsonl` next to the queue log
/// ([`default_queue_path`]), or `None` when neither is available.
pub fn default_lease_path() -> Option<PathBuf> {
    lease_path_from(std::env::var_os(LEASE_PATH_ENV), default_queue_path())
}

/// Pure form of [`default_lease_path`].
///
/// ```
/// use harness::leases::lease_path_from;
/// use std::path::PathBuf;
///
/// assert_eq!(
///     lease_path_from(Some("/var/lib/nanna/l.jsonl".into()), None),
///     Some(PathBuf::from("/var/lib/nanna/l.jsonl"))
/// );
/// assert_eq!(
///     lease_path_from(None, Some(PathBuf::from("/home/u/.local/state/nanna/queue.jsonl"))),
///     Some(PathBuf::from("/home/u/.local/state/nanna/leases.jsonl"))
/// );
/// assert_eq!(lease_path_from(None, None), None);
/// ```
pub fn lease_path_from(
    override_path: Option<std::ffi::OsString>,
    queue_path: Option<PathBuf>,
) -> Option<PathBuf> {
    override_path
        .map(PathBuf::from)
        .or_else(|| queue_path.map(|q| q.with_file_name("leases.jsonl")))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Record {
    Grant { lease: Lease },
    Drop { name: LeaseName },
}

/// Append-only JSON Lines store: one [`Record`] per line, replayed and
/// compacted on [`open`](Self::open). The live table is kept in memory and
/// every mutation is appended before it takes effect, so a failed write
/// leaves the store unchanged.
///
/// ```
/// use chrono::{Duration, Utc};
/// use harness::leases::{JsonlLeaseStore, LeaseName, LeaseStore};
///
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("leases.jsonl");
/// let now = Utc::now();
/// let name = LeaseName::deploy("example/repo", "prod");
/// let lease = JsonlLeaseStore::open(&path).unwrap().acquire(&name, "task-a", Duration::hours(1), now).unwrap();
/// let reopened = JsonlLeaseStore::open(&path).unwrap();
/// assert_eq!(reopened.snapshot().unwrap(), vec![lease.clone()]);
/// reopened.release(&lease).unwrap();
/// assert!(JsonlLeaseStore::open(&path).unwrap().snapshot().unwrap().is_empty());
/// ```
#[derive(Debug, Clone)]
pub struct JsonlLeaseStore {
    path: PathBuf,
    table: Arc<Mutex<LeaseTable>>,
}

impl JsonlLeaseStore {
    /// Open or create the log at `path`, creating parent directories, replay
    /// it and rewrite it with only the live records.
    pub fn open(path: &Path) -> Result<Self, LeaseError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        OpenOptions::new().create(true).append(true).open(path)?;
        let table = LeaseTable::from_leases(replay(path)?);
        let mut compacted = String::new();
        for lease in table.snapshot() {
            compacted.push_str(&serde_json::to_string(&Record::Grant { lease })?);
            compacted.push('\n');
        }
        fs::write(path, compacted)?;
        Ok(Self {
            path: path.to_path_buf(),
            table: Arc::new(Mutex::new(table)),
        })
    }

    /// Location of the log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Apply `op` to a copy of the table, append the resulting records, and
    /// only then adopt the copy.
    fn commit<T>(
        &self,
        op: impl FnOnce(&mut LeaseTable) -> Result<(T, Vec<Record>), LeaseError>,
    ) -> Result<T, LeaseError> {
        let mut table = self.table.lock().unwrap();
        let mut next = table.clone();
        let (value, records) = op(&mut next)?;
        let mut lines = String::new();
        for record in &records {
            lines.push_str(&serde_json::to_string(record)?);
            lines.push('\n');
        }
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        file.write_all(lines.as_bytes())?;
        file.flush()?;
        *table = next;
        Ok(value)
    }
}

fn replay(path: &Path) -> Result<Vec<Lease>, LeaseError> {
    let mut table = LeaseTable::default();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line)? {
            Record::Grant { lease } => table.insert(lease),
            Record::Drop { name } => table.remove(&name),
        }
    }
    Ok(table.snapshot())
}

fn drops(leases: &[Lease]) -> Vec<Record> {
    leases
        .iter()
        .map(|l| Record::Drop {
            name: l.name.clone(),
        })
        .collect()
}

impl LeaseStore for JsonlLeaseStore {
    fn acquire(
        &self,
        name: &LeaseName,
        holder: &str,
        ttl: Duration,
        now: DateTime<Utc>,
    ) -> Result<Lease, LeaseError> {
        self.commit(|table| {
            let lease = table.acquire(name, holder, ttl, now)?;
            let record = Record::Grant {
                lease: lease.clone(),
            };
            Ok((lease, vec![record]))
        })
    }

    fn renew(&self, lease: &Lease, ttl: Duration, now: DateTime<Utc>) -> Result<Lease, LeaseError> {
        self.commit(|table| {
            let renewed = table.renew(lease, ttl, now)?;
            let record = Record::Grant {
                lease: renewed.clone(),
            };
            Ok((renewed, vec![record]))
        })
    }

    fn release(&self, lease: &Lease) -> Result<(), LeaseError> {
        self.commit(|table| {
            let released = table.release(lease)?;
            Ok(((), drops(&[released])))
        })
    }

    fn release_all(&self, holder: &str) -> Result<Vec<Lease>, LeaseError> {
        self.commit(|table| {
            let released = table.release_all(holder);
            let records = drops(&released);
            Ok((released, records))
        })
    }

    fn expired(&self, now: DateTime<Utc>) -> Result<Vec<Lease>, LeaseError> {
        self.commit(|table| {
            let expired = table.expired(now);
            let records = drops(&expired);
            Ok((expired, records))
        })
    }

    fn snapshot(&self) -> Result<Vec<Lease>, LeaseError> {
        Ok(self.table.lock().unwrap().snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap()
    }

    fn ttl() -> Duration {
        Duration::minutes(10)
    }

    #[test]
    fn round_trip_replays_every_operation_and_compacts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("leases.jsonl");
        let store = JsonlLeaseStore::open(&path).unwrap();
        assert_eq!(store.path(), path.as_path());
        let a = LeaseName::branch("r", "a");
        let b = LeaseName::branch("r", "b");
        let c = LeaseName::deploy("r", "prod");
        let lease_a = store.acquire(&a, "h1", ttl(), t0()).unwrap();
        let lease_b = store.acquire(&b, "h1", ttl(), t0()).unwrap();
        let lease_c = store.acquire(&c, "h2", ttl(), t0()).unwrap();
        let renewed_b = store
            .renew(&lease_b, ttl(), t0() + Duration::minutes(1))
            .unwrap();
        store.release(&lease_a).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 5);

        let reopened = JsonlLeaseStore::open(&path).unwrap();
        assert_eq!(
            reopened.snapshot().unwrap(),
            vec![lease_c.clone(), renewed_b.clone()]
        );
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);

        assert_eq!(reopened.release_all("h1").unwrap(), vec![renewed_b]);
        let stale = t0() + ttl();
        assert_eq!(reopened.expired(stale).unwrap(), vec![lease_c]);
        assert!(JsonlLeaseStore::open(&path)
            .unwrap()
            .snapshot()
            .unwrap()
            .is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn crash_recovery_reopens_with_holders_intact_for_release_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.jsonl");
        let name = LeaseName::sandbox("r", 9);
        JsonlLeaseStore::open(&path)
            .unwrap()
            .acquire(&name, "task-crashed", ttl(), t0())
            .unwrap();
        let survivor = JsonlLeaseStore::open(&path).unwrap();
        assert_eq!(
            survivor
                .acquire(&name, "task-new", ttl(), t0())
                .unwrap_err(),
            LeaseError::Held {
                name: name.clone(),
                by: "task-crashed".to_string(),
                until: t0() + ttl()
            }
        );
        assert_eq!(survivor.release_all("task-crashed").unwrap().len(), 1);
        assert_eq!(
            survivor
                .acquire(&name, "task-new", ttl(), t0())
                .unwrap()
                .holder,
            "task-new"
        );
    }

    #[test]
    fn skips_blank_lines_and_rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.jsonl");
        fs::write(&path, "\n\n").unwrap();
        assert!(JsonlLeaseStore::open(&path)
            .unwrap()
            .snapshot()
            .unwrap()
            .is_empty());
        fs::write(&path, "{not json}\n").unwrap();
        let err = JsonlLeaseStore::open(&path).unwrap_err();
        assert!(matches!(err, LeaseError::Serde(_)));
        assert!(err.to_string().contains("not valid JSON"));
    }

    #[test]
    fn failed_append_leaves_the_table_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leases.jsonl");
        let store = JsonlLeaseStore::open(&path).unwrap();
        let name = LeaseName::branch("r", "main");
        let lease = store.acquire(&name, "h", ttl(), t0()).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(matches!(
            store.acquire(&LeaseName::branch("r", "x"), "h", ttl(), t0()),
            Err(LeaseError::Io(_))
        ));
        assert!(matches!(store.release(&lease), Err(LeaseError::Io(_))));
        assert!(matches!(store.release_all("h"), Err(LeaseError::Io(_))));
        assert!(matches!(
            store.expired(t0() + ttl()),
            Err(LeaseError::Io(_))
        ));
        assert!(matches!(
            store.renew(&lease, ttl(), t0()),
            Err(LeaseError::Io(_))
        ));
        assert_eq!(store.snapshot().unwrap(), vec![lease]);
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "").unwrap();
        let err = JsonlLeaseStore::open(&blocker.join("x")).unwrap_err();
        assert!(err.to_string().contains("I/O error"));
    }

    #[test]
    fn default_lease_path_reads_environment() {
        let resolved = default_lease_path();
        let expected = lease_path_from(std::env::var_os(LEASE_PATH_ENV), default_queue_path());
        assert_eq!(resolved, expected);
    }
}
