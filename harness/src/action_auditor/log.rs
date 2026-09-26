//! Append-only record of every reviewed action.
//!
//! A sibling of [`crate::auditor::AuditLog`] rather than a reuse of it:
//! that log is typed to [`SpawnRequest`](crate::auditor::SpawnRequest)/
//! [`SpawnVerdict`](crate::auditor::SpawnVerdict), and an [`ActionReview`]/
//! [`ActionVerdict`] pair is a different record shape (a tool call, not a
//! spawn), so it gets its own log type with the same append-only contract.

use super::{ActionAuditError, ActionReview, ActionVerdict};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// One logged decision: the action that was reviewed and what the auditor
/// decided.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionAuditLogEntry {
    /// The action that was reviewed.
    pub review: ActionReview,
    /// What the auditor decided.
    pub verdict: ActionVerdict,
}

#[derive(Clone)]
enum Backing {
    Memory(Arc<Mutex<Vec<ActionAuditLogEntry>>>),
    File(PathBuf),
}

/// Append-only audit trail: one entry per reviewed action.
///
/// ```
/// use harness::action_auditor::{ActionAuditLog, ActionReview, ActionVerdict};
/// use harness::effects::EffectClass;
/// use harness::task::TaskId;
///
/// let dir = tempfile::tempdir().unwrap();
/// let log = ActionAuditLog::file(dir.path().join("action_audit.jsonl"));
/// let review = ActionReview {
///     identity: "rust-implementer".to_string(),
///     task_id: TaskId("t1".to_string()),
///     tool: "github_pr_status".to_string(),
///     args: serde_json::json!({}),
///     effect_class: EffectClass::Repository,
///     prior_actions: vec![],
/// };
/// log.append(&review, &ActionVerdict::Allow).unwrap();
/// let entries = log.entries().unwrap();
/// assert_eq!(entries.len(), 1);
/// assert!(entries[0].verdict.is_allow());
///
/// let reopened = ActionAuditLog::file(dir.path().join("action_audit.jsonl"));
/// assert_eq!(reopened.entries().unwrap().len(), 1);
/// ```
#[derive(Clone)]
pub struct ActionAuditLog {
    backing: Backing,
}

impl std::fmt::Debug for ActionAuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.backing {
            Backing::Memory(entries) => write!(
                f,
                "ActionAuditLog::in_memory({} entries)",
                entries.lock().unwrap_or_else(|p| p.into_inner()).len()
            ),
            Backing::File(path) => write!(f, "ActionAuditLog::file({})", path.display()),
        }
    }
}

impl ActionAuditLog {
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

    /// Append `verdict` for `review`.
    pub fn append(
        &self,
        review: &ActionReview,
        verdict: &ActionVerdict,
    ) -> Result<(), ActionAuditError> {
        let entry = ActionAuditLogEntry {
            review: review.clone(),
            verdict: verdict.clone(),
        };
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
    pub fn entries(&self) -> Result<Vec<ActionAuditLogEntry>, ActionAuditError> {
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
                    .map(|line| serde_json::from_str(line).map_err(ActionAuditError::from))
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectClass;
    use crate::task::TaskId;

    fn review() -> ActionReview {
        ActionReview {
            identity: "rust-implementer".to_string(),
            task_id: TaskId("t1".to_string()),
            tool: "github_pr_status".to_string(),
            args: serde_json::json!({}),
            effect_class: EffectClass::Repository,
            prior_actions: vec![],
        }
    }

    #[test]
    fn in_memory_log_round_trips_multiple_entries_in_order() {
        let log = ActionAuditLog::in_memory();
        assert_eq!(format!("{log:?}"), "ActionAuditLog::in_memory(0 entries)");
        let r1 = review();
        let mut r2 = review();
        r2.tool = "ci_trigger".to_string();
        log.append(&r1, &ActionVerdict::Allow).unwrap();
        log.append(&r2, &ActionVerdict::block(vec![])).unwrap();
        assert_eq!(format!("{log:?}"), "ActionAuditLog::in_memory(2 entries)");
        let entries = log.entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].review, r1);
        assert_eq!(entries[1].review, r2);
    }

    #[test]
    fn file_log_appends_json_lines_and_survives_a_new_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("audit.jsonl");
        let log = ActionAuditLog::file(&path);
        assert_eq!(
            format!("{log:?}"),
            format!("ActionAuditLog::file({})", path.display())
        );
        let r = review();
        log.append(&r, &ActionVerdict::Allow).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 1);
        let reopened = ActionAuditLog::file(&path);
        assert_eq!(
            reopened.entries().unwrap(),
            vec![ActionAuditLogEntry {
                review: r,
                verdict: ActionVerdict::Allow
            }]
        );
    }

    #[test]
    fn file_log_with_no_entries_yet_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log = ActionAuditLog::file(dir.path().join("missing.jsonl"));
        assert_eq!(log.entries().unwrap(), Vec::new());
    }

    #[test]
    fn file_log_skips_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        std::fs::write(&path, "\n\n").unwrap();
        let log = ActionAuditLog::file(&path);
        assert_eq!(log.entries().unwrap(), Vec::new());
    }
}
