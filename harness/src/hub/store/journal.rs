use super::StoreError;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

pub const LOCK_FILE: &str = "hub.lock";

/// Exclusive claim on a hub state directory. A second claim on the directory
/// fails with [`StoreError::Locked`], and so does opening a second store on
/// a file that another open store in this directory already owns, so two
/// writers can never interleave on one file, in or across processes.
///
/// ```
/// use harness::hub::store::{HubLock, StoreError};
///
/// let dir = tempfile::tempdir().unwrap();
/// let first = HubLock::acquire(dir.path()).unwrap();
/// assert!(matches!(HubLock::acquire(dir.path()), Err(StoreError::Locked)));
/// drop(first);
/// assert!(HubLock::acquire(dir.path()).is_ok());
/// ```
#[derive(Debug)]
pub struct HubLock {
    dir: PathBuf,
    _file: File,
    claimed: Mutex<HashSet<OsString>>,
}

pub(super) struct Claim {
    lock: Arc<HubLock>,
    name: OsString,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.lock
            .claimed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.name);
    }
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn lock_outcome(attempt: Result<(), fs4::TryLockError>) -> Result<(), StoreError> {
    match attempt {
        Ok(()) => Ok(()),
        Err(fs4::TryLockError::WouldBlock) => Err(StoreError::Locked),
        Err(fs4::TryLockError::Error(source)) => Err(StoreError::Io(source)),
    }
}

impl HubLock {
    /// Create `dir` if needed and take the exclusive lock on `dir/hub.lock`.
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// let lock = harness::hub::store::HubLock::acquire(&dir.path().join("state")).unwrap();
    /// assert!(lock.dir().ends_with("state"));
    /// ```
    pub fn acquire(dir: &Path) -> Result<Arc<Self>, StoreError> {
        fs::create_dir_all(dir)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(dir.join(LOCK_FILE))?;
        lock_outcome(fs4::FileExt::try_lock(&file))?;
        Ok(Arc::new(Self {
            dir: dir.to_path_buf(),
            _file: file,
            claimed: Mutex::new(HashSet::new()),
        }))
    }

    /// The directory this lock protects.
    ///
    /// ```
    /// let dir = tempfile::tempdir().unwrap();
    /// let lock = harness::hub::store::HubLock::acquire(dir.path()).unwrap();
    /// assert_eq!(lock.dir(), dir.path());
    /// ```
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub(super) fn claim(self: &Arc<Self>, path: &Path) -> Result<Claim, StoreError> {
        let name = match path.file_name() {
            Some(name) if parent_dir(path) == self.dir => name.to_os_string(),
            _ => {
                return Err(StoreError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "state file is not directly inside the locked hub directory",
                )))
            }
        };
        let fresh = self
            .claimed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.clone());
        if !fresh {
            return Err(StoreError::Locked);
        }
        Ok(Claim {
            lock: Arc::clone(self),
            name,
        })
    }
}

pub(super) trait Journal: Send {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()>;
}

pub(super) trait Backing: Write + Send {
    fn len(&self) -> io::Result<u64>;
    fn truncate(&mut self, len: u64) -> io::Result<()>;
}

impl Backing for File {
    fn len(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        self.set_len(len)
    }
}

pub(super) struct FileJournal<B: Backing> {
    backing: B,
    poisoned: bool,
}

impl<B: Backing> FileJournal<B> {
    pub(super) fn new(backing: B) -> Self {
        Self {
            backing,
            poisoned: false,
        }
    }
}

impl<B: Backing> Journal for FileJournal<B> {
    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other(
                "journal is unusable after a failed rollback",
            ));
        }
        let before = self.backing.len()?;
        let written = self
            .backing
            .write_all(bytes)
            .and_then(|()| self.backing.flush());
        if let Err(error) = written {
            self.poisoned = self.backing.truncate(before).is_err();
            return Err(error);
        }
        Ok(())
    }
}

pub(super) fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().append(true).open(path)
}

pub(super) fn rewrite_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Mock {
        bytes: Vec<u8>,
        fail_write_after: Option<usize>,
        fail_flush: bool,
        fail_truncate: bool,
        fail_len: bool,
    }

    impl Write for Mock {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if let Some(limit) = self.fail_write_after {
                let room = limit.saturating_sub(self.bytes.len()).min(buf.len());
                self.bytes.extend_from_slice(&buf[..room]);
                if room < buf.len() {
                    return Err(io::Error::other("disk full"));
                }
                return Ok(room);
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                return Err(io::Error::other("flush failed"));
            }
            Ok(())
        }
    }

    impl Backing for Mock {
        fn len(&self) -> io::Result<u64> {
            if self.fail_len {
                return Err(io::Error::other("stat failed"));
            }
            Ok(self.bytes.len() as u64)
        }

        fn truncate(&mut self, len: u64) -> io::Result<()> {
            if self.fail_truncate {
                return Err(io::Error::other("truncate failed"));
            }
            self.bytes.truncate(len as usize);
            Ok(())
        }
    }

    #[test]
    fn a_successful_append_adds_the_bytes() {
        let mut journal = FileJournal::new(Mock::default());
        journal.append(b"one\n").unwrap();
        journal.append(b"two\n").unwrap();
        assert_eq!(journal.backing.bytes, b"one\ntwo\n");
    }

    #[test]
    fn a_partial_write_is_rolled_back_so_the_next_append_starts_on_a_record_boundary() {
        let mut journal = FileJournal::new(Mock::default());
        journal.append(b"one\n").unwrap();
        journal.backing.fail_write_after = Some(7);
        assert!(journal.append(b"two-two\n").is_err());
        assert_eq!(journal.backing.bytes, b"one\n");
        journal.backing.fail_write_after = None;
        journal.append(b"three\n").unwrap();
        assert_eq!(journal.backing.bytes, b"one\nthree\n");
    }

    #[test]
    fn a_failed_flush_is_rolled_back_too() {
        let mut journal = FileJournal::new(Mock::default());
        journal.backing.fail_flush = true;
        assert!(journal.append(b"one\n").is_err());
        assert!(journal.backing.bytes.is_empty());
    }

    #[test]
    fn a_failed_rollback_poisons_the_journal() {
        let mut journal = FileJournal::new(Mock::default());
        journal.backing.fail_write_after = Some(2);
        journal.backing.fail_truncate = true;
        assert!(journal.append(b"abcdef\n").is_err());
        journal.backing.fail_write_after = None;
        journal.backing.fail_truncate = false;
        let error = journal.append(b"next\n").unwrap_err();
        assert!(error.to_string().contains("unusable"));
        assert_eq!(journal.backing.bytes, b"ab");
    }

    #[test]
    fn a_length_probe_failure_refuses_the_append() {
        let mut journal = FileJournal::new(Mock::default());
        journal.backing.fail_len = true;
        assert!(journal.append(b"x\n").is_err());
        assert!(journal.backing.bytes.is_empty());
    }

    #[test]
    fn a_real_file_reports_its_length_and_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"hello").unwrap();
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        assert_eq!(Backing::len(&file).unwrap(), 5);
        Backing::truncate(&mut file, 2).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"he");
    }

    #[test]
    fn lock_outcomes_map_to_store_errors() {
        assert!(lock_outcome(Ok(())).is_ok());
        assert!(matches!(
            lock_outcome(Err(fs4::TryLockError::WouldBlock)),
            Err(StoreError::Locked)
        ));
        let failure = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(matches!(
            lock_outcome(Err(fs4::TryLockError::Error(failure))),
            Err(StoreError::Io(_))
        ));
    }

    #[test]
    fn acquire_fails_when_the_directory_cannot_be_created() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plain-file");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            HubLock::acquire(&file.join("sub")),
            Err(StoreError::Io(_))
        ));
    }

    #[test]
    fn a_relative_file_belongs_to_the_current_directory() {
        assert_eq!(parent_dir(Path::new("sessions.jsonl")), Path::new("."));
        assert_eq!(parent_dir(Path::new("a/b.jsonl")), Path::new("a"));
    }

    #[test]
    fn a_claim_covers_only_files_directly_inside_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let lock = HubLock::acquire(dir.path()).unwrap();
        assert!(lock.claim(&dir.path().join("x.jsonl")).is_ok());
        assert!(lock
            .claim(&dir.path().join("nested").join("x.jsonl"))
            .is_err());
        assert!(lock.claim(Path::new("x.jsonl")).is_err());
        assert!(lock.claim(Path::new("/")).is_err());
    }

    #[test]
    fn a_file_can_be_claimed_once_until_the_claim_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let lock = HubLock::acquire(dir.path()).unwrap();
        let first = lock.claim(&dir.path().join("a.jsonl")).unwrap();
        assert!(matches!(
            lock.claim(&dir.path().join("a.jsonl")),
            Err(StoreError::Locked)
        ));
        let other = lock.claim(&dir.path().join("b.jsonl")).unwrap();
        drop(first);
        assert!(lock.claim(&dir.path().join("a.jsonl")).is_ok());
        drop(other);
    }

    #[test]
    fn rewrite_replaces_the_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"old old old").unwrap();
        rewrite_private(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!dir.path().join("f.tmp").exists());
        assert!(rewrite_private(&dir.path().join("missing").join("f"), b"x").is_err());
    }
}
