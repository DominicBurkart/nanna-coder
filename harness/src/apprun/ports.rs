//! Per-task backend port allocation.
//!
//! Every task that starts its application needs a port nobody else in the
//! same range is using. Ports are handed out by a [`PortAllocator`]: a
//! mutex-guarded set of ports held by this process, backed by one lease file
//! per port in a directory shared by every harness process on the host, so
//! two harness processes never hand out the same port. A [`PortLease`]
//! releases its port (and deletes the lease file) when dropped.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use thiserror::Error;
use tracing::warn;

/// Ports handed out by [`PortAllocator::shared`].
pub const DEFAULT_PORT_RANGE: RangeInclusive<u16> = 18000..=18999;

/// Directory under the system temp dir, next to the `nanna-task-*`
/// workspaces, where the shared allocator keeps its lease files.
pub const LEASE_DIR_NAME: &str = "nanna-task-ports";

/// Errors from port allocation.
#[derive(Debug, Error)]
pub enum PortError {
    #[error("no free port in {start}-{end} (leases in {lease_dir})")]
    Exhausted {
        start: u16,
        end: u16,
        lease_dir: PathBuf,
    },
    #[error("could not write lease {path}: {source}")]
    Lease {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Hands out distinct ports from a range, one per task.
///
/// Ports held by this process live in a mutex-guarded set; every held port
/// also has a lease file `<port>.lease` in `lease_dir`, created atomically,
/// so allocators in other harness processes sharing the directory skip it.
///
/// ```
/// use harness::apprun::PortAllocator;
/// use std::sync::Arc;
///
/// let dir = tempfile::tempdir().unwrap();
/// let allocator = Arc::new(PortAllocator::new(20000..=20001, dir.path()));
/// let first = allocator.allocate("task-a").unwrap();
/// let second = allocator.allocate("task-b").unwrap();
/// assert_ne!(first.port(), second.port());
/// assert!(allocator.allocate("task-c").is_err(), "range exhausted");
/// drop(first);
/// assert_eq!(allocator.allocate("task-c").unwrap().port(), 20000);
/// ```
pub struct PortAllocator {
    range: RangeInclusive<u16>,
    lease_dir: PathBuf,
    held: Mutex<BTreeSet<u16>>,
}

impl PortAllocator {
    /// Allocator over `range` with lease files in `lease_dir`.
    pub fn new(range: RangeInclusive<u16>, lease_dir: impl Into<PathBuf>) -> Self {
        Self {
            range,
            lease_dir: lease_dir.into(),
            held: Mutex::new(BTreeSet::new()),
        }
    }

    /// The process-wide allocator: [`DEFAULT_PORT_RANGE`] with leases in
    /// [`LEASE_DIR_NAME`] under the system temp dir.
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<PortAllocator>> = OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| {
            Arc::new(Self::new(
                DEFAULT_PORT_RANGE,
                std::env::temp_dir().join(LEASE_DIR_NAME),
            ))
        }))
    }

    /// The range ports are taken from.
    pub fn range(&self) -> &RangeInclusive<u16> {
        &self.range
    }

    /// Directory holding the lease files.
    pub fn lease_dir(&self) -> &Path {
        &self.lease_dir
    }

    /// Lease file for `port`.
    pub fn lease_path(&self, port: u16) -> PathBuf {
        self.lease_dir.join(format!("{port}.lease"))
    }

    /// Ports currently held by this process, ascending.
    pub fn held(&self) -> Vec<u16> {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    /// Take the lowest free port for `task_id`.
    ///
    /// A port is free when this process does not hold it and no lease file
    /// exists for it. The lease file records the task id and this process id.
    pub fn allocate(self: &Arc<Self>, task_id: &str) -> Result<PortLease, PortError> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let content = format!("{task_id}\n{}\n", std::process::id());
        for port in self.range.clone() {
            if !held.contains(&port) {
                let path = self.lease_path(port);
                match create_lease(&path, &content) {
                    Ok(()) => {
                        held.insert(port);
                        return Ok(PortLease {
                            port,
                            allocator: Arc::clone(self),
                        });
                    }
                    Err(source) if source.kind() == ErrorKind::AlreadyExists => {}
                    Err(source) => return Err(PortError::Lease { path, source }),
                }
            }
        }
        Err(PortError::Exhausted {
            start: *self.range.start(),
            end: *self.range.end(),
            lease_dir: self.lease_dir.clone(),
        })
    }

    fn release(&self, port: u16) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&port);
        let path = self.lease_path(port);
        if std::fs::remove_file(&path).is_err() {
            warn!("lease file {} was already gone", path.display());
        }
    }
}

impl std::fmt::Debug for PortAllocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortAllocator")
            .field("range", &self.range)
            .field("lease_dir", &self.lease_dir)
            .field("held", &self.held())
            .finish()
    }
}

fn create_lease(path: &Path, content: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap_or(path))?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(content.as_bytes())
}

/// A port held by a task; released when dropped.
#[derive(Debug)]
pub struct PortLease {
    port: u16,
    allocator: Arc<PortAllocator>,
}

impl PortLease {
    /// The leased port.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for PortLease {
    fn drop(&mut self) {
        self.allocator.release(self.port);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn allocator(range: RangeInclusive<u16>) -> (Arc<PortAllocator>, TempDir) {
        let dir = TempDir::new().unwrap();
        let allocator = Arc::new(PortAllocator::new(range, dir.path().join("leases")));
        (allocator, dir)
    }

    #[test]
    fn two_tasks_get_distinct_ports() {
        let (allocator, _dir) = allocator(30000..=30010);
        let a = allocator.allocate("task-a").unwrap();
        let b = allocator.allocate("task-b").unwrap();
        assert_ne!(a.port(), b.port());
        assert_eq!(allocator.held(), vec![a.port(), b.port()]);
    }

    #[test]
    fn concurrent_allocations_from_threads_are_distinct() {
        let (allocator, _dir) = allocator(31000..=31031);
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let allocator = Arc::clone(&allocator);
                std::thread::spawn(move || allocator.allocate(&format!("t{i}")).unwrap())
            })
            .collect();
        let leases: Vec<PortLease> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let mut ports: Vec<u16> = leases.iter().map(PortLease::port).collect();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(ports.len(), leases.len());
        assert_eq!(allocator.held(), ports);
    }

    #[test]
    fn lease_file_records_task_and_pid_and_is_removed_on_drop() {
        let (allocator, _dir) = allocator(32000..=32000);
        let lease = allocator.allocate("task-x").unwrap();
        let path = allocator.lease_path(lease.port());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            content,
            format!("task-x\n{}\n", std::process::id()),
            "lease holds task id and pid"
        );
        drop(lease);
        assert!(!path.exists());
        assert!(allocator.held().is_empty());
    }

    #[test]
    fn exhausted_range_is_an_error_until_a_lease_is_released() {
        let (allocator, _dir) = allocator(33000..=33001);
        let a = allocator.allocate("a").unwrap();
        let _b = allocator.allocate("b").unwrap();
        let err = allocator.allocate("c").unwrap_err();
        assert!(matches!(
            err,
            PortError::Exhausted {
                start: 33000,
                end: 33001,
                ..
            }
        ));
        assert!(err.to_string().contains("33000-33001"));
        drop(a);
        assert_eq!(allocator.allocate("c").unwrap().port(), 33000);
    }

    #[test]
    fn lease_files_from_another_process_are_respected() {
        let (allocator, _dir) = allocator(34000..=34001);
        std::fs::create_dir_all(allocator.lease_dir()).unwrap();
        std::fs::write(allocator.lease_path(34000), "other\n1\n").unwrap();
        let lease = allocator.allocate("mine").unwrap();
        assert_eq!(lease.port(), 34001);
        let other = Arc::new(PortAllocator::new(34000..=34001, allocator.lease_dir()));
        assert!(matches!(
            other.allocate("x"),
            Err(PortError::Exhausted { .. })
        ));
    }

    #[test]
    fn release_tolerates_a_missing_lease_file() {
        let (allocator, _dir) = allocator(35000..=35000);
        let lease = allocator.allocate("a").unwrap();
        std::fs::remove_file(allocator.lease_path(lease.port())).unwrap();
        drop(lease);
        assert!(allocator.held().is_empty());
        assert_eq!(allocator.allocate("b").unwrap().port(), 35000);
    }

    #[test]
    fn unwritable_lease_dir_is_a_lease_error() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let allocator = Arc::new(PortAllocator::new(36000..=36000, file.join("leases")));
        let err = allocator.allocate("a").unwrap_err();
        assert!(matches!(err, PortError::Lease { .. }), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_lease_dir_is_a_lease_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let leases = dir.path().join("leases");
        std::fs::create_dir_all(&leases).unwrap();
        std::fs::set_permissions(&leases, std::fs::Permissions::from_mode(0o555)).unwrap();
        let allocator = Arc::new(PortAllocator::new(37000..=37000, &leases));
        let result = allocator.allocate("a");
        std::fs::set_permissions(&leases, std::fs::Permissions::from_mode(0o755)).unwrap();
        match result {
            Err(PortError::Lease { path, .. }) => assert_eq!(path, leases.join("37000.lease")),
            other => panic!("expected a lease error, got {other:?}"),
        }
    }

    #[test]
    fn shared_allocator_is_process_wide() {
        let a = PortAllocator::shared();
        let b = PortAllocator::shared();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.range(), &DEFAULT_PORT_RANGE);
        assert!(a.lease_dir().ends_with(LEASE_DIR_NAME));
    }

    #[test]
    fn debug_output_names_range_and_dir() {
        let (allocator, _dir) = allocator(38000..=38001);
        let text = format!("{allocator:?}");
        assert!(text.contains("38000..=38001"), "{text}");
        assert!(text.contains("held"), "{text}");
    }

    #[derive(Debug, Clone)]
    enum Op {
        Allocate,
        Release(usize),
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => Just(Op::Allocate),
            2 => (0usize..8).prop_map(Op::Release),
        ]
    }

    proptest! {
        #[test]
        fn held_ports_never_repeat(ops in prop::collection::vec(op_strategy(), 1..40)) {
            let (allocator, _dir) = allocator(39000..=39007);
            let mut held: Vec<PortLease> = Vec::new();
            for op in ops {
                match op {
                    Op::Allocate => match allocator.allocate("p") {
                        Ok(lease) => held.push(lease),
                        Err(PortError::Exhausted { .. }) => prop_assert_eq!(held.len(), 8),
                        Err(other) => return Err(TestCaseError::fail(other.to_string())),
                    },
                    Op::Release(i) => {
                        if i < held.len() {
                            held.swap_remove(i);
                        }
                    }
                }
                let mut ports: Vec<u16> = held.iter().map(PortLease::port).collect();
                ports.sort_unstable();
                let mut unique = ports.clone();
                unique.dedup();
                prop_assert_eq!(&ports, &unique);
                prop_assert_eq!(allocator.held(), ports);
                prop_assert!(held.iter().all(|l| allocator.lease_path(l.port()).exists()));
            }
        }
    }
}
