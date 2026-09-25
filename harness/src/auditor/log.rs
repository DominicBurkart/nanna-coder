//! Append-only record of every reviewed spawn.

use super::{AuditError, AuditOutcome, AuditRecord, SpawnRequest, SpawnVerdict};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// One logged decision: the request, the verdict, and how it was reached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditLogEntry {
    /// The spawn that was reviewed.
    pub request: SpawnRequest,
    /// What the auditor decided.
    pub verdict: SpawnVerdict,
    /// How the verdict was reached.
    pub record: AuditRecord,
}

impl AuditLogEntry {
    fn from_outcome(request: SpawnRequest, outcome: &AuditOutcome) -> Self {
        Self {
            request,
            verdict: outcome.verdict.clone(),
            record: outcome.record.clone(),
        }
    }
}

#[derive(Clone)]
enum Backing {
    Memory(Arc<Mutex<Vec<AuditLogEntry>>>),
    File(PathBuf),
}

/// Append-only audit trail: one entry per reviewed spawn.
///
/// [`AuditLog::in_memory`] is for tests; [`AuditLog::file`] writes JSON
/// Lines, one [`AuditLogEntry`] per line, appended next to the task queue's
/// own logs (see `evals::eval::scoring::append_line` for the sibling
/// convention this mirrors).
///
/// ```
/// use harness::auditor::{AuditLog, AuditOutcome, AuditRecord, SpawnRequest, SpawnVerdict, TaskSummary};
/// use harness::effects::EffectClass;
/// use harness::identity::DevLoop;
///
/// let dir = tempfile::tempdir().unwrap();
/// let log = AuditLog::file(dir.path().join("audit.jsonl"));
/// let request = SpawnRequest {
///     parent_task: TaskSummary::new("t1", "Fix bug", "github.com/example/repo"),
///     identity: "rust-implementer".to_string(),
///     subtask: "Add a test.".to_string(),
///     dev_loop: DevLoop::Inner,
///     requested_effect: EffectClass::Workspace,
/// };
/// let outcome = AuditOutcome {
///     verdict: SpawnVerdict::Allow,
///     record: AuditRecord { model: "rule-auditor".to_string(), prompt_hash: "abc".to_string(), rationale: "allowed".to_string() },
/// };
/// log.append(&request, &outcome).unwrap();
/// let entries = log.entries().unwrap();
/// assert_eq!(entries.len(), 1);
/// assert_eq!(entries[0].request, request);
/// assert!(entries[0].verdict.is_allow());
///
/// let fresh_handle_on_the_same_path = AuditLog::file(dir.path().join("audit.jsonl"));
/// assert_eq!(fresh_handle_on_the_same_path.entries().unwrap().len(), 1);
/// ```
#[derive(Clone)]
pub struct AuditLog {
    backing: Backing,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.backing {
            Backing::Memory(entries) => write!(
                f,
                "AuditLog::in_memory({} entries)",
                entries.lock().unwrap_or_else(|p| p.into_inner()).len()
            ),
            Backing::File(path) => write!(f, "AuditLog::file({})", path.display()),
        }
    }
}

impl AuditLog {
    /// A log that keeps entries in memory; used by tests.
    pub fn in_memory() -> Self {
        Self {
            backing: Backing::Memory(Arc::new(Mutex::new(Vec::new()))),
        }
    }

    /// A log that appends one JSON line per entry to `path`, creating
    /// parent directories as needed.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            backing: Backing::File(path.into()),
        }
    }

    /// Append `outcome` for `request`.
    pub fn append(&self, request: &SpawnRequest, outcome: &AuditOutcome) -> Result<(), AuditError> {
        let entry = AuditLogEntry::from_outcome(request.clone(), outcome);
        match &self.backing {
            Backing::Memory(entries) => {
                entries
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(entry);
                Ok(())
            }
            Backing::File(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let line = serde_json::to_string(&entry)?;
                let mut file = OpenOptions::new().create(true).append(true).open(path)?;
                writeln!(file, "{line}")?;
                Ok(())
            }
        }
    }

    /// Every entry logged so far, in append order. A file backing that has
    /// not been written to yet is an empty log, not an error.
    pub fn entries(&self) -> Result<Vec<AuditLogEntry>, AuditError> {
        match &self.backing {
            Backing::Memory(entries) => {
                Ok(entries.lock().unwrap_or_else(|p| p.into_inner()).clone())
            }
            Backing::File(path) => {
                if !path.exists() {
                    return Ok(Vec::new());
                }
                let content = std::fs::read_to_string(path)?;
                content
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| serde_json::from_str(line).map_err(AuditError::from))
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::request::tests::request;
    use crate::effects::EffectClass;
    use crate::identity::DevLoop;

    fn allow_outcome() -> AuditOutcome {
        AuditOutcome {
            verdict: SpawnVerdict::Allow,
            record: AuditRecord {
                model: "rule-auditor".to_string(),
                prompt_hash: "h".to_string(),
                rationale: "allowed".to_string(),
            },
        }
    }

    #[test]
    fn in_memory_log_round_trips_multiple_entries_in_order() {
        let log = AuditLog::in_memory();
        assert_eq!(format!("{log:?}"), "AuditLog::in_memory(0 entries)");
        let r1 = request(
            "rust-implementer",
            "a",
            DevLoop::Inner,
            EffectClass::Workspace,
        );
        let r2 = request("deployer", "b", DevLoop::Outer, EffectClass::Sandbox);
        log.append(&r1, &allow_outcome()).unwrap();
        log.append(&r2, &allow_outcome()).unwrap();
        assert_eq!(format!("{log:?}"), "AuditLog::in_memory(2 entries)");
        let entries = log.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].request, r1);
        assert_eq!(entries[1].request, r2);
    }

    #[test]
    fn file_log_appends_json_lines_and_survives_a_new_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("audit.jsonl");
        let log = AuditLog::file(&path);
        assert_eq!(
            format!("{log:?}"),
            format!("AuditLog::file({})", path.display())
        );
        let request = request(
            "rust-implementer",
            "a",
            DevLoop::Inner,
            EffectClass::Workspace,
        );
        log.append(&request, &allow_outcome()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 1);
        let reopened = AuditLog::file(&path);
        let entries = reopened.entries().unwrap();
        assert_eq!(
            entries,
            vec![AuditLogEntry::from_outcome(request, &allow_outcome())]
        );
    }

    #[test]
    fn file_log_with_no_entries_yet_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::file(dir.path().join("missing.jsonl"));
        assert_eq!(log.entries().unwrap(), Vec::new());
    }

    #[test]
    fn file_log_skips_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        std::fs::write(&path, "\n\n").unwrap();
        let log = AuditLog::file(&path);
        assert_eq!(log.entries().unwrap(), Vec::new());
    }
}
